use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use crate::{
    cmd_flow::router::Router,
    source::resolver::SourceResolver,
    state::RuntimeModel,
    state::{BkptLoc, BkptMeta, GroupMeta, SubBkptMeta, SubBkptType},
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
        for session in self.model.state().sessions() {
            let (sid, tag, alias, status) = session
                .read_with(|meta| {
                    (
                        meta.sid(),
                        meta.tag().to_string(),
                        meta.service_meta()
                            .map(|service| service.alias.clone())
                            .unwrap_or_else(|| "UNKNOWN".to_string()),
                        meta.status().to_string(),
                    )
                })
                .await;
            let group = match self.model.groups().group_info_by_session(sid) {
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
            .groups()
            .iter()
            .map(GroupView::from)
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(|group| group.id);
        groups
    }

    pub(crate) fn group_by_id(&self, id: u64) -> Option<GroupView> {
        self.model
            .groups()
            .group_by_id(id.into())
            .as_ref()
            .map(GroupView::from)
    }

    pub(crate) fn group_by_hash(&self, hash: &str) -> Option<GroupView> {
        self.model
            .groups()
            .group_by_hash(hash)
            .as_ref()
            .map(GroupView::from)
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

    pub(crate) fn breakpoints(&self) -> Vec<BreakpointView> {
        let mut breakpoints = self
            .model
            .breakpoints()
            .breakpoints()
            .iter()
            .map(BreakpointView::from)
            .collect::<Vec<_>>();
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BreakpointLocationView {
    src: String,
    line: u64,
}

impl From<&BkptLoc> for BreakpointLocationView {
    fn from(location: &BkptLoc) -> Self {
        Self {
            src: location.path().to_string(),
            line: location.line(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum SubBreakpointView {
    #[serde(rename = "session")]
    Session {
        id: u64,
        target_session: u64,
        local_breakpoint_id: u64,
    },
    #[serde(rename = "group")]
    Group {
        id: u64,
        target_group: u64,
        active_sessions: usize,
    },
}

impl From<&SubBkptMeta> for SubBreakpointView {
    fn from(sub_breakpoint: &SubBkptMeta) -> Self {
        match sub_breakpoint.kind() {
            SubBkptType::Session(session) => Self::Session {
                id: sub_breakpoint.id(),
                target_session: session.target_session(),
                local_breakpoint_id: session.local_id(),
            },
            SubBkptType::Group(group) => Self::Group {
                id: sub_breakpoint.id(),
                target_group: group.target_group().value(),
                active_sessions: group.local_ids().len(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BreakpointView {
    id: u64,
    location: BreakpointLocationView,
    enabled: bool,
    times: u64,
    subbkpts: Vec<SubBreakpointView>,
}

impl From<&BkptMeta> for BreakpointView {
    fn from(breakpoint: &BkptMeta) -> Self {
        Self {
            id: breakpoint.id(),
            location: breakpoint.location().into(),
            enabled: breakpoint.is_enabled(),
            times: breakpoint.times(),
            subbkpts: breakpoint
                .sub_breakpoints()
                .iter()
                .map(SubBreakpointView::from)
                .collect(),
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
            Arc::clone(model.groups()),
            CommandExecutor::new(Arc::clone(&router)),
            SourceResolutionPolicy::OnDemand,
        );
        ApiQueries::new(model, router, resolver)
    }

    #[tokio::test]
    async fn returns_api_owned_snapshots_with_stable_wire_shapes() {
        let model = RuntimeModel::new();
        model.state().register_session(7, "worker-7", None).await;
        model
            .groups()
            .register_session("binary-worker", "worker".to_string(), 7);
        let group_id = model.groups().group_id_by_session(7).unwrap();
        let breakpoint_id = model
            .breakpoints()
            .add_breakpoint(BkptLoc::new("src/worker.rs", 42));
        model.breakpoints().add_sub_breakpoint(
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
