use anyhow::{bail, Context, Result};

use super::{lifecycle::SessionTerminationReporter, SessionProcess};
use crate::{
    cmd_flow::{
        breakpoint::{publish_breakpoint_state_change, publish_breakpoint_state_changes},
        decoder::BreakpointCreated,
        get_router,
        session_runtime::{CompletionConsistency, SessionCommand, SessionHandle},
    },
    common::Config,
    notification::get_notif_mgr,
    plugin::get_framework_plugin,
    state::{get_bkpt_mgr, get_group_mgr, get_proclet_mgr, get_state_mgr, STATES},
};

/// Applies and removes every application projection associated with a process.
pub(crate) struct SessionActivation {
    config: &'static Config,
}

impl SessionActivation {
    pub(crate) fn new(config: &'static Config) -> Self {
        Self { config }
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
        let service_meta = request.service_meta.clone();
        let caladan_ip = request.caladan_ip;

        STATES
            .register_session(sid, &tag, service_meta.clone())
            .await;

        let handle = process.launch(termination.clone()).await?;

        if let Some(meta) = &service_meta {
            get_group_mgr().register_session(&meta.hash, meta.alias.clone(), sid);
        }

        self.sync_group_breakpoints(sid, &handle).await?;
        process.finish_bootstrap().await?;

        if termination.termination_requested() {
            bail!("session {} terminated before activation completed", sid);
        }

        get_router().add_session(handle);

        if get_framework_plugin().should_register_caladan_ip(self.config) {
            if let Some(caladan_ip) = caladan_ip {
                get_proclet_mgr().register_owner_session(caladan_ip, sid);
            }
        }

        #[cfg(not(feature = "lazy_source_map"))]
        {
            tokio::spawn(async move {
                if let Err(error) = crate::state::get_source_mgr().resolve_src_for(sid).await {
                    tracing::debug!(sid, ?error, "failed to resolve session source files");
                }
            });
        }

        STATES.update_session_status_on(sid).await;
        Ok(())
    }

    async fn sync_group_breakpoints(&self, sid: u64, handle: &SessionHandle) -> Result<()> {
        let Some(group_id) = get_group_mgr().group_id_by_session(sid) else {
            return Ok(());
        };

        let notifications = get_notif_mgr();
        for breakpoint in get_bkpt_mgr().group_breakpoints(group_id) {
            let path = breakpoint.location().breakpoint_path();
            let response = handle
                .execute(SessionCommand {
                    token: crate::common::counter::next_token(),
                    command: format!("-break-insert {}", path),
                    thread_id: None,
                    consistency: CompletionConsistency::StateConsistent,
                })
                .await
                .with_context(|| format!("Failed to insert existing breakpoint at {}", path))?;
            let local_id = BreakpointCreated::decode(&response)?.local_id;
            let breakpoints = get_bkpt_mgr();
            let change = breakpoints.attach_group_breakpoint_session_target(
                breakpoint.id(),
                group_id,
                sid,
                local_id,
            );
            publish_breakpoint_state_change(
                breakpoints,
                notifications.as_ref(),
                change,
                "setting up group breakpoint for new session",
            )
            .await;
        }
        Ok(())
    }

    pub(crate) async fn deactivate(&self, process: &mut SessionProcess) -> Result<()> {
        let sid = process.sid();

        get_router().remove_session(sid);
        get_state_mgr().update_session_status_off(sid).await;

        let groups = get_group_mgr();
        let group_id = groups.group_id_by_session(sid);
        let breakpoints = get_bkpt_mgr();
        let changes = breakpoints.clean_bkpts_for_terminated_session(sid, group_id);
        let notifications = get_notif_mgr();
        publish_breakpoint_state_changes(
            breakpoints,
            notifications.as_ref(),
            changes,
            "cleaning breakpoints for terminated session",
        )
        .await;

        groups.remove_session(sid);
        get_proclet_mgr().remove_owner_session(sid);
        get_state_mgr().remove_session(sid).await;
        process.shutdown().await
    }
}
