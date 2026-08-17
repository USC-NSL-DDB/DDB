use std::collections::HashSet;

use ddb_api_types::v2::{self, target, DdbErrorCode};

use crate::{
    api::read_model::ApiQueries,
    cmd_flow::router::Target as CommandTarget,
    state::{GlobalThreadId, GroupId},
};

use super::{ApplicationError, OpaqueIdRegistry, ResourceIdKind};

const MAX_TARGET_DEPTH: usize = 8;
const MAX_TARGET_SELECTORS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetPurpose {
    Command,
    Breakpoint,
}

pub(crate) struct ResolvedTarget {
    pub(crate) public: v2::Target,
    pub(crate) command: CommandTarget,
    pub(crate) resolved_target_count: u32,
    pub(crate) session_ids: Vec<u64>,
}

pub(crate) struct TargetResolver<'a> {
    queries: &'a ApiQueries,
    ids: &'a OpaqueIdRegistry,
}

impl<'a> TargetResolver<'a> {
    pub(crate) fn new(queries: &'a ApiQueries, ids: &'a OpaqueIdRegistry) -> Self {
        Self { queries, ids }
    }

    pub(crate) async fn resolve(
        &self,
        target: Option<&v2::Target>,
        purpose: TargetPurpose,
    ) -> Result<ResolvedTarget, ApplicationError> {
        let target = target.ok_or_else(|| ApplicationError::invalid("target", "is required"))?;
        let active_sessions = self
            .queries
            .sessions()
            .await
            .into_iter()
            .map(|session| session.sid)
            .collect::<HashSet<_>>();
        let mut selector_count = 0;
        let node = self.resolve_node(target, purpose, &active_sessions, 0, &mut selector_count)?;
        if node.sessions.is_empty() && !node.allows_empty {
            return Err(ApplicationError::new(
                DdbErrorCode::NotReady,
                "target does not currently resolve to an active debugger session",
            )
            .retryable(true));
        }
        let mut session_ids = node.sessions.iter().copied().collect::<Vec<_>>();
        session_ids.sort_unstable();

        Ok(ResolvedTarget {
            public: node.public,
            command: node.command,
            resolved_target_count: u32::try_from(node.sessions.len()).unwrap_or(u32::MAX),
            session_ids,
        })
    }

    pub(crate) fn operation_id(
        &self,
        target: Option<&v2::Target>,
    ) -> Result<String, ApplicationError> {
        let selector = target
            .and_then(|target| target.selector.as_ref())
            .ok_or_else(|| ApplicationError::invalid("target", "is required"))?;
        match selector {
            target::Selector::Operation(operation) if !operation.operation_id.trim().is_empty() => {
                Ok(operation.operation_id.clone())
            }
            target::Selector::Operation(_) => Err(ApplicationError::invalid(
                "target.operation.operation_id",
                "must not be empty",
            )),
            _ => Err(ApplicationError::invalid(
                "target",
                "must contain an operation selector",
            )),
        }
    }

    fn resolve_node(
        &self,
        public: &v2::Target,
        purpose: TargetPurpose,
        active_sessions: &HashSet<u64>,
        depth: usize,
        selector_count: &mut usize,
    ) -> Result<ResolvedNode, ApplicationError> {
        if depth > MAX_TARGET_DEPTH {
            return Err(ApplicationError::invalid(
                "target.multiple.targets",
                format!("target nesting must not exceed {MAX_TARGET_DEPTH}"),
            ));
        }
        *selector_count += 1;
        if *selector_count > MAX_TARGET_SELECTORS {
            return Err(ApplicationError::invalid(
                "target.multiple.targets",
                format!("must not contain more than {MAX_TARGET_SELECTORS} selectors"),
            ));
        }
        let selector = public
            .selector
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("target.selector", "is required"))?;

