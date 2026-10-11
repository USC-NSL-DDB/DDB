//! Session-owned debug sources and explicitly configured source workspaces.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::Context;
use ddb_api_types::v2;
use ddb_source_workspace::{EntryKind, Limits, SourceCache, SourceRoot};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::{
    ApplicationError, DdbApplicationService, RequestScope, ResourceIdKind, TargetPurpose,
    TargetResolver,
};
use crate::{api::read_model::SessionView, common::config::SourceRootConfig};

const MAX_ROOTS: usize = 32;
const MAX_DEBUG_DIRECTORIES: usize = 128;
const MAX_CATALOG_SOURCES: usize = 100_000;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

struct Export {
    config: SourceRootConfig,
    directory: Arc<SourceRoot>,
}

pub(super) struct SourceStore {
    cache: Arc<SourceCache>,
    roots: BTreeMap<String, Export>,
    // Exact debug-info files can be read without exporting their containing
    // directories. These handles are never accepted by ListSourceEntries.
    debug_directories: Mutex<HashMap<PathBuf, Arc<SourceRoot>>>,
    slots: Arc<Semaphore>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "origin", deny_unknown_fields)]
enum Identity {
    Debugger { session: u64, path: String },
    Workspace { root: String, path: String },
}

struct ContentLocation {
    root_name: Option<String>,
    path: String,
    directory: Arc<SourceRoot>,
}

impl SourceStore {
    pub(super) fn new(configs: &[SourceRootConfig]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            configs.len() <= MAX_ROOTS,
            "at most {MAX_ROOTS} SourceRoots may be exported"
        );
        let mut roots = BTreeMap::new();
        let cache = Arc::new(SourceCache::default());
        for config in configs {
            anyhow::ensure!(
                !config.name.trim().is_empty()
                    && config.name.len() <= 128
                    && !config.name.chars().any(char::is_control),
                "source root names must be nonempty, bounded text"
            );
            anyhow::ensure!(
                !roots.contains_key(&config.name),
                "duplicate source root name: {}",
                config.name
            );
            anyhow::ensure!(
                !config.path.as_os_str().is_empty(),
                "source root path cannot be empty"
            );
            if let Some(prefix) = &config.debugger_path {
                anyhow::ensure!(
                    !prefix.is_empty()
                        && prefix.len() <= 4096
                        && !prefix.chars().any(char::is_control),
                    "invalid debugger source prefix"
                );
            }
            anyhow::ensure!(
                config.session_tags.len() <= 128
                    && config.session_tags.iter().all(|tag| !tag.is_empty()
                        && tag.len() <= 4096
                        && !tag.chars().any(char::is_control)),
                "invalid source root session tags"
            );
            let directory =
                SourceRoot::open_with_cache(&config.path, Limits::default(), Arc::clone(&cache))
                    .with_context(|| format!("open source root {}", config.name))?;
            roots.insert(
                config.name.clone(),
                Export {
                    config: config.clone(),
                    directory: Arc::new(directory),
                },
            );
        }
        Ok(Self {
            cache,
            roots,
            debug_directories: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(4)),
        })
    }

    fn content_location(
        &self,
        identity: &Identity,
        tag: Option<&str>,
    ) -> Result<ContentLocation, ApplicationError> {
        match identity {
            Identity::Workspace { root, path } => {
                let export = self
                    .roots
                    .get(root)
                    .ok_or_else(|| ApplicationError::not_found("source root"))?;
                Ok(ContentLocation {
                    root_name: Some(root.clone()),
                    path: path.clone(),
                    directory: Arc::clone(&export.directory),
                })
            }
            Identity::Debugger { path, .. } => {
                let mut matches = self
                    .roots
                    .values()
                    .filter_map(|export| {
                        if !export.config.session_tags.is_empty()
                            && !tag.is_some_and(|tag| {
                                export
                                    .config
                                    .session_tags
                                    .iter()
                                    .any(|candidate| candidate == tag)
                            })
                        {
                            return None;
                        }
                        let prefix = export.config.debugger_path.as_ref()?;
                        let relative = Path::new(path).strip_prefix(prefix).ok()?;
                        Some((
                            prefix.len(),
                            !export.config.session_tags.is_empty(),
                            export,
                            relative,
                        ))
                    })
                    .collect::<Vec<_>>();
                matches.sort_by_key(|(length, scoped, _, _)| (*length, *scoped));
                if let Some((length, scoped, export, relative)) = matches.pop() {
                    if matches
                        .last()
                        .is_some_and(|(other_length, other_scoped, _, _)| {
                            (*other_length, *other_scoped) == (length, scoped)
                        })
                    {
                        return Err(ApplicationError::new(v2::DdbErrorCode::Conflict, "multiple source roots map this debugger path; disambiguate their prefixes or session tags"));
                    }
                    return Ok(ContentLocation {
                        root_name: Some(export.config.name.clone()),
                        path: relative
                            .to_str()
                            .ok_or_else(|| ApplicationError::not_found("UTF-8 source path"))?
                            .to_owned(),
                        directory: Arc::clone(&export.directory),
                    });
                }
                // This path is reached only for an exact catalog-authorized
                // debugger source, never for an arbitrary client path.
                let canonical = std::fs::canonicalize(path).map_err(|_| {
                    ApplicationError::not_found("source content; configure a source root mapping")
                })?;
                let parent = canonical
                    .parent()
                    .ok_or_else(|| ApplicationError::not_found("source directory"))?;
                let filename = canonical
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| ApplicationError::not_found("UTF-8 source name"))?
                    .to_owned();
                let mut directories = self
                    .debug_directories
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let directory = match directories.get(parent) {
                    Some(directory) => Arc::clone(directory),
                    None => {
                        if directories.len() >= MAX_DEBUG_DIRECTORIES {
                            return Err(ApplicationError::resource_exhausted("debugger source directory handle limit reached; configure project source roots"));
                        }
                        let directory = Arc::new(
                            SourceRoot::open_with_cache(
                                parent,
                                Limits::default(),
                                Arc::clone(&self.cache),
                            )
                            .map_err(source_error)?,
                        );
                        directories.insert(parent.to_path_buf(), Arc::clone(&directory));
                        directory
                    }
                };
                Ok(ContentLocation {
                    root_name: None,
                    path: filename,
                    directory,
                })
            }
        }
    }
}

