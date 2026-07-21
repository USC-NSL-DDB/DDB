use std::sync::Arc;

use crate::state::{BreakpointMgr, BreakpointStateChange, GroupId, GroupMgr, ProcletMgr, StateMgr};

/// Owns the mutable debugger model shared by application services.
///
/// Each repository keeps its specialized synchronization strategy. The model is
/// only a dependency boundary; it does not serialize independent sessions.
///
/// # Lock hierarchy
///
/// Callers that need more than one lock must acquire in this order:
/// group operation gate (tokio) -> session meta (tokio) -> the std locks of
/// the individual managers (thread indexes, breakpoints, groups, sources).
/// The std locks are leaves: they are never held across an `.await`.
pub struct RuntimeModel {
    state: Arc<StateMgr>,
    groups: Arc<GroupMgr>,
    breakpoints: Arc<BreakpointMgr>,
    proclets: Arc<ProcletMgr>,
}

impl RuntimeModel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(StateMgr::new()),
            groups: Arc::new(GroupMgr::new()),
            breakpoints: Arc::new(BreakpointMgr::new()),
            proclets: Arc::new(ProcletMgr::new()),
        })
    }

    pub fn state(&self) -> &Arc<StateMgr> {
        &self.state
    }

    pub fn groups(&self) -> &Arc<GroupMgr> {
        &self.groups
    }

    pub fn breakpoints(&self) -> &Arc<BreakpointMgr> {
        &self.breakpoints
    }

    pub fn proclets(&self) -> &Arc<ProcletMgr> {
        &self.proclets
    }

    /// Removes every trace of a terminated session across the aggregates.
    ///
    /// The group id is read before the group index is mutated and the
    /// breakpoint cleanup uses that id; encapsulating the sequence here makes
    /// the ordering impossible to break from call sites. Callers must hold the
    /// session's group operation gate.
    pub async fn retire_session(&self, sid: u64) -> SessionRetirement {
        let group_id = self.groups.group_id_by_session(sid);
        self.state.update_session_status_off(sid).await;
        let breakpoint_changes = self
            .breakpoints
            .clean_bkpts_for_terminated_session(sid, group_id);
        self.groups.remove_session(sid);
        let emptied_group =
            group_id.filter(|group_id| self.groups.group_by_id(*group_id).is_none());
        self.proclets.remove_owner_session(sid);
        self.state.remove_session(sid).await;
        SessionRetirement {
            breakpoint_changes,
            emptied_group,
        }
    }
}

/// Effects of retiring a session that the application layer must publish or
/// tear down outside the model.
pub struct SessionRetirement {
    pub breakpoint_changes: Vec<BreakpointStateChange>,
    /// The session's group, when this removal emptied it.
    pub emptied_group: Option<GroupId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BkptLoc, GroupSubBkpt, SubBkptType};

    async fn model_with_group_breakpoint(sid: u64) -> (Arc<RuntimeModel>, GroupId, u64) {
        let model = RuntimeModel::new();
        model.state().register_session(sid, "svc", None).await;
        model
            .groups()
            .register_session("hash-a", "api".to_string(), sid);
        let group_id = model.groups().group_id_by_session(sid).unwrap();
        let breakpoint_id = model
            .breakpoints()
            .add_breakpoint(BkptLoc::new("main.rs", 7));
        let mut group_breakpoint = GroupSubBkpt::new(group_id);
        group_breakpoint.add_local_bkpt(sid, 1);
        model
            .breakpoints()
            .add_sub_breakpoint(breakpoint_id, SubBkptType::Group(group_breakpoint));
        model.proclets().register_owner_session(42, sid);
        (model, group_id, breakpoint_id)
    }

    #[tokio::test]
    async fn retiring_the_last_session_reports_the_emptied_group() {
        let (model, group_id, breakpoint_id) = model_with_group_breakpoint(7).await;

        let retirement = model.retire_session(7).await;

        assert_eq!(retirement.emptied_group, Some(group_id));
        assert!(!retirement.breakpoint_changes.is_empty());
        assert!(model.groups().group_by_id(group_id).is_none());
        assert!(model.state().session(7).is_none());
        assert_eq!(model.proclets().session_id_for_caladan_ip(42), None);
        assert!(model
            .breakpoints()
            .local_breakpoint_ids(breakpoint_id)
            .is_empty());
    }

    #[tokio::test]
    async fn retiring_one_of_two_group_members_keeps_the_group() {
        let (model, group_id, _) = model_with_group_breakpoint(7).await;
        model.state().register_session(8, "svc-8", None).await;
        model
            .groups()
            .register_session("hash-a", "api".to_string(), 8);

        let retirement = model.retire_session(7).await;

        assert_eq!(retirement.emptied_group, None);
        assert!(model.groups().group_by_id(group_id).is_some());
        assert!(model.state().session(8).is_some());
    }
}
