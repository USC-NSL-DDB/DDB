use std::{
    collections::{HashMap, HashSet},
    fmt,
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
    cmd_flow::{api::CommandExecutor, decoder::SourceFiles, router::Target},
    state::{GroupId, GroupMeta, GroupMgr},
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
    async fn list_sources(&self, sid: u64, dirname: Option<&str>) -> Result<Vec<String>>;
}

struct DebuggerSourceListingProvider {
    executor: CommandExecutor,
}

#[async_trait]
impl SourceListingProvider for DebuggerSourceListingProvider {
    async fn list_sources(&self, sid: u64, dirname: Option<&str>) -> Result<Vec<String>> {
        let command = match dirname {
            Some(dirname) => {
                let dirname = serde_json::to_string(dirname)
                    .context("failed to quote source directory for debugger command")?;
                format!("-file-list-exec-source-files --dirname {dirname}")
            }
            None => "-file-list-exec-source-files".to_owned(),
        };
        let completion = self
            .executor
            .execute(&command, Target::Session(sid))
            .await
            .with_context(|| format!("failed to list sources from session {sid}"))?;
        Ok(SourceFiles::decode(&completion)?.paths)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ResolutionKey {
    group_id: GroupId,
    path: String,
}

/// Owns debugger source discovery and its lifecycle.
///
/// The catalog remains synchronous and passive. All debugger I/O, duplicate
/// suppression, concurrency limiting, and background task ownership live here.
pub(crate) struct SourceResolver {
    catalog: Arc<SourceCatalog>,
    groups: Arc<GroupMgr>,
    provider: Arc<dyn SourceListingProvider>,
    path_gates: DashMap<ResolutionKey, Arc<AsyncMutex<()>>>,
    full_gates: DashMap<GroupId, Arc<AsyncMutex<()>>>,
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
        groups: Arc<GroupMgr>,
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
        groups: Arc<GroupMgr>,
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
            path_gates: DashMap::new(),
            full_gates: DashMap::new(),
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

        let unresolved = self.groups.matching_groups(|group| {
            !group.session_ids().is_empty() && !self.catalog.is_checked(path, group.id())
        });
        if unresolved.is_empty() {
            return Ok(());
        }

        let dirname = Path::new(path)
            .parent()
            .ok_or_else(|| anyhow!("source path has no parent directory: {path}"))?
            .to_str()
            .ok_or_else(|| anyhow!("source path is not valid UTF-8: {path}"))?
            .to_owned();

        let jobs = unresolved.into_iter().filter_map(|group| {
            let sid = group.session_ids().iter().copied().min()?;
            Some(self.resolve_group_path(path, &dirname, group.id(), sid))
        });
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
                "source resolution failed for {} group(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    async fn resolve_group_path(
        &self,
        path: &str,
        dirname: &str,
        group_id: GroupId,
        sid: u64,
    ) -> Result<()> {
        let key = ResolutionKey {
            group_id,
            path: path.to_owned(),
        };
        let gate = self
            .path_gates
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _single_flight = gate.lock().await;

        if self.catalog.is_checked(path, group_id) {
            return Ok(());
        }

        let _slot = self
            .slots
            .acquire()
            .await
            .context("source resolver is shutting down")?;
        let sources = self.provider.list_sources(sid, Some(dirname)).await?;

        let _lifecycle = self.lifecycle.read().await;
        if self.groups.group_by_id(group_id).is_some() {
            self.catalog.record_path_listing(path, group_id, sources);
        }
        Ok(())
    }

    async fn resolve_session(&self, sid: u64) -> Result<()> {
        self.ensure_accepting()?;
        let group_id = self
            .groups
            .group_id_by_session(sid)
            .ok_or_else(|| anyhow!("session {sid} has no source group"))?;
        let gate = self
            .full_gates
            .entry(group_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _single_flight = gate.lock().await;

        if self.catalog.has_full_listing(group_id) {
            return Ok(());
        }

        debug!(sid, group_id, "resolving complete debugger source listing");
        let _slot = self
            .slots
            .acquire()
            .await
            .context("source resolver is shutting down")?;
        let sources = self.provider.list_sources(sid, None).await?;

        let _lifecycle = self.lifecycle.read().await;
        if self.groups.group_by_id(group_id).is_some() {
            self.catalog.record_full_listing(group_id, sources);
        }
        Ok(())
    }

    pub(crate) fn session_activated(self: &Arc<Self>, sid: u64) {
        if self.policy != SourceResolutionPolicy::Eager || !self.accepting.load(Ordering::Acquire) {
            return;
        }

        let resolver = Arc::clone(self);
        let task = tokio::spawn(async move {
            if let Err(error) = resolver.resolve_session(sid).await {
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
    }

    pub(crate) async fn remove_group(&self, group_id: GroupId) {
        let _lifecycle = self.lifecycle.write().await;
        self.catalog.remove_group(group_id);
        self.path_gates.retain(|key, _| key.group_id != group_id);
        self.full_gates.remove(&group_id);
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
        async fn list_sources(&self, _sid: u64, _dirname: Option<&str>) -> Result<Vec<String>> {
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

    fn resolver_with(
        groups: Arc<GroupMgr>,
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
        let groups = Arc::new(GroupMgr::new());
        groups.register_session("binary-a", "service-a".to_owned(), 11);
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
        let groups = Arc::new(GroupMgr::new());
        for sid in 1..=5 {
            groups.register_session(&format!("binary-{sid}"), format!("service-{sid}"), sid);
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
        let groups = Arc::new(GroupMgr::new());
        groups.register_session("binary-a", "service-a".to_owned(), 11);
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
        groups.remove_session(11);
        resolver.remove_group(group_id).await;
        release.notify_one();

        assert!(resolution.await.unwrap().unwrap().is_empty());
        assert!(catalog.group_ids("/src/main.rs").is_empty());
    }

    #[tokio::test]
    async fn eager_resolution_task_is_owned_and_cancellable() {
        let groups = Arc::new(GroupMgr::new());
        groups.register_session("binary-a", "service-a".to_owned(), 11);
        let group_id = groups.group_id_by_session(11).unwrap();
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
        assert!(!catalog.has_full_listing(group_id));
    }

    #[tokio::test]
    async fn shutdown_rejects_new_resolution() {
        let groups = Arc::new(GroupMgr::new());
        groups.register_session("binary-a", "service-a".to_owned(), 11);
        let provider = FakeProvider::new(vec!["/src/main.rs".to_owned()]);
        let (resolver, _) = resolver_with(groups, provider, SourceResolutionPolicy::OnDemand, 8);

        resolver.shutdown().await;

        assert!(resolver.group_ids_for("/src/main.rs").await.is_err());
    }
}
