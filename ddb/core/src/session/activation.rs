use std::sync::Arc;

use anyhow::{bail, Context, Result};

use super::{lifecycle::SessionTerminationReporter, SessionProcess};
use crate::{
    cmd_flow::{
        breakpoint::BreakpointEventPublisher,
        decoder::BreakpointCreated,
        router::Router,
        session_runtime::{CompletionConsistency, SessionCommand, SessionHandle},
    },
    common::Config,
    plugin::FrameworkPlugin,
    source::resolver::SourceResolver,
    state::RuntimeModel,
};

/// Applies and removes every application projection associated with a process.
pub(crate) struct SessionActivation {
    config: Arc<Config>,
    plugin: Arc<dyn FrameworkPlugin>,
    model: Arc<RuntimeModel>,
    router: Arc<Router>,
    breakpoint_events: Arc<BreakpointEventPublisher>,
    source_resolver: Arc<SourceResolver>,
}

impl SessionActivation {
    pub(crate) fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        model: Arc<RuntimeModel>,
        router: Arc<Router>,
        breakpoint_events: Arc<BreakpointEventPublisher>,
        source_resolver: Arc<SourceResolver>,
    ) -> Self {
        Self {
            config,
            plugin,
            model,
            router,
            breakpoint_events,
            source_resolver,
        }
    }

    pub(crate) async fn activate(
        &self,
        process: &mut SessionProcess,
        termination: SessionTerminationReporter,
    ) -> Result<()> {
        let sid = process.sid();
        let request = process.request();
        let tag = request
            .tag
            .clone()
            .unwrap_or_else(|| format!("session-{}", sid));
        let service_identity = request.service_identity.clone();
        let caladan_ip = request.caladan_ip;

        self.model
            .register_session(sid, &tag, service_identity.clone())
            .await;

        let handle = process.launch(termination.clone()).await?;

        let group_operation = match service_identity.as_ref() {
            Some(identity) => Some(self.model.register_service_group(sid, identity).await),
            None => None,
        };

        self.sync_group_breakpoints(sid, &handle).await?;
        process.finish_bootstrap().await?;

        if termination.termination_requested() {
            bail!("session {} terminated before activation completed", sid);
        }

        self.router.add_session(handle);
        drop(group_operation);
        self.source_resolver.session_activated(sid);

        let proclet_owner = self
            .plugin
            .should_register_caladan_ip(self.config.as_ref())
            .then_some(caladan_ip)
            .flatten();
        self.model
            .complete_session_activation(sid, proclet_owner)
            .await;
        Ok(())
    }

    async fn sync_group_breakpoints(&self, sid: u64, handle: &SessionHandle) -> Result<()> {
        let Some(group_id) = self.model.group_id_by_session(sid) else {
            return Ok(());
        };

        for breakpoint in self.model.group_breakpoints(group_id) {
            let path = breakpoint.location().breakpoint_path();
            let response = handle
                .execute(SessionCommand {
                    command: format!("-break-insert {}", path),
                    thread_id: None,
                    consistency: CompletionConsistency::StateConsistent,
                })
                .await
                .with_context(|| format!("Failed to insert existing breakpoint at {}", path))?;
            let local_id = BreakpointCreated::decode(&response)?.local_id;
            let change = self.model.attach_group_breakpoint_session_target(
                breakpoint.id(),
                group_id,
                sid,
                local_id,
            );
            self.breakpoint_events.publish_state_change(change).await;
        }
        Ok(())
    }

    pub(crate) async fn deactivate(&self, process: &mut SessionProcess) -> Result<()> {
        let sid = process.sid();

        self.source_resolver.cancel_session(sid).await;
        let retirement = self.model.begin_session_retirement(sid).await;
        self.router.remove_session(sid);
        let mut retirement = retirement.finish().await;
        self.breakpoint_events
            .publish_state_changes(std::mem::take(&mut retirement.breakpoint_changes))
            .await;

        if let Some(group_id) = retirement.emptied_group {
            self.source_resolver.remove_group(group_id).await;
        }
        drop(retirement);
        process.shutdown().await
    }
}
