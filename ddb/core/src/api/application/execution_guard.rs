use super::{ApplicationError, DdbApplicationService, ResolvedTarget};
use ddb_api_types::v2::{DdbErrorCode, ExecutionGuard};
use std::sync::Arc;

use crate::{
    cmd_flow::input::{DispatchGuard, DispatchPermit, StaleExecution},
    state::{GlobalThreadId, RuntimeModel},
};

impl DispatchPermit for crate::state::ExecutionDispatchPermit {
    fn dispatch(
        &mut self,
        validate: &mut dyn FnMut() -> anyhow::Result<()>,
        action: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        validate()?;
        self.admit(action).ok_or(StaleExecution)?
    }
}

pub(super) struct StopDispatchGuard {
    pub model: Arc<RuntimeModel>,
    pub thread: GlobalThreadId,
    pub revision: u64,
}

impl std::fmt::Debug for StopDispatchGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StopDispatchGuard")
            .field("thread", &self.thread)
            .field("revision", &self.revision)
            .finish()
    }
}

#[async_trait::async_trait]
impl DispatchGuard for StopDispatchGuard {
    async fn prepare(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        self.model
            .prepare_stop_for_dispatch(self.thread, self.revision)
            .await
            .map(|permit| Box::new(permit) as Box<dyn DispatchPermit>)
            .ok_or_else(|| StaleExecution.into())
    }
    async fn check(&self) -> anyhow::Result<()> {
        if !self
            .model
            .claim_thread_stop(self.thread, self.revision)
            .await
        {
            return Err(StaleExecution.into());
        }
        Ok(())
    }
}

impl DdbApplicationService {
    pub(super) async fn execution_guard(
        &self,
        guard: Option<&ExecutionGuard>,
        resolved: &ResolvedTarget,
    ) -> Result<Option<Arc<dyn DispatchGuard>>, ApplicationError> {
        let Some(guard) = guard else { return Ok(None) };
        if guard.server_instance_id != self.server_instance_id() {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "execution guard belongs to a different backend instance",
            ));
        }
        let crate::cmd_flow::router::Target::Thread(thread) = resolved.command else {
            return Err(ApplicationError::invalid(
                "execution_guard",
                "requires one explicit thread target",
            ));
        };
        let threads = self
            .queries
            .threads_for_sessions(&resolved.session_ids)
            .await
            .into_iter()
            .filter(|view| view.global_id == thread.value())
            .collect::<Vec<_>>();
        let current = self
            .projection()
            .execution_state(resolved.public.clone(), &threads)?;
        let Some((_, current)) = current else {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "execution stop no longer exists",
            ));
        };
        if current.running
            || current.execution_state_id != guard.execution_state_id
            || current.revision != guard.revision
            || threads[0].status != "stopped"
        {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "execution stop was superseded",
            ));
        }
        Ok(Some(Arc::new(StopDispatchGuard {
            model: Arc::clone(self.queries.model()),
            thread,
            revision: threads[0].stop_revision,
        })))
    }
}
