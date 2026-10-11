//! The transport/filesystem boundary for source reads and discovery.
//! A failed explicit mapping never silently switches providers.

use crate::{
    source_browser::SourceRequest,
    source_explorer::{Entry, Node, Page, Place, Request, PAGE_SIZE},
    source_types::{Address, Content, LocalRoot, LocalSource, Root, Source},
    source_workspace::Workspace,
};
use anyhow::{ensure, Context, Result};
use ddb_api_client::{v2, DdbClient};
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "source_provider_tests.rs"]
mod tests;

fn instance(context: Option<v2::ResponseContext>, expected: Option<&str>) -> Result<String> {
    let instance = context
        .context("DDB omitted source response context")?
        .server_instance_id;
    ensure!(!instance.is_empty(), "DDB omitted source server instance");
    ensure!(
        expected.is_none_or(|expected| expected == instance),
        "DDB instance changed during source request; refresh and retry"
    );
    Ok(instance)
}

fn remote(instance: String, file: v2::SourceFile) -> Result<Source> {
    ensure!(
        !file.source_reference.is_empty(),
        "DDB omitted source reference"
    );
    Ok(Source::Ddb { instance, file })
}

pub async fn read(
    client: &DdbClient,
    workspace: &Workspace,
    request: SourceRequest,
) -> Result<Content> {
    let line = u32::try_from(request.line).context("source line exceeds API limit")?;
    let source = match request.address {
        Address::File(source) => source,
        Address::Export {
            root: Root::Local(root),
            path,
        } => Source::Local(LocalSource {
            root,
            path,
            owner_hint: Vec::new(),
            owner_instance: None,
        }),
        address => {
            let (expected, target, root_reference, location) = match address {
                Address::Resolve {
                    instance,
                    target,
                    location,
                } => (instance, target, None, location),
                Address::Export {
                    root: Root::Ddb { instance, root },
                    path,
                } => (
                    Some(instance),
                    None,
                    Some(root.root_reference),
                    v2::SourceLocation {
                        path: Some(path),
                        ..Default::default()
                    },
                ),
                _ => unreachable!("local and resolved sources handled above"),
            };
            let response = client
                .resolve_source(v2::ResolveSourceRequest {
                    context: None,
                    target,
                    root_reference,
                    location: Some(location),
                })
                .await?;
            let instance = instance(response.context, expected.as_deref())?;
            remote(
                instance,
                response.source.context("ResolveSource omitted source")?,
            )?
        }
    };
    match workspace.map_source(source)? {
        Source::Local(source) => workspace.read(source, line).await,
        Source::Ddb {
            instance: expected,
            file,
        } => {
            let mut request = v2::ReadSourceRequest {
                context: None,
                source_reference: file.source_reference,
                start_line: line.saturating_sub(100).max(1),
                max_lines: 240,
            };
            let content = read_remote(client, &expected, request.clone()).await?;
            if content.line_count == 0
                && content.total_lines > 0
                && request.start_line > content.total_lines
            {
                request.start_line = content.total_lines.saturating_sub(239).max(1);
                return read_remote(client, &expected, request).await;
            }
            Ok(content)
        }
    }
}

async fn read_remote(
    client: &DdbClient,
    expected: &str,
    request: v2::ReadSourceRequest,
) -> Result<Content> {
    let reference = request.source_reference.clone();
    let response = client.read_source(request).await?;
    let instance = instance(response.context, Some(expected))?;
    let content = response.source.context("ReadSource omitted source")?;
    ensure!(
        content
            .source
            .as_ref()
            .is_some_and(|source| source.source_reference == reference),
        "ReadSource returned a different source identity"
    );
    Content::from_ddb(instance, content)
}

