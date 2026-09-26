use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{api::read_model::ThreadView, cmd_flow::router::Target, state::GlobalThreadId};

use super::{service::StopFrameKey, ApplicationCommandPort, ApplicationError};

pub(super) const MAX_VARIABLE_OBJECTS: usize = 1_024;

/// Owns a backend root and its descendants. In-flight readers keep the root alive
/// even after its stop expires; the final owner schedules backend cleanup.
pub(super) struct VariableObject {
    pub(super) name: String,
    pub(super) frame: StopFrameKey,
    command_port: Arc<dyn ApplicationCommandPort>,
    _permit: OwnedSemaphorePermit,
    creation_started: AtomicBool,
}

impl VariableObject {
    pub(super) fn mark_creation_started(&self) {
        self.creation_started.store(true, Ordering::Relaxed);
    }
}

impl Drop for VariableObject {
    fn drop(&mut self) {
        if !self.creation_started.load(Ordering::Relaxed) {
            return;
        }
        let command_port = Arc::clone(&self.command_port);
        let command = format!("-var-delete {}", self.name);
        let target = Target::Thread(GlobalThreadId::new(self.frame.global_thread_id));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    command_port.execute(&command, target),
                )
                .await;
            });
        }
    }
}

pub(super) struct VariableObjects {
    roots: Mutex<HashMap<String, Arc<VariableObject>>>,
    capacity: Arc<Semaphore>,
}

impl VariableObjects {
    pub(super) fn new() -> Self {
        Self {
            roots: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(MAX_VARIABLE_OBJECTS)),
        }
    }

    pub(super) fn reserve(
        &self,
        frame: StopFrameKey,
        command_port: Arc<dyn ApplicationCommandPort>,
    ) -> Result<Arc<VariableObject>, ApplicationError> {
        let permit = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| {
                ApplicationError::resource_exhausted("stopped variable object limit reached")
            })?;
        Ok(Arc::new(VariableObject {
            name: format!("ddb_api_{}", uuid::Uuid::new_v4().simple()),
            frame,
            command_port,
            _permit: permit,
            creation_started: AtomicBool::new(false),
        }))
    }

    pub(super) fn retain(&self, object: Arc<VariableObject>) {
        self.roots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(object.name.clone(), object);
    }

    pub(super) fn get(&self, name: &str) -> Result<Arc<VariableObject>, ApplicationError> {
        self.roots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned()
            .ok_or_else(|| {
                ApplicationError::new(
                    ddb_api_types::v2::DdbErrorCode::Expired,
                    "variable object is no longer available",
                )
            })
    }

    pub(super) fn remove(&self, name: &str) {
        self.roots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
    }

    pub(super) fn reconcile(&self, threads: &[ThreadView]) {
        self.roots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, object| {
                threads
                    .iter()
                    .find(|thread| thread.global_id == object.frame.global_thread_id)
                    .is_some_and(|thread| {
                        // A snapshot taken before a new stop must not retire its values.
                        thread.execution_revision < object.frame.execution_revision
                            || (thread.status == "stopped"
                                && thread.execution_revision == object.frame.execution_revision)
                    })
            });
    }
}

#[cfg(test)]
mod tests {
    use super::super::CommandPortError;
    use super::*;
    use crate::cmd_flow::CommandOutcome;

    struct CleanupPort(tokio::sync::mpsc::UnboundedSender<String>);

    #[async_trait::async_trait]
    impl ApplicationCommandPort for CleanupPort {
        async fn execute(
            &self,
            command: &str,
            _target: Target,
        ) -> Result<CommandOutcome, CommandPortError> {
            let _ = self.0.send(command.to_string());
            Ok(CommandOutcome::empty())
        }
    }

    #[tokio::test]
    async fn expired_roots_wait_for_readers_and_release_capacity_with_cleanup() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let port: Arc<dyn ApplicationCommandPort> = Arc::new(CleanupPort(sender));
        let objects = VariableObjects {
            roots: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(1)),
        };
        let frame = StopFrameKey {
            internal: "7:2:0".to_string(),
            global_thread_id: 7,
            execution_revision: 2,
            level: 0,
        };
        let object = objects.reserve(frame.clone(), Arc::clone(&port)).unwrap();
        let name = object.name.clone();
        object.mark_creation_started();
        objects.retain(object);
        let reader = objects.get(&name).unwrap();
        assert!(objects.reserve(frame.clone(), Arc::clone(&port)).is_err());

        objects.reconcile(&[]); // The owning thread has been removed.
        assert!(objects.get(&name).is_err());
        assert!(receiver.try_recv().is_err());
        assert!(objects.reserve(frame.clone(), Arc::clone(&port)).is_err());
        drop(reader);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            format!("-var-delete {name}")
        );

        // Rejected or replayed admissions have not created a backend object.
        drop(objects.reserve(frame.clone(), Arc::clone(&port)).unwrap());
        tokio::task::yield_now().await;
        assert!(receiver.try_recv().is_err());

        // A reservation dropped on command failure gets the same cleanup.
        let failed = objects.reserve(frame, port).unwrap();
        let failed_name = failed.name.clone();
        failed.mark_creation_started();
        drop(failed);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            format!("-var-delete {failed_name}")
        );
        assert!(receiver.try_recv().is_err());
    }
}
