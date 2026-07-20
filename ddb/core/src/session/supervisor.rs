use std::sync::Arc;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use futures::future::join_all;
use tokio::{
    sync::{mpsc, oneshot, Mutex},
    task::{JoinHandle, JoinSet},
};
use tracing::{debug, error, trace};

use super::{
    activation::SessionActivation,
    lifecycle::{self, SessionLifecycleHandle, SessionTermination},
    SessionProcess,
};
use crate::{
    cmd_flow::router::Router,
    common::Config,
    group_operation::GroupOperationCoordinator,
    notification::{Notification, NotificationManager, NotificationPayload},
    plugin::FrameworkPlugin,
    runtime_model::RuntimeModel,
    shutdown::{get_shutdown_ctrl, ShutdownCause},
    source::resolver::SourceResolver,
};

type ManagedSessionRef = Arc<Mutex<ManagedSession>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionPhase {
    Starting,
    Active,
}

struct ManagedSession {
    phase: SessionPhase,
    process: SessionProcess,
}

/// Sole owner of debugger process admission, activation, and termination.
pub(crate) struct SessionSupervisor {
    sessions: DashMap<u64, ManagedSessionRef>,
    activation: SessionActivation,
    source_resolver: Arc<SourceResolver>,
    lifecycle: SessionLifecycleHandle,
    lifecycle_events: Mutex<Option<mpsc::UnboundedReceiver<SessionTermination>>>,
    lifecycle_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    lifecycle_task: Mutex<Option<JoinHandle<()>>>,
    transitions: Mutex<()>,
    notifications: Arc<NotificationManager>,
    auto_shutdown: bool,
}

impl SessionSupervisor {
    pub(crate) fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        model: Arc<RuntimeModel>,
        router: Arc<Router>,
        notifications: Arc<NotificationManager>,
        group_operations: Arc<GroupOperationCoordinator>,
        source_resolver: Arc<SourceResolver>,
    ) -> Arc<Self> {
        let (lifecycle, lifecycle_events) = lifecycle::channel();
        Arc::new(Self {
            sessions: DashMap::new(),
            activation: SessionActivation::new(
                Arc::clone(&config),
                plugin,
                model,
                router,
                Arc::clone(&notifications),
                group_operations,
                Arc::clone(&source_resolver),
            ),
            source_resolver,
            lifecycle,
            lifecycle_events: Mutex::new(Some(lifecycle_events)),
            lifecycle_shutdown: Mutex::new(None),
            lifecycle_task: Mutex::new(None),
            transitions: Mutex::new(()),
            notifications,
            auto_shutdown: config.conf.auto_shutdown,
        })
    }

    pub(crate) async fn start(self: &Arc<Self>) -> Result<()> {
        let mut events = self.lifecycle_events.lock().await;
        let mut events = events
            .take()
            .ok_or_else(|| anyhow!("session supervisor is already started"))?;
        let (shutdown, mut shutdown_requested) = oneshot::channel();
        self.lifecycle_shutdown.lock().await.replace(shutdown);
        let supervisor = Arc::downgrade(self);

        let task = tokio::spawn(async move {
            let mut cleanups = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_requested => break,
                    termination = events.recv() => {
                        let Some(termination) = termination else {
                            break;
                        };
                        let Some(supervisor) = supervisor.upgrade() else {
                            break;
                        };
                        cleanups.spawn(async move {
                            supervisor.finish_session(termination).await;
                        });
                    }
                    Some(result) = cleanups.join_next(), if !cleanups.is_empty() => {
                        if let Err(error) = result {
                            error!(?error, "session cleanup task failed");
                        }
                    }
                }
            }

            while let Some(result) = cleanups.join_next().await {
                if let Err(error) = result {
                    error!(?error, "session cleanup task failed");
                }
            }
        });
        self.lifecycle_task.lock().await.replace(task);
        Ok(())
    }

    pub(crate) async fn admit(&self, process: SessionProcess) -> Result<u64> {
        let sid = process.sid();
        let termination = self.lifecycle.bind(sid);
        let session = Arc::new(Mutex::new(ManagedSession {
            phase: SessionPhase::Starting,
            process,
        }));

        {
            let _transition = self.transitions.lock().await;
            self.sessions.insert(sid, Arc::clone(&session));
        }

        let start_result = {
            let mut managed = session.lock().await;
            self.activation
                .activate(&mut managed.process, termination.clone())
                .await
        };

        if let Err(error) = start_result {
            self.rollback_if_owned(sid, "failed to roll back session startup")
                .await;
            return Err(error);
        }

        let activated = {
            let _transition = self.transitions.lock().await;
            if termination.termination_requested() || !self.sessions.contains_key(&sid) {
                false
            } else {
                session.lock().await.phase = SessionPhase::Active;
                self.notify_session_list_changed().await;
                true
            }
        };

        if !activated {
            self.rollback_if_owned(sid, "failed to roll back terminated session")
                .await;
            return Err(anyhow!(
                "session {} terminated before activation completed",
                sid
            ));
        }

        debug!(sid, "session activated successfully");
        Ok(sid)
    }

    async fn rollback_if_owned(&self, sid: u64, context: &str) {
        let session = {
            let _transition = self.transitions.lock().await;
            self.sessions.remove(&sid).map(|(_, session)| session)
        };
        if let Some(session) = session {
            if let Err(error) = self
                .activation
                .deactivate(&mut session.lock().await.process)
                .await
            {
                error!(sid, ?error, context);
            }
        }
    }

    async fn finish_session(&self, termination: SessionTermination) {
        debug!(
            sid = termination.sid,
            cause = ?termination.cause,
            "session termination requested"
        );
        self.remove_session(termination.sid).await;
    }

    async fn remove_session(&self, sid: u64) {
        let session = {
            let _transition = self.transitions.lock().await;
            self.sessions.remove(&sid).map(|(_, session)| session)
        };
        let Some(session) = session else {
            trace!(sid, "ignoring termination for an unregistered session");
            return;
        };

        let was_active = {
            let mut managed = session.lock().await;
            let was_active = managed.phase == SessionPhase::Active;
            if let Err(error) = self.activation.deactivate(&mut managed.process).await {
                error!(sid, ?error, "session cleanup failed");
            }
            was_active
        };

        let _transition = self.transitions.lock().await;
        if was_active {
            self.notify_session_list_changed().await;
        }
        if self.auto_shutdown && self.sessions.is_empty() {
            debug!("No more sessions. Possibly shutting down...");
            get_shutdown_ctrl().trigger_once(ShutdownCause::NoSessions);
        }
    }

    async fn notify_session_list_changed(&self) {
        self.notifications
            .broadcast(Notification::new(NotificationPayload::SessionListChanged))
            .await;
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(shutdown) = self.lifecycle_shutdown.lock().await.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.lifecycle_task.lock().await.take() {
            let _ = task.await;
        }

        let sessions = {
            let _transition = self.transitions.lock().await;
            let keys = self
                .sessions
                .iter()
                .map(|entry| *entry.key())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|sid| {
                    self.sessions
                        .remove(&sid)
                        .map(|(_, session)| (sid, session))
                })
                .collect::<Vec<_>>()
        };

        join_all(sessions.into_iter().map(|(sid, session)| async move {
            if let Err(error) = self
                .activation
                .deactivate(&mut session.lock().await.process)
                .await
            {
                error!(
                    sid,
                    ?error,
                    "session cleanup failed during supervisor shutdown"
                );
            }
        }))
        .await;
        self.source_resolver.shutdown().await;
    }
}
