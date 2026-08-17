use std::{sync::Arc, time::SystemTime};

use anyhow::Result;
use ddb_api_extension::{ExtensionRegistry, ExtensionSchema, ExtensionStateSnapshot};
use ddb_api_types::v2::{extension_payload, ExtensionDescriptor, ExtensionPresentationKind};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::warn;

use crate::{
    cmd_flow::{
        router::Router,
        session_runtime::{PendingCommandChange, SessionPendingCommand},
    },
    common::Config,
    plugin::FrameworkPlugin,
    source::resolver::SourceResolver,
    state::{
        BreakpointSnapshot, GlobalThreadId, GroupMeta, RuntimeChange, RuntimeModel, SessionStatus,
        ThreadStatus,
    },
};

#[derive(Clone)]
pub(crate) struct ApiQueries {
    model: Arc<RuntimeModel>,
    router: Arc<Router>,
    source_resolver: Arc<SourceResolver>,
    extensions: Arc<ExtensionRegistry>,
}

impl ApiQueries {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        router: Arc<Router>,
        source_resolver: Arc<SourceResolver>,
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
    ) -> Result<Arc<Self>> {
        let providers = plugin.api_extensions(config.as_ref(), Arc::clone(&model));
        let extensions = Arc::new(ExtensionRegistry::new(providers, 1024 * 1024)?);
        Ok(Arc::new(Self {
            model,
            router,
            source_resolver,
            extensions,
        }))
    }

    pub(crate) fn subscribe_runtime_changes(&self) -> broadcast::Receiver<RuntimeChange> {
        self.model.subscribe_changes()
    }

    pub(crate) fn subscribe_pending_changes(&self) -> broadcast::Receiver<PendingCommandChange> {
        self.router.subscribe_pending_changes()
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
            let starting = session.status == SessionStatus::STARTING;
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
                starting,
                group,
                selected_thread_id: session.current_context.map(|context| context.tid.value()),
                in_custom_context: session.in_custom_context,
                all_threads_stopped: session.all_threads_stopped,
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

    pub(crate) fn pending_command_details(&self) -> Vec<ApiPendingCommandView> {
        self.router
            .pending_commands()
            .into_iter()
            .map(ApiPendingCommandView::from)
            .collect()
    }
    /// Returns selected public-routing identities without exposing repositories.
    pub(crate) fn selection_ids(&self) -> (Option<u64>, Option<u64>) {
        (
            self.model.current_session_id(),
            self.model.current_thread_id().map(GlobalThreadId::value),
        )
    }

    /// Resolves a global thread identity to its owning session without
    /// exposing the backend-local thread identity.
    pub(crate) fn thread_session_id(&self, global_thread_id: u64) -> Option<u64> {
        self.model
            .local_thread_id(GlobalThreadId::new(global_thread_id))
            .map(|local| local.0)
    }

    async fn active_session_ids(&self, session_ids: &[u64]) -> Vec<u64> {
        let mut active = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            if self
                .model
                .session_snapshot(*session_id)
                .await
                .is_some_and(|session| session.status == SessionStatus::ON)
            {
                active.push(*session_id);
            }
        }
        active
    }

    pub(crate) async fn threads_for_sessions(&self, session_ids: &[u64]) -> Vec<ThreadView> {
        let active_session_ids = self.active_session_ids(session_ids).await;
        self.model
            .thread_snapshots_for_sessions(&active_session_ids)
            .await
            .into_iter()
            .map(|thread| ThreadView {
                global_id: thread.global_id.value(),
                process_id: thread.process_id.map(|id| id.value()),
                session_id: thread.session_id,
                group_id: self
                    .model
                    .group_id_by_session(thread.session_id)
                    .map(|group_id| group_id.value()),
                backend_thread_id: thread.local_id.to_string(),
                status: match thread.status {
                    ThreadStatus::INIT => "unavailable",
                    ThreadStatus::STOPPED => "stopped",
                    ThreadStatus::RUNNING => "running",
                },
                selected: thread.selected,
                execution_revision: thread.execution_revision,
                location: thread.location,
            })
            .collect()
    }

    pub(crate) async fn processes_for_sessions(&self, session_ids: &[u64]) -> Vec<ProcessView> {
        let active_session_ids = self.active_session_ids(session_ids).await;
        self.model
            .process_snapshots_for_sessions(&active_session_ids)
            .await
            .into_iter()
            .map(|process| ProcessView {
                global_id: process.global_id.value(),
                session_id: process.session_id,
                group_id: self
                    .model
                    .group_id_by_session(process.session_id)
                    .map(|group_id| group_id.value()),
                system_process_id: process.system_process_id,
            })
            .collect()
    }

    pub(crate) async fn process_by_id(&self, global_process_id: u64) -> Option<ProcessView> {
        self.processes_for_sessions(&self.model.session_ids())
            .await
            .into_iter()
            .find(|process| process.global_id == global_process_id)
    }

    pub(crate) async fn thread_by_id(&self, global_thread_id: u64) -> Option<ThreadView> {
        let session_id = self.thread_session_id(global_thread_id)?;
        self.threads_for_sessions(&[session_id])
            .await
            .into_iter()
            .find(|thread| thread.global_id == global_thread_id)
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

    pub(crate) fn extension_descriptors(&self) -> Vec<ExtensionDescriptor> {
        self.extensions.descriptors()
    }

    pub(crate) fn extension_states(&self) -> Vec<ExtensionStateSnapshot> {
        let collection = self.extensions.collect_states();
        for failure in collection.failures {
            warn!(
                extension_id = %failure.extension_id,
                failure_kind = ?failure.kind,
                "extension state was omitted"
            );
        }
        collection.states
    }

    pub(crate) fn extension_registry(&self) -> Arc<ExtensionRegistry> {
        Arc::clone(&self.extensions)
    }

    pub(crate) fn extension_schema(
        &self,
        extension_id: &str,
        schema_uri: &str,
    ) -> Option<ExtensionSchema> {
        self.extensions.schema(extension_id, schema_uri).cloned()
    }

    pub(crate) fn legacy_extension_descriptors(&self) -> Vec<LegacyUiExtensionDescriptor> {
        self.extensions
            .descriptors()
            .into_iter()
            .map(|descriptor| LegacyUiExtensionDescriptor {
                id: descriptor.extension_id,
                title: descriptor.title,
                description: descriptor.description,
                panels: descriptor
                    .presentations
                    .into_iter()
                    .filter(|presentation| {
                        ExtensionPresentationKind::try_from(presentation.kind)
                            .is_ok_and(|kind| kind == ExtensionPresentationKind::Table)
                    })
                    .map(|presentation| LegacyUiPanelDescriptor {
                        id: presentation.id,
                        title: presentation.title,
                        columns: presentation
                            .columns
                            .into_iter()
                            .map(|column| column.title)
                            .collect(),
                    })
                    .collect(),
            })
            .collect()
    }

    pub(crate) fn legacy_extension_states(
        &self,
        states: &[ExtensionStateSnapshot],
    ) -> Vec<LegacyUiExtensionState> {
        states
            .iter()
            .filter_map(|state| {
                state.payloads.iter().find_map(|payload| {
                    let extension_payload::Payload::PayloadJson(json) = payload.payload.as_ref()?
                    else {
                        return None;
                    };
                    let legacy = serde_json::from_str::<LegacyUiExtensionState>(json).ok()?;
                    (legacy.id == state.extension_id).then_some(legacy)
                })
            })
            .collect()
    }

    /// Returns one detached hydration view for interactive clients. The
    /// repositories are intentionally sampled independently; this is a UI
    /// snapshot, not a transaction spanning live debugger sessions.
    pub(crate) async fn snapshot(&self) -> StateSnapshotView {
        let sessions = self.sessions().await;
        let session_ids = sessions
            .iter()
            .map(|session| session.sid)
            .collect::<Vec<_>>();
        let (processes, threads) = tokio::join!(
            self.processes_for_sessions(&session_ids),
            self.threads_for_sessions(&session_ids)
        );
        StateSnapshotView {
            selected_thread_id: self.model.current_thread_id().map(|id| id.value()),
            selected_session_id: self.model.current_session_id(),
            sessions,
            groups: self.groups(),
            processes,
            threads,
            breakpoints: self.breakpoints(),
            pending_commands: self.pending_commands(),
            pending_command_details: self.pending_command_details(),
            extensions: self.extension_states(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionView {
    pub(crate) sid: u64,
    pub(crate) tag: String,
    pub(crate) alias: String,
    pub(crate) status: String,
    #[serde(skip)]
    pub(crate) starting: bool,
    pub(crate) group: SessionGroupView,
    pub(crate) selected_thread_id: Option<u64>,
    pub(crate) in_custom_context: bool,
    pub(crate) all_threads_stopped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ThreadView {
    pub(crate) global_id: u64,
    pub(crate) process_id: Option<u64>,
    pub(crate) session_id: u64,
    pub(crate) group_id: Option<u64>,
    pub(crate) backend_thread_id: String,
    pub(crate) status: &'static str,
    pub(crate) selected: bool,
    pub(crate) execution_revision: u64,
    pub(crate) location: Option<crate::state::ThreadLocation>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessView {
    pub(crate) global_id: u64,
    pub(crate) session_id: u64,
    pub(crate) group_id: Option<u64>,
    pub(crate) system_process_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionGroupView {
    pub(crate) valid: bool,
    pub(crate) id: u64,
    pub(crate) hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PendingCommandView {
    pub(crate) sid: u64,
    pub(crate) in_flight: usize,
    pub(crate) queued: usize,
    pub(crate) closed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ApiPendingCommandView {
    pub(crate) sid: u64,
    pub(crate) token: u64,
    pub(crate) operation_id: Option<String>,
    pub(crate) operation_kind: Option<u32>,
    pub(crate) enqueued_at: SystemTime,
    pub(crate) running: bool,
}

impl From<SessionPendingCommand> for ApiPendingCommandView {
    fn from(command: SessionPendingCommand) -> Self {
        Self {
            sid: command.sid,
            token: command.token,
            operation_id: command.operation_id,
            operation_kind: command.operation_kind,
            enqueued_at: command.enqueued_at,
            running: command.running,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GroupView {
    pub(crate) id: u64,
    pub(crate) hash: String,
    pub(crate) alias: String,
    pub(crate) sids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StateSnapshotView {
    pub(crate) selected_thread_id: Option<u64>,
    pub(crate) selected_session_id: Option<u64>,
    pub(crate) sessions: Vec<SessionView>,
    pub(crate) groups: Vec<GroupView>,
    pub(crate) processes: Vec<ProcessView>,
    pub(crate) threads: Vec<ThreadView>,
    pub(crate) breakpoints: Vec<BreakpointSnapshot>,
    pub(crate) pending_commands: Vec<PendingCommandView>,
    pub(crate) pending_command_details: Vec<ApiPendingCommandView>,
    #[serde(skip_serializing)]
    pub(crate) extensions: Vec<ExtensionStateSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LegacyUiExtensionDescriptor {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) panels: Vec<LegacyUiPanelDescriptor>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LegacyUiPanelDescriptor {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) columns: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LegacyUiExtensionState {
    pub(crate) id: String,
    pub(crate) panels: Vec<LegacyUiPanelState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LegacyUiPanelState {
    pub(crate) id: String,
    pub(crate) rows: Vec<Vec<String>>,
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
        let config = Arc::new(crate::common::Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        ApiQueries::new(model, router, resolver, config, plugin).unwrap()
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

    #[tokio::test]
    async fn starting_sessions_hide_debugger_resources_until_activation_completes() {
        let model = RuntimeModel::new();
        model.register_session(9, "starting-session", None).await;
        model.register_thread_group(9, "i1").await.unwrap();
        model.register_thread(9, 3, "i1").await.unwrap();
        let queries = queries(Arc::clone(&model));

        let sessions = queries.sessions().await;
        assert!(sessions[0].starting);
        assert_eq!(sessions[0].status, "OFF");
        assert!(queries.snapshot().await.threads.is_empty());

        model.complete_session_activation(9, None).await;

        let sessions = queries.sessions().await;
        assert!(!sessions[0].starting);
        assert_eq!(sessions[0].status, "ON");
        assert_eq!(queries.snapshot().await.threads.len(), 1);
    }
}
