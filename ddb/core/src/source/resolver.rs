use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::join_all;
use tokio::{
    sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock, Semaphore},
    task::JoinHandle,
};
use tracing::{debug, warn};

use crate::{
    cmd_flow::{
        api::CommandExecutor,
        decoder::{Payload, SourceFiles},
        router::Target,
        FinishedCmd,
    },
    state::{GroupId, GroupMeta, RuntimeModel},
};

use super::catalog::SourceCatalog;

const SOURCE_RESOLUTION_CONCURRENCY: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceResolutionPolicy {
    OnDemand,
    Eager,
}

impl SourceResolutionPolicy {
    pub(crate) const fn configured() -> Self {
        if cfg!(feature = "lazy_source_map") {
            Self::OnDemand
        } else {
            Self::Eager
        }
    }
}

#[async_trait]
trait SourceListingProvider: Send + Sync {
    /// Discover an exact path's membership, or list the complete source catalog.
    async fn list_sources(&self, sid: u64, path: Option<&str>) -> Result<Vec<String>>;
}

struct DebuggerSourceListingProvider {
    executor: CommandExecutor,
}

#[async_trait]
impl SourceListingProvider for DebuggerSourceListingProvider {
    async fn list_sources(&self, sid: u64, path: Option<&str>) -> Result<Vec<String>> {
        discover_source_paths(path, |command| async move {
            self.executor.execute(&command, Target::Session(sid)).await
        })
        .await
        .with_context(|| format!("failed to discover sources from session {sid}"))
    }
}

async fn discover_source_paths<F, Fut>(path: Option<&str>, mut execute: F) -> Result<Vec<String>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<FinishedCmd>>,
{
    let dirname = path
        .map(|path| {
            Path::new(path)
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(|| anyhow!("source path has no UTF-8 parent"))
        })
        .transpose()?;
    if let Some(path) = path.filter(|path| Path::new(path).is_absolute()) {
        let command = format!("-ddb-probe-source {}", serde_json::to_string(path)?);
        let completion = execute(command).await?;
        let proof = SourceFiles::decode(&completion)?.paths;
        ensure_source_budget(&proof)?;
        let payload = Payload::first(&completion)?;
        match (payload.string("source_status")?, proof.as_slice()) {
            ("present", [known]) if known == path => return Ok(proof),
            ("absent", []) => return Ok(proof),
            ("unknown", []) => {} // No proof is not a negative. Discover below.
            _ => return Err(anyhow!("debugger source probe returned an unexpected path")),
        }
    }
    let completion = execute(source_listing_command(dirname)?).await?;
    let paths = SourceFiles::decode(&completion)?.paths;
    ensure_source_budget(&paths)?;
    Ok(paths)
}

fn ensure_source_budget(paths: &[String]) -> Result<()> {
    if paths.len() > 100_000 || paths.iter().map(String::len).sum::<usize>() > 16 * 1024 * 1024 {
        return Err(anyhow!(
            "debugger source catalog exceeds its resource bound"
        ));
    }
    Ok(())
}

fn source_listing_command(dirname: Option<&str>) -> Result<String> {
    match dirname {
        Some(dirname) => {
            // GDB's source-listing operand uses basic regular expressions.
            // Encode literals in the BRE/Python common subset used by GDB and
            // the LLDB bridge. Exact-directory discovery is sufficient for the
            // requested path; a full catalog query remains unfiltered.
            if dirname.is_empty() {
                return Ok("-file-list-exec-source-files".to_owned());
            }
            let trimmed = dirname.trim_end_matches('/');
            let directory = if trimmed.is_empty() { "/" } else { trimmed };
            let mut pattern = String::from("^");
            for character in directory.chars() {
                match character {
                    '\\' | '.' | '^' | '$' | '[' | ']' | '*' => {
                        pattern.push('\\');
                        pattern.push(character);
                    }
                    // Backslash-escaping these changes their meaning in BRE.
                    // A single-character class is literal in both engines.
                    '(' | ')' | '|' | '+' | '?' | '{' | '}' => {
                        pattern.push('[');
                        pattern.push(character);
                        pattern.push(']');
                    }
                    _ => pattern.push(character),
                }
            }
            pattern.push('$');
            let pattern = serde_json::to_string(&pattern)
                .context("failed to quote source directory pattern for debugger command")?;
            Ok(format!("-file-list-exec-source-files --dirname {pattern}"))
        }
        None => Ok("-file-list-exec-source-files".to_owned()),
    }
}

