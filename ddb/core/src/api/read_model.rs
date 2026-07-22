use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use crate::{
    cmd_flow::router::Router,
    source::resolver::SourceResolver,
    state::{BreakpointSnapshot, GroupMeta, RuntimeModel},
};

#[derive(Clone)]
pub(crate) struct ApiQueries {
    model: Arc<RuntimeModel>,
    router: Arc<Router>,
    source_resolver: Arc<SourceResolver>,
}

impl ApiQueries {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        router: Arc<Router>,
        source_resolver: Arc<SourceResolver>,
    ) -> Arc<Self> {
        Arc::new(Self {
            model,
            router,
            source_resolver,
        })
    }

    #[cfg(test)]
    pub(crate) fn model(&self) -> &Arc<RuntimeModel> {
        &self.model
    }

    pub(crate) async fn sessions(&self) -> Vec<SessionView> {
        let mut sessions = Vec::new();
        for session in self.model.session_snapshots().await {
            let sid = session.sid;
            let tag = session.tag;
            let alias = session
                .service_identity
                .map(|service| service.alias)
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let status = session.status.to_string();
            let group = match self.model.group_info_by_session(sid) {
                Some((id, hash)) => SessionGroupView {
                    valid: true,
                    id: id.value(),
                    hash,
                },
                None => SessionGroupView {
                    valid: false,
                    id: 0,
                    hash: "UNKNOWN".to_string(),
                },
            };
            sessions.push(SessionView {
                sid,
                tag,
                alias,
                status,
                group,
            });
        }
        sessions.sort_unstable_by_key(|session| session.sid);
        sessions
    }

    pub(crate) fn pending_commands(&self) -> Vec<PendingCommandView> {
        self.router
            .runtime_statuses()
            .into_iter()
            .map(|status| PendingCommandView {
                sid: status.sid,
                in_flight: status.in_flight,
                queued: status.queued,
                closed: status.closed,
            })
            .collect()
    }

    pub(crate) fn groups(&self) -> Vec<GroupView> {
        let mut groups = self
            .model
            .groups()
            .iter()
            .map(GroupView::from)
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(|group| group.id);
        groups
    }

    pub(crate) fn group_by_id(&self, id: u64) -> Option<GroupView> {
        self.model
            .group_by_id(id.into())
            .as_ref()
            .map(GroupView::from)
    }

    pub(crate) fn group_by_hash(&self, hash: &str) -> Option<GroupView> {
        self.model.group_by_hash(hash).as_ref().map(GroupView::from)
    }

    pub(crate) async fn group_ids_for_source(&self, source: &str) -> Result<Vec<u64>> {
        let mut group_ids = self
            .source_resolver
            .group_ids_for(source)
            .await?
            .into_iter()
            .map(Into::<u64>::into)
            .collect::<Vec<_>>();
        group_ids.sort_unstable();
        Ok(group_ids)
    }

    pub(crate) async fn groups_for_source(&self, source: &str) -> Result<Vec<GroupView>> {
        let mut groups = self
            .source_resolver
            .groups_for(source)
            .await?
            .iter()
            .map(GroupView::from)
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(|group| group.id);
        Ok(groups)
    }

    pub(crate) fn breakpoints(&self) -> Vec<BreakpointSnapshot> {
        let mut breakpoints = self.model.breakpoint_snapshots();
        breakpoints.sort_unstable_by_key(|breakpoint| breakpoint.id);
        breakpoints
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionView {
    sid: u64,
    tag: String,
    alias: String,
    status: String,
    group: SessionGroupView,
}

#[derive(Debug, Serialize)]
struct SessionGroupView {
    valid: bool,
    id: u64,
    hash: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PendingCommandView {
    sid: u64,
    in_flight: usize,
    queued: usize,
    closed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GroupView {
    id: u64,
    hash: String,
    alias: String,
    sids: Vec<u64>,
}

impl From<&GroupMeta> for GroupView {
    fn from(group: &GroupMeta) -> Self {
        let mut sids = group.session_ids().iter().copied().collect::<Vec<_>>();
        sids.sort_unstable();
        Self {
            id: group.id().value(),
            hash: group.hash().to_string(),
            alias: group.alias().to_string(),
            sids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cmd_flow::api::CommandExecutor,
        source::{
            catalog::SourceCatalog,
            resolver::{SourceResolutionPolicy, SourceResolver},
        },
        state::{BkptLoc, GroupSubBkpt, SubBkptType},
    };

    fn queries(model: Arc<RuntimeModel>) -> Arc<ApiQueries> {
        let router = Arc::new(Router::new(Arc::clone(&model)));
        let resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(&model) as _,
            CommandExecutor::new(Arc::clone(&router)),
            SourceResolutionPolicy::OnDemand,
        );
        ApiQueries::new(model, router, resolver)
    }

    #[tokio::test]
    async fn returns_api_owned_snapshots_with_stable_wire_shapes() {
        let model = RuntimeModel::new();
        model.register_session(7, "worker-7", None).await;
        let identity = crate::state::ServiceIdentity::new("binary-worker", "worker");
        drop(model.register_service_group(7, &identity).await);
        let group_id = model.group_id_by_session(7).unwrap();
        let breakpoint_id = model.add_breakpoint(BkptLoc::new("src/worker.rs", 42));
        model.add_sub_breakpoint(
            breakpoint_id,
            SubBkptType::Group(GroupSubBkpt::new(group_id)),
        );

        let queries = queries(model);
        let groups = serde_json::to_value(queries.groups()).unwrap();
        let sessions = serde_json::to_value(queries.sessions().await).unwrap();
        let breakpoints = serde_json::to_value(queries.breakpoints()).unwrap();

        assert_eq!(groups[0]["hash"], "binary-worker");
        assert_eq!(groups[0]["sids"][0], 7);
        assert_eq!(sessions[0]["group"]["id"], group_id.value());
        assert_eq!(breakpoints[0]["location"]["src"], "src/worker.rs");
        assert_eq!(breakpoints[0]["subbkpts"][0]["type"], "group");
    }
}
