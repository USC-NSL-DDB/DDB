//! Explicit TUI-local directory grants, mappings, and bounded filesystem work.

use crate::source_types::{Content, LocalRoot, LocalSource, Source};
use anyhow::{bail, Context, Result};
use ddb_api_client::v2;
use ddb_source_workspace::{Limits, SourceCache, SourceRoot};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{Arc, RwLock},
};
use tokio::sync::Semaphore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    roots: Vec<RootConfig>,
}

type RootConfig = ddb_config::SourceRoot;

struct Grant {
    metadata: LocalRoot,
    directory: Arc<SourceRoot>,
}

pub struct Workspace {
    roots: BTreeMap<String, Grant>,
    sessions: RwLock<Vec<v2::Session>>,
    slots: Arc<Semaphore>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            sessions: RwLock::new(Vec::new()),
            slots: Arc::new(Semaphore::new(4)),
        }
    }
}

impl Workspace {
    /// Called before terminal entry. No implicit cwd or backend-config roots.
    pub fn load(path: &Path) -> Result<Self> {
        let config: Config = ddb_config::read_yaml(path, ddb_config::MAX_CONFIG_BYTES)
            .context("invalid source workspace configuration")?;
        Self::from_roots(config.roots, path.parent().unwrap_or(Path::new(".")))
    }

    pub fn from_roots(roots: Vec<RootConfig>, base: &Path) -> Result<Self> {
        anyhow::ensure!(
            roots.len() <= 32,
            "at most 32 local source roots may be configured"
        );
        let mut workspace = Self::default();
        let cache = Arc::new(SourceCache::default());
        for config in roots {
            anyhow::ensure!(
                !config.name.trim().is_empty()
                    && config.name.len() <= 128
                    && !config
                        .name
                        .chars()
                        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\')),
                "local source root needs a bounded name without path separators"
            );
            anyhow::ensure!(
                !workspace.roots.contains_key(&config.name),
                "duplicate local source root {}",
                config.name
            );
            anyhow::ensure!(
                config.session_tags.len() <= 128
                    && config.session_tags.iter().all(|tag| !tag.is_empty()
                        && tag.len() <= 4096
                        && !tag.chars().any(char::is_control)),
                "invalid session_tags for {}",
                config.name
            );
            anyhow::ensure!(
                config.session_tags.iter().collect::<HashSet<_>>().len()
                    == config.session_tags.len(),
                "duplicate session_tags for {}",
                config.name
            );
            let debugger_path = config
                .debugger_path
                .as_deref()
                .map(debugger_prefix)
                .transpose()
                .with_context(|| format!("invalid debugger_path for {}", config.name))?;
            anyhow::ensure!(
                !config.path.as_os_str().is_empty(),
                "local source root path cannot be empty"
            );
            let path = if config.path.is_absolute() {
                config.path
            } else {
                base.join(config.path)
            };
            let directory = Arc::new(
                SourceRoot::open_with_cache(&path, Limits::default(), cache.clone()).with_context(
                    || {
                        format!(
                            "cannot open local source root {} at {}",
                            config.name,
                            path.display()
                        )
                    },
                )?,
            );
            let metadata = LocalRoot {
                name: config.name.clone(),
                debugger_path,
                session_tags: config.session_tags,
            };
            workspace.roots.insert(
                config.name,
                Grant {
                    metadata,
                    directory,
                },
            );
        }
        Ok(workspace)
    }

    pub fn roots(&self) -> Vec<LocalRoot> {
        self.roots
            .values()
            .map(|root| root.metadata.clone())
            .collect()
    }
    pub fn update_sessions(&self, sessions: &[v2::Session]) {
        *self.sessions.write().expect("session projection lock") = sessions.to_vec();
    }