/// Read-only view of group membership used during source resolution.
///
/// The resolver only queries groups; depending on this view instead of the
/// write-capable manager keeps mutation structurally out of reach.
pub(crate) trait GroupResolutionView: fmt::Debug + Send + Sync {
    fn group_by_id(&self, group_id: GroupId) -> Option<GroupMeta>;
    fn group_id_by_session(&self, sid: u64) -> Option<GroupId>;
    fn matching_groups(&self, predicate: &dyn Fn(&GroupMeta) -> bool) -> Vec<GroupMeta>;
}

impl GroupResolutionView for RuntimeModel {
    fn group_by_id(&self, group_id: GroupId) -> Option<GroupMeta> {
        RuntimeModel::group_by_id(self, group_id)
    }

    fn group_id_by_session(&self, sid: u64) -> Option<GroupId> {
        RuntimeModel::group_id_by_session(self, sid)
    }

    fn matching_groups(&self, predicate: &dyn Fn(&GroupMeta) -> bool) -> Vec<GroupMeta> {
        RuntimeModel::matching_groups(self, predicate)
    }
}

/// Owns debugger source discovery and its lifecycle.
///
/// The catalog remains synchronous and passive. All debugger I/O, duplicate
/// suppression, concurrency limiting, and background task ownership live here.
pub(crate) struct SourceResolver {
    catalog: Arc<SourceCatalog>,
    groups: Arc<dyn GroupResolutionView>,
    provider: Arc<dyn SourceListingProvider>,
    discovery_gates: DashMap<(GroupId, u64), Arc<AsyncMutex<()>>>,
    background: Mutex<HashMap<u64, JoinHandle<()>>>,
    lifecycle: AsyncRwLock<()>,
    slots: Arc<Semaphore>,
    accepting: AtomicBool,
    policy: SourceResolutionPolicy,
}