pub async fn search(
    client: &DdbClient,
    workspace: &Workspace,
    request: crate::source_search::Request,
) -> Result<crate::source_search::Outcome> {
    match &request.source {
        Source::Local(source) => workspace.search(source.clone(), request).await,
        Source::Ddb {
            instance: expected,
            file,
        } => {
            let response = client
                .search_source(v2::SearchSourceRequest {
                    source_reference: file.source_reference.clone(),
                    query: request.query,
                    mode: match request.mode {
                        crate::source_search::Mode::Exact => v2::SourceSearchMode::Exact as i32,
                        crate::source_search::Mode::Fuzzy => v2::SourceSearchMode::Fuzzy as i32,
                    },
                    origin_line: u32::try_from(request.cursor)
                        .context("source search line exceeds the API limit")?,
                    backward: request.backward,
                    include_origin: request.include_origin,
                    max_lines: 240,
                    context: None,
                })
                .await?;
            let instance = instance(response.context, Some(expected))?;
            let content = response
                .source
                .context("SearchSource omitted pinned context")?;
            ensure!(
                content
                    .source
                    .as_ref()
                    .is_some_and(|source| source.source_reference == file.source_reference
                        && source.content_hash.as_deref() == Some(response.content_hash.as_str()))
                    && !response.content_hash.is_empty()
                    && content.total_lines == response.total_lines,
                "SearchSource returned inconsistent source identity or version"
            );
            Ok(crate::source_search::Outcome {
                found: response
                    .result
                    .map(|hit| ddb_source_workspace::SearchMatch {
                        line: hit.line,
                        start_column: hit.start_column,
                        end_column: hit.end_column,
                        wrapped: hit.wrapped,
                    }),
                content: Content::from_ddb(instance, content)?,
            })
        }
    }
}