    /// Select solely by explicit prefix and session tags, never by file existence.
    /// The longest prefix wins; scoped mappings beat unscoped peers. Ties fail.
    pub fn map_source(&self, source: Source) -> Result<Source> {
        let Source::Ddb { instance, file } = &source else {
            return Ok(source);
        };
        let Some(path) = file.debugger_path.as_deref() else {
            return Ok(source);
        };
        let sessions = self.sessions.read().expect("session projection lock");
        let mut selected = None;
        let mut specificity = (0, false);
        let mut ambiguous = false;
        for grant in self.roots.values() {
            let Some(prefix) = grant.metadata.debugger_path.as_deref() else {
                continue;
            };
            let relative = if prefix == "/" {
                path.strip_prefix('/')
            } else {
                path.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_prefix('/'))
            };
            let Some(relative) = relative.filter(|value| !value.is_empty()) else {
                continue;
            };
            let tags = &grant.metadata.session_tags;
            let scoped = !tags.is_empty();
            if scoped
                && (file.owners.is_empty()
                    || !file.owners.iter().all(|owner| {
                        sessions.iter().any(|session| {
                            session.session_id == owner.session_id && tags.contains(&session.tag)
                        })
                    }))
            {
                continue;
            }
            let rank = (prefix.len(), scoped);
            if selected.is_none() || rank > specificity {
                selected = Some((grant.metadata.clone(), relative.to_string()));
                specificity = rank;
                ambiguous = false;
            } else if rank == specificity {
                ambiguous = true;
            }
        }
        if ambiguous {
            bail!("ambiguous local source mapping for {path}; configure distinct prefixes or session_tags");
        }
        Ok(match selected {
            Some((root, path)) => Source::Local(LocalSource {
                root,
                path,
                owner_hint: file.owners.clone(),
                owner_instance: Some(instance.clone()),
            }),
            None => source,
        })
    }

    fn grant(&self, root: &LocalRoot) -> Result<Arc<SourceRoot>> {
        let grant = self
            .roots
            .get(&root.name)
            .context("unknown local source root")?;
        anyhow::ensure!(
            grant.metadata == *root,
            "local source root configuration changed"
        );
        Ok(grant.directory.clone())
    }

    pub async fn read(&self, source: LocalSource, line: u32) -> Result<Content> {
        let directory = self.grant(&source.root)?;
        let permit = self.slots.clone().acquire_owned().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let document = directory.read(&source.path)?;
            let line = line.clamp(1, document.total_lines().max(1));
            let window = document.window(line.saturating_sub(100).max(1), 240)?;
            Ok(Content {
                source: Source::Local(source),
                start_line: window.start_line,
                content: window.content,
                line_count: window.line_count,
                total_lines: window.total_lines,
            })
        })
        .await
        .context("local source read task failed")?
    }

    pub async fn search(
        &self,
        source: LocalSource,
        request: crate::source_search::Request,
    ) -> Result<crate::source_search::Outcome> {
        let directory = self.grant(&source.root)?;
        let query =
            ddb_source_workspace::SearchQuery::new(&request.query, request.mode.matching())?;
        let origin =
            u32::try_from(request.cursor).context("source search line exceeds the API limit")?;
        let permit = self.slots.clone().acquire_owned().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let document = directory.read(&source.path)?;
            let found = document.search(&query, origin, request.backward, request.include_origin);
            let line = found
                .as_ref()
                .map_or(origin, |hit| hit.line)
                .clamp(1, document.total_lines().max(1));
            let window = document.window(line.saturating_sub(120).max(1), 240)?;
            Ok(crate::source_search::Outcome {
                found,
                content: Content {
                    source: Source::Local(source),
                    start_line: window.start_line,
                    content: window.content,
                    line_count: window.line_count,
                    total_lines: window.total_lines,
                },
            })
        })
        .await
        .context("local source search task failed")?
    }

    pub async fn list(
        &self,
        root: LocalRoot,
        path: String,
        hidden: bool,
        continuation: bool,
    ) -> Result<Arc<ddb_source_workspace::Directory>> {
        let directory = self.grant(&root)?;
        let permit = self.slots.clone().acquire_owned().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Ok(if continuation {
                directory.list_cached(&path, hidden)?
            } else {
                directory.list(&path, hidden)?
            })
        })
        .await
        .context("local directory listing task failed")?
    }
}

fn debugger_prefix(prefix: &str) -> Result<String> {
    let normalized = if prefix == "/" {
        prefix
    } else {
        prefix.trim_end_matches('/')
    };
    anyhow::ensure!(
        prefix.len() <= 4096
            && normalized.starts_with('/')
            && !prefix.contains('\\')
            && !prefix.chars().any(char::is_control)
            && (normalized == "/"
                || !normalized[1..]
                    .split('/')
                    .any(|part| matches!(part, "" | "." | ".."))),
        "expected an absolute slash-separated path without traversal or empty components"
    );
    Ok(normalized.into())
}