impl DdbApplicationService {
    async fn source_io<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&SourceStore) -> Result<T, ApplicationError> + Send + 'static,
    ) -> Result<T, ApplicationError> {
        let store = Arc::clone(&self.sources);
        let permit = Arc::clone(&store.slots)
            .acquire_owned()
            .await
            .map_err(|_| ApplicationError::backend("source reader is shutting down"))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&store)
        })
        .await
        .map_err(|_| ApplicationError::backend("source reader task failed"))?
    }

    fn source_owner(&self, session: &SessionView) -> Result<v2::SourceOwner, ApplicationError> {
        Ok(v2::SourceOwner {
            session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
            group_id: session
                .group
                .valid
                .then(|| self.ids.encode(ResourceIdKind::Group, session.group.id))
                .transpose()?,
        })
    }

    fn source_identity(&self, reference: &str) -> Result<Identity, ApplicationError> {
        let internal = self.ids.decode(ResourceIdKind::Source, reference)?;
        serde_json::from_str(&internal).map_err(|_| ApplicationError::not_found("source identity"))
    }

    async fn with_source_document<T: Send + 'static>(
        &self,
        identity: Identity,
        sessions: &[SessionView],
        read: impl FnOnce(&ddb_source_workspace::Document) -> ddb_source_workspace::Result<T>
            + Send
            + 'static,
    ) -> Result<T, ApplicationError> {
        let tag = match &identity {
            Identity::Debugger { session, path } => {
                let owner = sessions
                    .iter()
                    .find(|owner| owner.sid == *session)
                    .ok_or_else(|| ApplicationError::not_found("source owner"))?;
                if self.queries.cached_source_membership(*session, path) != Some(true) {
                    return Err(ApplicationError::not_found("source in current catalog"));
                }
                Some(owner.tag.clone())
            }
            Identity::Workspace { .. } => None,
        };
        self.source_io(move |store| {
            let location = store.content_location(&identity, tag.as_deref())?;
            let document = location
                .directory
                .read(&location.path)
                .map_err(source_error)?;
            read(&document).map_err(source_error)
        })
        .await
    }

    async fn describe_source(
        &self,
        identity: Identity,
        sessions: &[SessionView],
    ) -> Result<v2::SourceFile, ApplicationError> {
        let internal = serde_json::to_string(&identity)
            .map_err(|_| ApplicationError::backend("cannot encode source identity"))?;
        let source_reference = self.ids.encode(ResourceIdKind::Source, internal)?;
        let (path, origin, owners, debugger_path, tag) = match &identity {
            Identity::Debugger { session, path } => {
                let owner = sessions
                    .iter()
                    .find(|candidate| candidate.sid == *session)
                    .ok_or_else(|| ApplicationError::not_found("source owner"))?;
                (
                    path.clone(),
                    v2::SourceOrigin::Debugger,
                    vec![self.source_owner(owner)?],
                    Some(path.clone()),
                    Some(owner.tag.clone()),
                )
            }
            Identity::Workspace { root, path } => {
                let export = self
                    .sources
                    .roots
                    .get(root)
                    .ok_or_else(|| ApplicationError::not_found("source root"))?;
                let owners = sessions
                    .iter()
                    .filter(|session| {
                        !export.config.session_tags.is_empty()
                            && export.config.session_tags.contains(&session.tag)
                    })
                    .map(|session| self.source_owner(session))
                    .collect::<Result<Vec<_>, _>>()?;
                let debugger_path = export
                    .config
                    .debugger_path
                    .as_ref()
                    .map(|prefix| Path::new(prefix).join(path).to_string_lossy().into_owned());
                (
                    format!("{root}/{path}"),
                    v2::SourceOrigin::Workspace,
                    owners,
                    debugger_path,
                    None,
                )
            }
        };
        let description = self
            .source_io(move |store| {
                let location = store.content_location(&identity, tag.as_deref())?;
                let available = location
                    .directory
                    .check_file(&location.path)
                    .map_err(source_error);
                Ok((location.root_name, location.path, available))
            })
            .await;
        let (root, relative, availability, reason) = match description {
            Ok((root, relative, Ok(()))) => (
                root,
                Some(relative),
                v2::SourceAvailability::Available,
                None,
            ),
            Ok((root, relative, Err(error))) => (
                root,
                Some(relative),
                v2::SourceAvailability::Unavailable,
                Some(error.to_string()),
            ),
            Err(error) => (
                None,
                None,
                v2::SourceAvailability::Unavailable,
                Some(error.to_string()),
            ),
        };
        let root_reference = root
            .map(|name| self.ids.encode(ResourceIdKind::SourceRoot, name))
            .transpose()?;
        Ok(v2::SourceFile {
            source_reference,
            name: Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&path)
                .to_owned(),
            path: Some(path),
            media_type: "text/plain; charset=utf-8".into(),
            content_hash: None,
            origin: origin as i32,
            root_reference,
            relative_path: relative,
            owners,
            availability: availability as i32,
            unavailable_reason: reason,
            debugger_path,
        })
    }

    pub(crate) async fn list_source_roots(
        &self,
        request: v2::ListSourceRootsRequest,
    ) -> Result<v2::ListSourceRootsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let sessions = self.queries.sessions().await;
        let roots = self
            .sources
            .roots
            .values()
            .map(|export| {
                Ok(v2::SourceRoot {
                    root_reference: self
                        .ids
                        .encode(ResourceIdKind::SourceRoot, &export.config.name)?,
                    name: export.config.name.clone(),
                    debugger_path: export.config.debugger_path.clone(),
                    owners: sessions
                        .iter()
                        .filter(|session| {
                            !export.config.session_tags.is_empty()
                                && export.config.session_tags.contains(&session.tag)
                        })
                        .map(|session| self.source_owner(session))
                        .collect::<Result<Vec<_>, _>>()?,
                })
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        let page = self.pages.paginate(
            "source-roots",
            super::collection_revision(&roots),
            roots,
            request.page.as_ref(),
        )?;
        scope.ensure_active()?;
        Ok(v2::ListSourceRootsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            roots: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn list_source_entries(
        &self,
        request: v2::ListSourceEntriesRequest,
    ) -> Result<v2::ListSourceEntriesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let root = self
            .ids
            .decode(ResourceIdKind::SourceRoot, &request.root_reference)?;
        let name = root.clone();
        let directory = request.directory.clone();
        let include_hidden = request.include_hidden;
        let continuation = request
            .page
            .as_ref()
            .and_then(|page| page.page_token.as_ref())
            .is_some();
        let listing = self
            .source_io(move |store| {
                let export = store
                    .roots
                    .get(&name)
                    .ok_or_else(|| ApplicationError::not_found("source root"))?;
                if continuation {
                    export.directory.list_cached(&directory, include_hidden)
                } else {
                    export.directory.list(&directory, include_hidden)
                }
                .map_err(source_error)
            })
            .await?;
        let collection = format!(
            "source-entries:{}:{}:{}",
            request.root_reference, request.directory, include_hidden
        );
        let revision = revision(&listing.revision);
        let window = self
            .pages
            .window(&collection, revision, request.page.as_ref())?;
        if window.offset > listing.entries.len() {
            return Err(ApplicationError::new(
                v2::DdbErrorCode::Expired,
                "directory page no longer exists",
            ));
        }
        let items = listing
            .entries
            .iter()
            .skip(window.offset)
            .take(window.size + 1)
            .cloned()
            .collect();
        let page = self
            .pages
            .finish_window(&collection, revision, window, items);
        let sessions = self.queries.sessions().await;
        let mut entries = Vec::with_capacity(page.items.len());
        for entry in page.items {
            scope.ensure_active()?;
            let kind = match entry.kind {
                EntryKind::Directory => v2::SourceEntryKind::Directory,
                EntryKind::File => v2::SourceEntryKind::File,
                EntryKind::Symlink => v2::SourceEntryKind::Symlink,
                EntryKind::Other => v2::SourceEntryKind::Other,
            };
            let source = if entry.kind == EntryKind::File {
                Some(
                    self.describe_source(
                        Identity::Workspace {
                            root: root.clone(),
                            path: entry.relative_path.clone(),
                        },
                        &sessions,
                    )
                    .await?,
                )
            } else {
                None
            };
            entries.push(v2::SourceEntry {
                name: entry.name,
                relative_path: entry.relative_path,
                kind: kind as i32,
                source,
            });
        }
        Ok(v2::ListSourceEntriesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            entries,
            page: Some(page.info),
            revision: listing.revision.clone(),
            unsupported_names: listing.unsupported_names as u32,
        })
    }

    pub(crate) async fn list_sources(
        &self,
        request: v2::ListSourcesRequest,
    ) -> Result<v2::ListSourcesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.query.len() > 4096
            || (request.refresh
                && request
                    .page
                    .as_ref()
                    .and_then(|page| page.page_token.as_ref())
                    .is_some())
        {
            return Err(ApplicationError::invalid(
                "query/page",
                "query is too long, or refresh was requested with a page token",
            ));
        }
        let sessions = self.queries.sessions().await;
        let selected = if request.target.is_some() {
            Some(
                TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(request.target.as_ref(), TargetPurpose::Command)
                    .await?
                    .session_ids,
            )
        } else {
            None
        };
        let mut identities = Vec::new();
        let mut identity_bytes = 0usize;
        let mut warnings = Vec::new();
        let query = request.query.to_lowercase();
        for session in &sessions {
            if selected
                .as_ref()
                .is_some_and(|selected| !selected.contains(&session.sid))
            {
                continue;
            }
            scope.ensure_active()?;
            let paths = if session.all_threads_stopped {
                match self
                    .queries
                    .source_listing(session.sid, request.refresh)
                    .await
                {
                    Ok(paths) => paths,
                    Err(_) => {
                        warnings.push(v2::SourceDiscoveryWarning {
                            session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
                            message: "debugger source discovery failed; showing cached entries"
                                .into(),
                        });
                        self.queries.cached_sources(session.sid)
                    }
                }
            } else {
                warnings.push(v2::SourceDiscoveryWarning {
                    session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
                    message:
                        "target is not stopped; showing cached sources without interrupting it"
                            .into(),
                });
                self.queries.cached_sources(session.sid)
            };
            for path in paths {
                if query.is_empty() || path.to_lowercase().contains(&query) {
                    identity_bytes =
                        identity_bytes.saturating_add(path.len() + std::mem::size_of::<Identity>());
                    identities.push(Identity::Debugger {
                        session: session.sid,
                        path,
                    });
                    if identities.len() > MAX_CATALOG_SOURCES || identity_bytes > MAX_CATALOG_BYTES
                    {
                        return Err(ApplicationError::resource_exhausted(
                            "source catalog is too large; narrow the target or query",
                        ));
                    }
                }
            }
        }
        let key = serde_json::to_string(&identities)
            .map_err(|_| ApplicationError::backend("cannot encode source revision"))?;
        let collection = format!(
            "sources:{}:{}",
            serde_json::to_string(&request.target).unwrap_or_default(),
            query
        );
        let page = self.pages.paginate(
            &collection,
            revision(&key),
            identities,
            request.page.as_ref(),
        )?;
        let mut sources = Vec::with_capacity(page.items.len());
        for identity in page.items {
            scope.ensure_active()?;
            sources.push(self.describe_source(identity, &sessions).await?);
        }
        Ok(v2::ListSourcesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            sources,
            page: Some(page.info),
            warnings,
        })
    }

    pub(crate) async fn resolve_source_targets(
        &self,
        request: v2::ResolveSourceTargetsRequest,
    ) -> Result<v2::ResolveSourceTargetsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.source.trim().is_empty()
            || request.source.len() > 4096
            || request.source.chars().any(char::is_control)
        {
            return Err(ApplicationError::invalid(
                "source",
                "requires a nonempty debugger path of at most 4096 bytes without controls",
            ));
        }
        let sessions = self.queries.sessions().await;
        let selected = if request.target.is_some() {
            Some(
                TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(request.target.as_ref(), TargetPurpose::Command)
                    .await?
                    .session_ids,
            )
        } else {
            None
        };
        let selected_sessions: Vec<_> = sessions
            .into_iter()
            .filter(|session| {
                selected
                    .as_ref()
                    .is_none_or(|selected| selected.contains(&session.sid))
            })
            .collect();
        if selected_sessions.len() > 10_000 {
            return Err(ApplicationError::resource_exhausted(
                "too many sessions for source target discovery; narrow the target",
            ));
        }
        let discovery = stream::iter(selected_sessions.into_iter().map(|session| {
            let source = request.source.as_str();
            let refresh = request.refresh;
            async move {
                let cached = self.queries.cached_source_membership(session.sid, source);
                if !session.all_threads_stopped {
                    return (session, cached, cached.is_none().then_some(
                        "target is running or starting; source membership is unknown without interrupting it"));
                }
                match self.queries.source_membership(session.sid, source, refresh).await {
                    Ok(found) => (session, Some(found), None),
                    Err(_) => (session, cached, Some("debugger source discovery failed; only cached matches are available")),
                }
            }
        })).buffer_unordered(4);
        futures::pin_mut!(discovery);
        let mut matched = Vec::new();
        let mut warnings = Vec::new();
        while let Some((session, found, warning)) = scope.wait(discovery.next()).await? {
            if let Some(message) = warning {
                warnings.push(v2::SourceDiscoveryWarning {
                    session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
                    message: message.into(),
                });
            }
            if found == Some(true) {
                matched.push(session);
            }
        }
        // A retired session or changed group must not be published from an old
        // discovery job. Results remain an observation, never a mutation lease.
        let current = self
            .queries
            .sessions()
            .await
            .into_iter()
            .map(|session| (session.sid, session))
            .collect::<BTreeMap<_, _>>();
        let mut targets = Vec::new();
        let mut groups = BTreeMap::<u64, u32>::new();
        for session in matched {
            let Some(now) = current.get(&session.sid).filter(|now| {
                now.group.id == session.group.id && now.group.valid == session.group.valid
            }) else {
                warnings.push(v2::SourceDiscoveryWarning {
                    session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
                    message: "session topology changed during discovery; retry".into(),
                });
                continue;
            };
            targets.push(v2::SourceTarget {
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: self.ids.encode(ResourceIdKind::Session, now.sid)?,
                    })),
                }),
                display_name: format!("{} ({})", now.alias, now.tag),
                matching_session_count: 1,
            });
            if now.group.valid {
                *groups.entry(now.group.id).or_default() += 1;
            }
        }
        for (id, matching_session_count) in groups {
            if let Some(group) = self.queries.group_by_id(id) {
                targets.push(v2::SourceTarget {
                    target: Some(v2::Target {
                        selector: Some(v2::target::Selector::Group(v2::GroupTarget {
                            group_id: self.ids.encode(ResourceIdKind::Group, id)?,
                        })),
                    }),
                    display_name: group.alias,
                    matching_session_count,
                });
            }
        }
        if targets.len() > 10_000 {
            return Err(ApplicationError::resource_exhausted(
                "source target result exceeds 10000 entries; narrow the target",
            ));
        }
        targets.sort_by_key(|entry| {
            match entry
                .target
                .as_ref()
                .and_then(|target| target.selector.as_ref())
            {
                Some(v2::target::Selector::Group(group)) => (0, group.group_id.clone()),
                Some(v2::target::Selector::Session(session)) => (1, session.session_id.clone()),
                _ => unreachable!("only group and session targets are constructed"),
            }
        });
        warnings.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        scope.ensure_active()?;
        Ok(v2::ResolveSourceTargetsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            targets,
            complete: warnings.is_empty(),
            warnings,
        })
    }

    pub(crate) async fn resolve_source(
        &self,
        request: v2::ResolveSourceRequest,
    ) -> Result<v2::ResolveSourceResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let location = request
            .location
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("location", "is required"))?;
        let sessions = self.queries.sessions().await;
        let identity = if let Some(reference) = &location.source_reference {
            if request.root_reference.is_some() {
                return Err(ApplicationError::invalid(
                    "root_reference",
                    "cannot be combined with source_reference",
                ));
            }
            self.source_identity(reference)?
        } else if let Some(root) = &request.root_reference {
            let root = self.ids.decode(ResourceIdKind::SourceRoot, root)?;
            let path = normalized_relative(location.path.as_deref().unwrap_or_default())?;
            Identity::Workspace { root, path }
        } else {
            let path = location
                .path
                .as_deref()
                .filter(|path| !path.is_empty() && path.len() <= 4096)
                .ok_or_else(|| {
                    ApplicationError::invalid("location.path", "a bounded source path is required")
                })?;
            let selected = if request.target.is_some() {
                Some(
                    TargetResolver::new(self.queries.as_ref(), &self.ids)
                        .resolve(request.target.as_ref(), TargetPurpose::Command)
                        .await?
                        .session_ids,
                )
            } else {
                None
            };
            let mut owners = Vec::new();
            for session in &sessions {
                if selected
                    .as_ref()
                    .is_some_and(|selected| !selected.contains(&session.sid))
                {
                    continue;
                }
                let sources = if session.all_threads_stopped {
                    self.queries
                        .source_listing(session.sid, false)
                        .await
                        .map_err(|_| ApplicationError::backend("source discovery failed"))?
                } else {
                    self.queries.cached_sources(session.sid)
                };
                if sources.iter().any(|candidate| candidate == path) {
                    owners.push(session.sid);
                }
            }
            match owners.as_slice() {
                [session] => Identity::Debugger { session: *session, path: path.into() },
                [] => return Err(ApplicationError::not_found("debugger source")),
                _ => return Err(ApplicationError::new(v2::DdbErrorCode::Conflict, "source belongs to multiple sessions; supply its source_reference or an explicit session target")),
            }
        };
        let scoped = match &identity {
            Identity::Debugger { .. } => true,
            Identity::Workspace { root, .. } => self
                .sources
                .roots
                .get(root)
                .is_some_and(|export| !export.config.session_tags.is_empty()),
        };
        let source = self.describe_source(identity, &sessions).await?;
        if let Some(target) = request.target.as_ref() {
            let selected = TargetResolver::new(self.queries.as_ref(), &self.ids)
                .resolve(Some(target), TargetPurpose::Command)
                .await?
                .session_ids;
            if scoped
                && !source.owners.iter().any(|owner| {
                    selected.iter().any(|sid| {
                        self.ids
                            .encode(ResourceIdKind::Session, sid)
                            .is_ok_and(|id| id == owner.session_id)
                    })
                })
            {
                return Err(ApplicationError::not_found("source for target"));
            }
        }
        scope.ensure_active()?;
        Ok(v2::ResolveSourceResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            source: Some(source),
        })
    }

    pub(crate) async fn read_source(
        &self,
        request: v2::ReadSourceRequest,
    ) -> Result<v2::ReadSourceResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.start_line == 0
            || request.max_lines == 0
            || request.max_lines > self.api_config.limits.max_source_lines
        {
            return Err(ApplicationError::invalid(
                "start_line/max_lines",
                "source windows must be nonzero and within the advertised line limit",
            ));
        }
        let identity = self.source_identity(&request.source_reference)?;
        let sessions = self.queries.sessions().await;
        let window = scope
            .wait(
                self.with_source_document(identity.clone(), &sessions, move |document| {
                    document.window(request.start_line, request.max_lines)
                }),
            )
            .await??;
        scope.ensure_active()?;
        let mut source = self.describe_source(identity, &sessions).await?;
        source.content_hash = Some(window.content_hash);
        Ok(v2::ReadSourceResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            source: Some(v2::SourceContent {
                source: Some(source),
                start_line: window.start_line,
                content: window.content,
                line_count: window.line_count,
                has_more: window.has_more,
                total_lines: window.total_lines,
            }),
        })
    }

    pub(crate) async fn search_source(
        &self,
        request: v2::SearchSourceRequest,
    ) -> Result<v2::SearchSourceResponse, ApplicationError> {
        use ddb_source_workspace::{SearchMode, SearchQuery};
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.origin_line == 0
            || request.max_lines == 0
            || request.max_lines > self.api_config.limits.max_source_lines
        {
            return Err(ApplicationError::invalid(
                "origin_line/max_lines",
                "requires a one-based origin and a nonzero window within the advertised line limit",
            ));
        }
        let mode = match v2::SourceSearchMode::try_from(request.mode) {
            Ok(v2::SourceSearchMode::Exact) => SearchMode::Exact,
            Ok(v2::SourceSearchMode::Fuzzy) => SearchMode::Fuzzy,
            _ => return Err(ApplicationError::invalid("mode", "requires EXACT or FUZZY")),
        };
        let query = SearchQuery::new(&request.query, mode).map_err(source_error)?;
        let identity = self.source_identity(&request.source_reference)?;
        let sessions = self.queries.sessions().await;
        let (found, window, content_hash, total_lines) = scope
            .wait(
                self.with_source_document(identity.clone(), &sessions, move |document| {
                    let found = document.search(
                        &query,
                        request.origin_line,
                        request.backward,
                        request.include_origin,
                    );
                    let line = found
                        .as_ref()
                        .map_or(request.origin_line, |found| found.line)
                        .clamp(1, document.total_lines().max(1));
                    let window = document.window(
                        line.saturating_sub(request.max_lines / 2).max(1),
                        request.max_lines,
                    )?;
                    Ok((
                        found,
                        window,
                        document.content_hash().to_owned(),
                        document.total_lines(),
                    ))
                }),
            )
            .await??;
        let mut source = self.describe_source(identity, &sessions).await?;
        source.content_hash = Some(content_hash.clone());
        let source = Some(v2::SourceContent {
            source: Some(source),
            start_line: window.start_line,
            content: window.content,
            line_count: window.line_count,
            has_more: window.has_more,
            total_lines: window.total_lines,
        });
        scope.ensure_active()?;
        Ok(v2::SearchSourceResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            result: found.map(|found| v2::SourceSearchMatch {
                line: found.line,
                start_column: found.start_column,
                end_column: found.end_column,
                wrapped: found.wrapped,
            }),
            source,
            content_hash,
            total_lines,
        })
    }
}

fn normalized_relative(path: &str) -> Result<String, ApplicationError> {
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(ApplicationError::invalid(
            "location.path",
            "must be a bounded root-relative file path without parent traversal",
        ));
    }
    Ok(Path::new(path)
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn revision(value: &str) -> u64 {
    let digest = Sha256::digest(value.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"))
}

fn source_error(error: ddb_source_workspace::Error) -> ApplicationError {
    use ddb_source_workspace::Error;
    match error {
        Error::InvalidSearch(message) => ApplicationError::invalid("query", message),
        Error::InvalidPath => {
            ApplicationError::invalid("source path", "path must remain within its root")
        }
        Error::Limit(_) => ApplicationError::resource_exhausted(error.to_string()),
        Error::NotText => ApplicationError::new(
            v2::DdbErrorCode::Unsupported,
            "source is not a regular UTF-8 text file",
        ),
        Error::Changed => ApplicationError::new(
            v2::DdbErrorCode::Conflict,
            "source changed while being read",
        )
        .retryable(true),
        Error::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ApplicationError::new(
                v2::DdbErrorCode::PermissionDenied,
                "source access is denied or escapes its configured root",
            )
        }
        Error::Io(_) => ApplicationError::not_found("source content"),
    }
}