pub async fn discover(client: &DdbClient, workspace: &Workspace, request: Request) -> Result<Page> {
    let mut page = v2::PageRequest {
        page_size: PAGE_SIZE,
        page_token: request.page_token.clone(),
    };
    let (entries, next, warnings) = match request.place {
        Place::Roots => {
            let mut local: Vec<_> = if page.page_token.is_none() {
                workspace
                    .roots()
                    .into_iter()
                    .map(|root| Entry::Root(Root::Local(root)))
                    .collect()
            } else {
                Vec::new()
            };
            // Fixed remote page size across all pages (DDB binds tokens to it).
            page.page_size -= workspace.roots().len() as u32;
            let remote_roots = async {
                let response = client
                    .list_source_roots(v2::ListSourceRootsRequest {
                        page: Some(page),
                        ..Default::default()
                    })
                    .await?;
                let instance = instance(response.context, request.instance.as_deref())?;
                let roots = response
                    .roots
                    .into_iter()
                    .map(|root| {
                        ensure!(
                            !root.root_reference.is_empty(),
                            "DDB omitted root reference"
                        );
                        Ok(Entry::Root(Root::Ddb {
                            instance: instance.clone(),
                            root,
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok::<_, anyhow::Error>((roots, response.page))
            };
            // A stalled backend must not hold local directory access behind
            // the SDK's longer general-purpose request deadline.
            let result = if local.is_empty() {
                remote_roots.await
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(2), remote_roots)
                    .await
                    .context("DDB root discovery timed out; refresh to retry")
                    .and_then(|result| result)
            };
            match result {
                Ok((roots, next)) => {
                    local.extend(roots);
                    (local, next, Vec::new())
                }
                Err(error) if !local.is_empty() => (
                    local,
                    None,
                    vec![format!("DDB roots unavailable: {error:#}")],
                ),
                Err(error) => return Err(error),
            }
        }
        Place::Catalog => {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let response = client
                .list_sources(v2::ListSourcesRequest {
                    page: Some(page),
                    query: request.query,
                    refresh: request.refresh_catalog,
                    ..Default::default()
                })
                .await?;
            let instance = instance(response.context, request.instance.as_deref())?;
            let entries = response
                .sources
                .into_iter()
                .map(|file| remote(instance.clone(), file).map(Entry::Source))
                .collect::<Result<Vec<_>>>()?;
            (
                entries,
                response.page,
                response
                    .warnings
                    .into_iter()
                    .map(|warning| format!("{}: {}", warning.session_id, warning.message))
                    .collect(),
            )
        }
        Place::Directory {
            root: Root::Local(root),
            path,
        } => {
            return local_directory(
                workspace,
                root,
                path,
                request.include_hidden,
                request.page_token,
            )
            .await
        }
        Place::Directory {
            root:
                Root::Ddb {
                    instance: expected,
                    root,
                },
            path,
        } => {
            let response = client
                .list_source_entries(v2::ListSourceEntriesRequest {
                    root_reference: root.root_reference,
                    directory: path,
                    include_hidden: request.include_hidden,
                    page: Some(page),
                    ..Default::default()
                })
                .await?;
            let instance = instance(response.context, Some(&expected))?;
            let warnings = omitted_names(response.unsupported_names);
            let entries = response
                .entries
                .into_iter()
                .map(|entry| {
                    let node = Node {
                        name: entry.name,
                        relative_path: entry.relative_path,
                    };
                    Ok(
                        match v2::SourceEntryKind::try_from(entry.kind)
                            .context("unknown source entry kind")?
                        {
                            v2::SourceEntryKind::Directory => Entry::Directory(node),
                            v2::SourceEntryKind::File => Entry::Source(remote(
                                instance.clone(),
                                entry
                                    .source
                                    .context("file entry omitted source descriptor")?,
                            )?),
                            v2::SourceEntryKind::Symlink => Entry::Link(node),
                            _ => Entry::Other(node),
                        },
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            (entries, response.page, warnings)
        }
    };
    Ok(Page {
        entries,
        next_token: next.and_then(|page| page.next_page_token),
        warnings,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalPage {
    root: String,
    path: String,
    hidden: bool,
    revision: String,
    offset: usize,
}

async fn local_directory(
    workspace: &Workspace,
    root: LocalRoot,
    path: String,
    hidden: bool,
    token: Option<String>,
) -> Result<Page> {
    let token: Option<LocalPage> = token
        .map(|token| -> Result<_> {
            ensure!(
                token.len() <= 16 * 1024,
                "local directory token exceeds limit"
            );
            serde_json::from_str(&token).context("invalid local directory token")
        })
        .transpose()?;
    if let Some(token) = &token {
        ensure!(
            token.root == root.name && token.path == path && token.hidden == hidden,
            "local directory token does not match request"
        );
    }
    let directory = workspace
        .list(root.clone(), path.clone(), hidden, token.is_some())
        .await?;
    let offset = if let Some(token) = token {
        ensure!(
            token.revision == directory.revision,
            "local directory changed; refresh to restart pagination"
        );
        ensure!(
            token.offset > 0 && token.offset < directory.entries.len(),
            "invalid local directory offset"
        );
        token.offset
    } else {
        0
    };
    let end = offset
        .saturating_add(PAGE_SIZE as usize)
        .min(directory.entries.len());
    let entries = directory.entries[offset..end]
        .iter()
        .map(|entry| {
            use ddb_source_workspace::EntryKind;
            let node = Node {
                name: entry.name.clone(),
                relative_path: entry.relative_path.clone(),
            };
            match entry.kind {
                EntryKind::Directory => Entry::Directory(node),
                EntryKind::File => Entry::Source(Source::Local(LocalSource {
                    root: root.clone(),
                    path: entry.relative_path.clone(),
                    owner_hint: Vec::new(),
                    owner_instance: None,
                })),
                EntryKind::Symlink => Entry::Link(node),
                EntryKind::Other => Entry::Other(node),
            }
        })
        .collect();
    let next_token = (end < directory.entries.len())
        .then(|| {
            serde_json::to_string(&LocalPage {
                root: root.name,
                path,
                hidden,
                revision: directory.revision.clone(),
                offset: end,
            })
        })
        .transpose()?;
    Ok(Page {
        entries,
        next_token,
        warnings: omitted_names(directory.unsupported_names as u32),
    })
}

fn omitted_names(count: u32) -> Vec<String> {
    (count > 0)
        .then(|| format!("{count} unsupported filenames omitted"))
        .into_iter()
        .collect()
}