impl fmt::Debug for SourceResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceResolver")
            .field("catalog", &self.catalog)
            .field("groups", &self.groups)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl SourceResolver {
    pub(crate) fn new(
        catalog: Arc<SourceCatalog>,
        groups: Arc<dyn GroupResolutionView>,
        executor: CommandExecutor,
        policy: SourceResolutionPolicy,
    ) -> Arc<Self> {
        Self::new_with_provider(
            catalog,
            groups,
            Arc::new(DebuggerSourceListingProvider { executor }),
            policy,
            SOURCE_RESOLUTION_CONCURRENCY,
        )
    }

    fn new_with_provider(
        catalog: Arc<SourceCatalog>,
        groups: Arc<dyn GroupResolutionView>,
        provider: Arc<dyn SourceListingProvider>,
        policy: SourceResolutionPolicy,
        concurrency: usize,
    ) -> Arc<Self> {
        assert!(
            concurrency > 0,
            "source resolution concurrency must be non-zero"
        );
        Arc::new(Self {
            catalog,
            groups,
            provider,
            discovery_gates: DashMap::new(),
            background: Mutex::new(HashMap::new()),
            lifecycle: AsyncRwLock::new(()),
            slots: Arc::new(Semaphore::new(concurrency)),
            accepting: AtomicBool::new(true),
            policy,
        })
    }

    pub(crate) async fn group_ids_for(&self, path: &str) -> Result<HashSet<GroupId>> {
        self.resolve_path(path).await?;
        Ok(self.catalog.group_ids(path))
    }

    pub(crate) async fn groups_for(&self, path: &str) -> Result<Vec<GroupMeta>> {
        let mut groups = self
            .group_ids_for(path)
            .await?
            .into_iter()
            .filter_map(|group_id| self.groups.group_by_id(group_id))
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(GroupMeta::id);
        Ok(groups)
    }

    pub(crate) async fn resolve_path(&self, path: &str) -> Result<()> {
        self.ensure_accepting()?;

        let unresolved = self.groups.matching_groups(&|group| {
            group
                .session_ids()
                .iter()
                .any(|sid| !self.catalog.is_checked(path, *sid))
        });
        if unresolved.is_empty() {
            return Ok(());
        }

        let sessions = unresolved
            .iter()
            .flat_map(|group| {
                group
                    .session_ids()
                    .iter()
                    .copied()
                    .map(|sid| (group.id(), sid))
            })
            .filter(|(_, sid)| !self.catalog.is_checked(path, *sid))
            .collect::<Vec<_>>();
        let jobs = sessions
            .into_iter()
            .map(|(group_id, sid)| self.resolve_session_path(path, group_id, sid));
        let failures = join_all(jobs)
            .await
            .into_iter()
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "source resolution failed for {} session(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    async fn resolve_session_path(&self, path: &str, group_id: GroupId, sid: u64) -> Result<()> {
        let gate = self
            .discovery_gates
            .entry((group_id, sid))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _single_flight = gate.lock().await;

        if self.catalog.is_checked(path, sid) {
            return Ok(());
        }

        let _slot = self
            .slots
            .acquire()
            .await
            .context("source resolver is shutting down")?;
        let sources = self.provider.list_sources(sid, Some(path)).await?;

        let _lifecycle = self.lifecycle.read().await;
        if self.discovery_is_current(group_id, sid, &gate) {
            self.catalog
                .record_path_listing(path, sid, group_id, sources)?;
        }
        Ok(())
    }

    pub(crate) fn cached_sources(&self, sid: u64) -> Vec<String> {
        self.catalog.session_listing(sid)
    }

    pub(crate) fn cached_membership(&self, sid: u64, path: &str) -> Option<bool> {
        self.catalog.membership(path, sid)
    }

    pub(crate) async fn source_membership(
        &self,
        sid: u64,
        path: &str,
        refresh: bool,
    ) -> Result<bool> {
        self.ensure_accepting()?;
        if refresh {
            self.resolve_session(sid, true).await?;
        } else {
            let group = self
                .groups
                .group_id_by_session(sid)
                .ok_or_else(|| anyhow!("session {sid} has no source group"))?;
            self.resolve_session_path(path, group, sid).await?;
        }
        self.catalog
            .membership(path, sid)
            .ok_or_else(|| anyhow!("source discovery was invalidated"))
    }

    pub(crate) async fn resolve_session(&self, sid: u64, refresh: bool) -> Result<Vec<String>> {
        self.ensure_accepting()?;
        let group_id = self
            .groups
            .group_id_by_session(sid)
            .ok_or_else(|| anyhow!("session {sid} has no source group"))?;
        let gate = self
            .discovery_gates
            .entry((group_id, sid))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _single_flight = gate.lock().await;

        if !refresh && self.catalog.has_full_listing(sid) {
            return Ok(self.catalog.session_listing(sid));
        }

        debug!(sid, group_id = %group_id, "resolving complete debugger source listing");
        let _slot = self
            .slots
            .acquire()
            .await
            .context("source resolver is shutting down")?;
        let sources = self.provider.list_sources(sid, None).await?;

        let _lifecycle = self.lifecycle.read().await;
        if self.discovery_is_current(group_id, sid, &gate) {
            self.catalog.record_full_listing(sid, group_id, sources)?;
        }
        Ok(self.catalog.session_listing(sid))
    }

    // A retired session/group removes its gate. In-flight discovery must not
    // publish after that invalidation, even before topology retirement finishes.
    fn discovery_is_current(
        &self,
        group_id: GroupId,
        sid: u64,
        gate: &Arc<AsyncMutex<()>>,
    ) -> bool {
        self.accepting.load(Ordering::Acquire)
            && self.groups.group_id_by_session(sid) == Some(group_id)
            && self
                .discovery_gates
                .get(&(group_id, sid))
                .is_some_and(|current| Arc::ptr_eq(&current, gate))
    }

    pub(crate) fn session_activated(self: &Arc<Self>, sid: u64) {
        if self.policy != SourceResolutionPolicy::Eager || !self.accepting.load(Ordering::Acquire) {
            return;
        }

        let resolver = Arc::clone(self);
        let task = tokio::spawn(async move {
            if let Err(error) = resolver.resolve_session(sid, false).await {
                warn!(sid, error = ?error, "failed to eagerly resolve debugger sources");
            }
        });
        if let Some(previous) = self.background.lock().unwrap().insert(sid, task) {
            previous.abort();
        }
    }

    pub(crate) async fn cancel_session(&self, sid: u64) {
        let task = self.background.lock().unwrap().remove(&sid);
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        let _lifecycle = self.lifecycle.write().await;
        self.catalog.remove_session(sid);
        self.discovery_gates
            .retain(|(_, candidate), _| *candidate != sid);
    }

    pub(crate) async fn remove_group(&self, group_id: GroupId) {
        let _lifecycle = self.lifecycle.write().await;
        self.catalog.remove_group(group_id);
        self.discovery_gates
            .retain(|(candidate, _), _| *candidate != group_id);
    }

    pub(crate) async fn shutdown(&self) {
        if !self.accepting.swap(false, Ordering::AcqRel) {
            return;
        }
        self.slots.close();

        let tasks = {
            let mut background = self.background.lock().unwrap();
            background.drain().map(|(_, task)| task).collect::<Vec<_>>()
        };
        for task in &tasks {
            task.abort();
        }
        let _ = join_all(tasks).await;
    }

    fn ensure_accepting(&self) -> Result<()> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(anyhow!("source resolver is shutting down"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use tokio::sync::Notify;

    use super::*;

    fn source_completion(paths: &[&str]) -> FinishedCmd {
        source_probe_completion(
            if paths.is_empty() {
                "unknown"
            } else {
                "present"
            },
            paths,
        )
    }

    fn source_probe_completion(status: &str, paths: &[&str]) -> FinishedCmd {
        use crate::{
            cmd_flow::ParsedSessionResponse,
            debugger::protocol::{Dict, Value},
        };
        let files = paths
            .iter()
            .map(|path| {
                Value::Dict(Dict::from(HashMap::from([(
                    "fullname".to_owned(),
                    Value::String((*path).to_owned()),
                )])))
            })
            .collect();
        let payload = Dict::from(HashMap::from([
            ("files".to_owned(), Value::List(files)),
            ("source_status".to_owned(), Value::String(status.to_owned())),
        ]));
        FinishedCmd::new(
            None,
            1,
            vec![ParsedSessionResponse::new(
                1,
                "done".to_owned(),
                Some(payload),
            )],
        )
    }

    #[tokio::test]
    async fn exact_positive_source_probe_avoids_catalog_enumeration() {
        let mut commands = Vec::new();
        let paths = discover_source_paths(Some("/src/main.c"), |command| {
            commands.push(command);
            std::future::ready(Ok(source_completion(&["/src/main.c"])))
        })
        .await
        .unwrap();
        assert_eq!(paths, ["/src/main.c"]);
        assert_eq!(commands, [r#"-ddb-probe-source "/src/main.c""#]);
    }

    #[tokio::test]
    async fn proven_absent_source_avoids_catalog_enumeration() {
        let mut commands = Vec::new();
        let paths = discover_source_paths(Some("/src/not_owned.c"), |command| {
            commands.push(command);
            std::future::ready(Ok(source_probe_completion("absent", &[])))
        })
        .await
        .unwrap();
        assert!(paths.is_empty());
        assert_eq!(commands, [r#"-ddb-probe-source "/src/not_owned.c""#]);
    }

    #[tokio::test]
    async fn inconclusive_source_probe_discovers_the_directory_without_caching_a_false_negative() {
        let mut replies = std::collections::VecDeque::from([
            source_completion(&[]),
            source_completion(&["/src/main.c", "/src/other.c"]),
        ]);
        let mut commands = Vec::new();
        let paths = discover_source_paths(Some("/src/main.c"), |command| {
            commands.push(command);
            std::future::ready(Ok(replies.pop_front().expect("unexpected discovery I/O")))
        })
        .await
        .unwrap();
        assert_eq!(paths, ["/src/main.c", "/src/other.c"]);
        assert_eq!(
            commands,
            [
                r#"-ddb-probe-source "/src/main.c""#,
                r#"-file-list-exec-source-files --dirname "^/src$""#
            ]
        );
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn invalid_or_failed_source_probes_do_not_fall_back_or_invent_ownership() {
        use crate::cmd_flow::ParsedSessionResponse;
        for response in [
            Ok(source_completion(&["/foreign/main.c"])),
            Ok(source_completion(&["/src/main.c", "/src/other.c"])),
            Ok(source_probe_completion("absent", &["/src/main.c"])),
            Ok(source_probe_completion("unknown", &["/src/main.c"])),
            Ok(source_probe_completion("present", &[])),
            Ok(source_probe_completion("invalid", &[])),
            Ok(FinishedCmd::new(
                None,
                1,
                vec![ParsedSessionResponse::new(1, "done".to_owned(), None)],
            )),
            Err(anyhow!("debugger transport is unavailable")),
        ] {
            let mut response = Some(response);
            let mut commands = Vec::new();
            assert!(discover_source_paths(Some("/src/main.c"), |command| {
                commands.push(command);
                std::future::ready(
                    response
                        .take()
                        .expect("failed probes must not issue catalog I/O"),
                )
            })
            .await
            .is_err());
            assert_eq!(commands, [r#"-ddb-probe-source "/src/main.c""#]);
        }
    }

    #[tokio::test]
    async fn complete_and_relative_source_queries_do_not_use_absolute_path_probes() {
        for path in [None, Some("main.c")] {
            let mut commands = Vec::new();
            let paths = discover_source_paths(path, |command| {
                commands.push(command);
                std::future::ready(Ok(source_completion(&["/src/main.c"])))
            })
            .await
            .unwrap();
            assert_eq!(paths, ["/src/main.c"]);
            assert_eq!(commands, ["-file-list-exec-source-files"]);
        }
    }

    #[tokio::test]
    #[ignore = "requires cc and real GDB"]
    async fn real_gdb_source_directory_is_literal_and_exact() {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        for directory in [
            "src[x]",
            "src.v1",
            "src+plus",
            "src (copy)",
            r#"src{a}^$|?*\"quote"#,
            "源码",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let requested = fixture.path().join(directory);
            let nested = requested.join("nested");
            let sibling = fixture.path().join(format!("{directory}-other"));
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::create_dir_all(&sibling).unwrap();
            let source = requested.join("requested.c");
            let child = nested.join("child.c");
            let other = sibling.join("other.c");
            let data_only = requested.join("data_only.c");
            std::fs::write(&data_only, "int data_only = 5;\n").unwrap();
            let header = requested.join("types.h");
            std::fs::write(
                &header,
                "typedef struct HeaderOnly { int field; } HeaderOnly;\n",
            )
            .unwrap();
            let unowned = fixture.path().join("not-compiled/requested.c");
            std::fs::create_dir_all(unowned.parent().unwrap()).unwrap();
            std::fs::write(&unowned, "int not_compiled(void) { return 0; }\n").unwrap();
            std::fs::write(
                &source,
                "#include \"types.h\"\nHeaderOnly header_value;\nint child(void); int other(void); int main(void) { return child() + other(); }\n",
            )
            .unwrap();
            std::fs::write(&child, "int child(void) { return 1; }\n").unwrap();
            std::fs::write(&other, "int other(void) { return 2; }\n").unwrap();
            let executable = fixture.path().join("fixture");
            let compilation = std::process::Command::new("cc")
                .args(["-g", "-O0"])
                .args([&source, &child, &other, &data_only])
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(
                compilation.status.success(),
                "{}",
                String::from_utf8_lossy(&compilation.stderr)
            );
            let mut gdb = tokio::process::Command::new("gdb")
                .args(["--nx", "--quiet", "--interpreter=mi2"])
                .arg(&executable)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let command = source_listing_command(Some(requested.to_str().unwrap())).unwrap();
            let runtime =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/gdb_ext/runtime-gdb.py");
            let load = serde_json::to_string(&format!("source {}", runtime.display())).unwrap();
            let probe = serde_json::to_string(source.to_str().unwrap()).unwrap();
            let wrong_probe = serde_json::to_string(unowned.to_str().unwrap()).unwrap();
            let data_probe = serde_json::to_string(data_only.to_str().unwrap()).unwrap();
            let header_probe = serde_json::to_string(header.to_str().unwrap()).unwrap();
            let localized_error_test = "original = gdb.execute_mi\ndef localized_error(*args):\n    raise gdb.error('localized or unrecognized lookup error')\ntry:\n    gdb.execute_mi = localized_error\n    assert ProbeSourceMICommand.invoke(None, ['/missing/file.c']) == {'files': [], 'source_status': 'unknown'}\nfinally:\n    gdb.execute_mi = original\n";
            let localized_error_test = serde_json::to_string(&format!(
                "python exec({})",
                serde_json::to_string(localized_error_test).unwrap()
            ))
            .unwrap();
            let mut input = gdb.stdin.take().unwrap();
            input
                .write_all(
                    format!("0-interpreter-exec console {load}\n1-file-list-exec-source-files\n2{command}\n4-ddb-probe-source {probe}\n5-ddb-probe-source {wrong_probe}\n7-ddb-probe-source {data_probe}\n8-ddb-probe-source {header_probe}\n9-interpreter-exec console {localized_error_test}\n6-gdb-exit\n").as_bytes(),
                )
                .await
                .unwrap();
            drop(input);
            let output = tokio::time::timeout(Duration::from_secs(10), gdb.wait_with_output())
                .await
                .expect("GDB source listing timed out")
                .unwrap();
            assert!(output.status.success());
            let output = String::from_utf8(output.stdout).unwrap();
            let complete = output
                .lines()
                .find(|line| line.starts_with("1^done"))
                .unwrap();
            let filtered = output
                .lines()
                .find(|line| line.starts_with("2^done"))
                .unwrap();
            let decode = |line: &str| {
                use crate::{
                    cmd_flow::{FinishedCmd, ParsedSessionResponse},
                    debugger::gdb::parser::{normalize_dict, GdbParser},
                };
                use gdbmi::parser::{Message, Response};
                let Message::Response(Response::Result {
                    message, payload, ..
                }) = GdbParser::parse(line).unwrap()
                else {
                    panic!("expected a result record");
                };
                assert_eq!(message, "done");
                SourceFiles::decode(&FinishedCmd::new(
                    None,
                    1,
                    vec![ParsedSessionResponse::new(
                        1,
                        message,
                        payload.map(normalize_dict),
                    )],
                ))
                .unwrap()
                .paths
            };
            let complete = decode(complete);
            let filtered = decode(filtered);
            let probe = output.lines().find(|line| line.starts_with("4^")).unwrap();
            let wrong_probe = output.lines().find(|line| line.starts_with("5^")).unwrap();
            assert_eq!(decode(probe), vec![source.to_str().unwrap().to_owned()]);
            assert!(
                decode(wrong_probe).is_empty(),
                "a same-basename file outside the binary must not establish ownership"
            );
            assert!(
                wrong_probe.contains(r#"source_status="absent""#),
                "a native unknown-source lookup must prove absence: {wrong_probe}"
            );
            let data_probe = output.lines().find(|line| line.starts_with("7^")).unwrap();
            assert!(
                !data_probe.contains(r#"source_status="absent""#),
                "a file without executable lines must not be classified absent: {data_probe}"
            );
            assert!(complete
                .iter()
                .any(|path| path == data_only.to_str().unwrap()));
            let header_probe = output.lines().find(|line| line.starts_with("8^")).unwrap();
            assert!(
                !header_probe.contains(r#"source_status="absent""#),
                "a header without executable lines must not be classified absent: {header_probe}"
            );
            assert!(complete.iter().any(|path| path == header.to_str().unwrap()));
            assert!(output.lines().any(|line| line.starts_with("9^done")));
            for path in [&source, &child, &other] {
                assert!(
                    complete
                        .iter()
                        .any(|actual| actual == path.to_str().unwrap()),
                    "fixture source is absent: {path:?}"
                );
            }
            assert!(
                filtered
                    .iter()
                    .any(|actual| actual == source.to_str().unwrap()),
                "literal directory {directory:?} omitted its source: {filtered:?}"
            );
            assert!(
                !filtered
                    .iter()
                    .any(|actual| actual == child.to_str().unwrap()),
                "exact directory {directory:?} included a descendant: {filtered:?}"
            );
            assert!(
                !filtered
                    .iter()
                    .any(|actual| actual == other.to_str().unwrap()),
                "directory filter matched sibling {directory:?}: {filtered:?}"
            );
        }
    }

    #[test]
    fn source_listing_command_quotes_patterns_and_handles_root() {
        assert_eq!(
            source_listing_command(None).unwrap(),
            "-file-list-exec-source-files"
        );
        for directory in ["/", "///"] {
            assert_eq!(
                source_listing_command(Some(directory)).unwrap(),
                r#"-file-list-exec-source-files --dirname "^/$""#
            );
        }
        assert_eq!(
            source_listing_command(Some("/src///")).unwrap(),
            r#"-file-list-exec-source-files --dirname "^/src$""#
        );
        // No path prefix is known for a relative basename. Do not turn that
        // into a root-only query, or treat a leading '-' as an MI option.
        assert_eq!(
            source_listing_command(Some("")).unwrap(),
            "-file-list-exec-source-files"
        );
        assert_eq!(
            source_listing_command(Some("-src")).unwrap(),
            r#"-file-list-exec-source-files --dirname "^-src$""#
        );
        for (directory, expected_pattern) in [
            ("src[x]", r"^/repo/src\[x\]$"),
            ("src.v1", r"^/repo/src\.v1$"),
            ("src+plus", "^/repo/src[+]plus$"),
            ("src (copy)", "^/repo/src [(]copy[)]$"),
            (
                r#"src{a}^$|?*\"quote"#,
                r#"^/repo/src[{]a[}]\^\$[|][?]\*\\"quote$"#,
            ),
            ("源码", "^/repo/源码$"),
        ] {
            let command = source_listing_command(Some(&format!("/repo/{directory}"))).unwrap();
            let argument = command
                .strip_prefix("-file-list-exec-source-files --dirname ")
                .unwrap();
            assert_eq!(
                serde_json::from_str::<String>(argument).unwrap(),
                expected_pattern
            );
        }
    }

    struct InFlightGuard<'a>(&'a AtomicUsize);

    impl Drop for InFlightGuard<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    struct FakeProvider {
        sources: Vec<String>,
        delay: Duration,
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        started: Arc<Notify>,
        release: Option<Arc<Notify>>,
    }

    impl FakeProvider {
        fn new(sources: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                sources,
                delay: Duration::ZERO,
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                started: Arc::new(Notify::new()),
                release: None,
            })
        }

        fn delayed(sources: Vec<String>, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                sources,
                delay,
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                started: Arc::new(Notify::new()),
                release: None,
            })
        }

        fn blocked(sources: Vec<String>, release: Arc<Notify>) -> Arc<Self> {
            Arc::new(Self {
                sources,
                delay: Duration::ZERO,
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                started: Arc::new(Notify::new()),
                release: Some(release),
            })
        }
    }

    #[async_trait]
    impl SourceListingProvider for FakeProvider {
        async fn list_sources(&self, _sid: u64, _path: Option<&str>) -> Result<Vec<String>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let current = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.max_in_flight.fetch_max(current, Ordering::AcqRel);
            let _in_flight = InFlightGuard(&self.in_flight);
            self.started.notify_one();

            if let Some(release) = &self.release {
                release.notified().await;
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(self.sources.clone())
        }
    }

    async fn register_group(model: &Arc<RuntimeModel>, sid: u64, hash: &str) -> GroupId {
        let identity = crate::state::ServiceIdentity::new(hash, format!("service-{sid}"));
        model
            .register_session(sid, &format!("session-{sid}"), Some(identity.clone()))
            .await;
        drop(model.register_service_group(sid, &identity).await);
        model.group_id_by_session(sid).unwrap()
    }

    fn resolver_with(
        groups: Arc<RuntimeModel>,
        provider: Arc<FakeProvider>,
        policy: SourceResolutionPolicy,
        concurrency: usize,
    ) -> (Arc<SourceResolver>, Arc<SourceCatalog>) {
        let catalog = Arc::new(SourceCatalog::new());
        let resolver = SourceResolver::new_with_provider(
            Arc::clone(&catalog),
            groups,
            provider,
            policy,
            concurrency,
        );
        (resolver, catalog)
    }

    #[tokio::test]
    async fn concurrent_path_resolution_is_single_flight() {
        let groups = RuntimeModel::new();
        register_group(&groups, 11, "binary-a").await;
        let provider =
            FakeProvider::delayed(vec!["/src/main.rs".to_owned()], Duration::from_millis(10));
        let (resolver, _) = resolver_with(
            groups,
            Arc::clone(&provider),
            SourceResolutionPolicy::OnDemand,
            8,
        );

        let results = join_all((0..16).map(|_| resolver.group_ids_for("/src/main.rs"))).await;

        assert!(results.iter().all(Result::is_ok));
        assert!(results.into_iter().all(|result| result.unwrap().len() == 1));
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn resolution_concurrency_is_bounded() {
        let groups = RuntimeModel::new();
        for sid in 1..=5 {
            register_group(&groups, sid, &format!("binary-{sid}")).await;
        }
        let provider =
            FakeProvider::delayed(vec!["/src/main.rs".to_owned()], Duration::from_millis(10));
        let (resolver, _) = resolver_with(
            groups,
            Arc::clone(&provider),
            SourceResolutionPolicy::OnDemand,
            2,
        );

        resolver.resolve_path("/src/main.rs").await.unwrap();

        assert_eq!(provider.calls.load(Ordering::Acquire), 5);
        assert_eq!(provider.max_in_flight.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn removed_group_is_not_reintroduced_by_late_resolution() {
        let groups = RuntimeModel::new();
        register_group(&groups, 11, "binary-a").await;
        let group_id = groups.group_id_by_session(11).unwrap();
        let release = Arc::new(Notify::new());
        let provider = FakeProvider::blocked(vec!["/src/main.rs".to_owned()], Arc::clone(&release));
        let started = Arc::clone(&provider.started);
        let (resolver, catalog) = resolver_with(
            Arc::clone(&groups),
            provider,
            SourceResolutionPolicy::OnDemand,
            8,
        );

        let resolution = tokio::spawn({
            let resolver = Arc::clone(&resolver);
            async move { resolver.group_ids_for("/src/main.rs").await }
        });
        started.notified().await;
        drop(groups.begin_session_retirement(11).await.finish().await);
        resolver.remove_group(group_id).await;
        release.notify_one();

        assert!(resolution.await.unwrap().unwrap().is_empty());
        assert!(catalog.group_ids("/src/main.rs").is_empty());
    }

    #[tokio::test]
    async fn eager_resolution_task_is_owned_and_cancellable() {
        let groups = RuntimeModel::new();
        register_group(&groups, 11, "binary-a").await;
        let release = Arc::new(Notify::new());
        let provider = FakeProvider::blocked(vec!["/src/main.rs".to_owned()], release);
        let started = Arc::clone(&provider.started);
        let (resolver, catalog) = resolver_with(
            groups,
            Arc::clone(&provider),
            SourceResolutionPolicy::Eager,
            8,
        );

        resolver.session_activated(11);
        started.notified().await;
        resolver.cancel_session(11).await;

        assert_eq!(provider.in_flight.load(Ordering::Acquire), 0);
        assert!(!catalog.has_full_listing(11));
    }

    #[tokio::test]
    async fn cancelled_session_does_not_publish_late_requested_discovery() {
        let groups = RuntimeModel::new();
        register_group(&groups, 11, "binary-a").await;
        let release = Arc::new(Notify::new());
        let provider = FakeProvider::blocked(vec!["/src/main.rs".to_owned()], Arc::clone(&release));
        let started = Arc::clone(&provider.started);
        let (resolver, catalog) =
            resolver_with(groups, provider, SourceResolutionPolicy::OnDemand, 8);
        let resolution = tokio::spawn({
            let resolver = Arc::clone(&resolver);
            async move { resolver.resolve_session(11, false).await }
        });
        started.notified().await;
        resolver.cancel_session(11).await;
        release.notify_one();
        assert!(resolution.await.unwrap().unwrap().is_empty());
        assert!(!catalog.has_full_listing(11));
    }

    #[tokio::test]
    async fn shutdown_rejects_new_resolution() {
        let groups = RuntimeModel::new();
        register_group(&groups, 11, "binary-a").await;
        let provider = FakeProvider::new(vec!["/src/main.rs".to_owned()]);
        let (resolver, _) = resolver_with(groups, provider, SourceResolutionPolicy::OnDemand, 8);

        resolver.shutdown().await;

        assert!(resolver.group_ids_for("/src/main.rs").await.is_err());
    }
}