        match selector {
            target::Selector::Session(session) => {
                let sid = self.decode_u64(
                    ResourceIdKind::Session,
                    &session.session_id,
                    "target.session.session_id",
                )?;
                self.require_session(active_sessions, sid)?;
                Ok(ResolvedNode::one(
                    public.clone(),
                    CommandTarget::Session(sid),
                    sid,
                ))
            }
            target::Selector::Thread(thread) => {
                let global_id = self.decode_u64(
                    ResourceIdKind::Thread,
                    &thread.thread_id,
                    "target.thread.thread_id",
                )?;
                let sid = self
                    .queries
                    .thread_session_id(global_id)
                    .ok_or_else(|| ApplicationError::not_found("thread"))?;
                self.require_session(active_sessions, sid)?;
                Ok(ResolvedNode::one(
                    public.clone(),
                    CommandTarget::Thread(GlobalThreadId::new(global_id)),
                    sid,
                ))
            }
            target::Selector::Group(group) => {
                let gid = self.decode_u64(
                    ResourceIdKind::Group,
                    &group.group_id,
                    "target.group.group_id",
                )?;
                let group_view = self
                    .queries
                    .group_by_id(gid)
                    .ok_or_else(|| ApplicationError::not_found("group"))?;
                let sessions = group_view
                    .sids
                    .into_iter()
                    .filter(|sid| active_sessions.contains(sid))
                    .collect::<HashSet<_>>();
                let command = if purpose == TargetPurpose::Breakpoint {
                    CommandTarget::Group(GroupId::new(gid))
                } else {
                    CommandTarget::SessionSet(sessions.clone())
                };
                Ok(ResolvedNode {
                    public: public.clone(),
                    command,
                    sessions,
                    allows_empty: purpose == TargetPurpose::Breakpoint,
                })
            }
            target::Selector::CurrentThread(_) => {
                let (_, thread_id) = self.queries.selection_ids();
                let global_id = thread_id.ok_or_else(|| {
                    ApplicationError::new(
                        DdbErrorCode::FailedPrecondition,
                        "no thread is currently selected",
                    )
                })?;
                let sid = self
                    .queries
                    .thread_session_id(global_id)
                    .ok_or_else(|| ApplicationError::not_found("selected thread"))?;
                self.require_session(active_sessions, sid)?;
                Ok(ResolvedNode::one(
                    v2::Target {
                        selector: Some(target::Selector::Thread(v2::ThreadTarget {
                            thread_id: self.ids.encode(ResourceIdKind::Thread, global_id)?,
                        })),
                    },
                    CommandTarget::Thread(GlobalThreadId::new(global_id)),
                    sid,
                ))
            }
            target::Selector::CurrentSession(_) => {
                let (session_id, _) = self.queries.selection_ids();
                let sid = session_id.ok_or_else(|| {
                    ApplicationError::new(
                        DdbErrorCode::FailedPrecondition,
                        "no session is currently selected",
                    )
                })?;
                self.require_session(active_sessions, sid)?;
                Ok(ResolvedNode::one(
                    v2::Target {
                        selector: Some(target::Selector::Session(v2::SessionTarget {
                            session_id: self.ids.encode(ResourceIdKind::Session, sid)?,
                        })),
                    },
                    CommandTarget::Session(sid),
                    sid,
                ))
            }
            target::Selector::SessionSet(set) => {
                if set.session_ids.is_empty() {
                    return Err(ApplicationError::invalid(
                        "target.session_set.session_ids",
                        "must not be empty",
                    ));
                }
                let mut sessions = HashSet::new();
                for id in &set.session_ids {
                    let sid = self.decode_u64(
                        ResourceIdKind::Session,
                        id,
                        "target.session_set.session_ids",
                    )?;
                    self.require_session(active_sessions, sid)?;
                    sessions.insert(sid);
                }
                Ok(ResolvedNode {
                    public: public.clone(),
                    command: CommandTarget::SessionSet(sessions.clone()),
                    sessions,
                    allows_empty: false,
                })
            }
            target::Selector::Broadcast(_) => Ok(ResolvedNode {
                public: public.clone(),
                command: CommandTarget::Broadcast,
                sessions: active_sessions.clone(),
                allows_empty: false,
            }),
            target::Selector::First(_) => {
                let sid = active_sessions.iter().copied().min().ok_or_else(|| {
                    ApplicationError::new(
                        DdbErrorCode::NotReady,
                        "no active debugger session is available",
                    )
                })?;
                Ok(ResolvedNode::one(
                    v2::Target {
                        selector: Some(target::Selector::Session(v2::SessionTarget {
                            session_id: self.ids.encode(ResourceIdKind::Session, sid)?,
                        })),
                    },
                    CommandTarget::Session(sid),
                    sid,
                ))
            }
            target::Selector::Multiple(multiple) => {
                if multiple.targets.is_empty() {
                    return Err(ApplicationError::invalid(
                        "target.multiple.targets",
                        "must not be empty",
                    ));
                }
                let mut commands = Vec::new();
                let mut public_targets = Vec::new();
                let mut sessions = HashSet::new();
                let mut allows_empty = true;
                for child in &multiple.targets {
                    let node = self.resolve_node(
                        child,
                        purpose,
                        active_sessions,
                        depth + 1,
                        selector_count,
                    )?;
                    if purpose == TargetPurpose::Command
                        && node.sessions.iter().any(|sid| sessions.contains(sid))
                    {
                        return Err(ApplicationError::invalid(
                            "target.multiple.targets",
                            "selectors must not route the same command to one session more than once",
                        ));
                    }
                    allows_empty &= node.allows_empty;
                    sessions.extend(node.sessions);
                    commands.push(node.command);
                    public_targets.push(node.public);
                }
                Ok(ResolvedNode {
                    public: v2::Target {
                        selector: Some(target::Selector::Multiple(v2::MultipleTarget {
                            targets: public_targets,
                        })),
                    },
                    command: CommandTarget::Multiple(commands),
                    sessions,
                    allows_empty,
                })
            }
            target::Selector::Operation(_) => Err(ApplicationError::invalid(
                "target",
                "operation selectors are valid only for operation management",
            )),
        }
    }

    fn decode_u64(
        &self,
        kind: ResourceIdKind,
        public_id: &str,
        field: &'static str,
    ) -> Result<u64, ApplicationError> {
        if public_id.trim().is_empty() {
            return Err(ApplicationError::invalid(field, "must not be empty"));
        }
        self.ids
            .decode(kind, public_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("resource"))
    }

    fn require_session(
        &self,
        active_sessions: &HashSet<u64>,
        sid: u64,
    ) -> Result<(), ApplicationError> {
        if active_sessions.contains(&sid) {
            Ok(())
        } else {
            Err(ApplicationError::not_found("session"))
        }
    }
}

struct ResolvedNode {
    public: v2::Target,
    command: CommandTarget,
    sessions: HashSet<u64>,
    allows_empty: bool,
}

impl ResolvedNode {
    fn one(public: v2::Target, command: CommandTarget, sid: u64) -> Self {
        Self {
            public,
            command,
            sessions: HashSet::from([sid]),
            allows_empty: false,
        }
    }
}
