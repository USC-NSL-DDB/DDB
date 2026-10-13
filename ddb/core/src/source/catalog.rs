use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::RwLock,
};

use crate::state::GroupId;

#[derive(Clone, Debug)]
struct SessionSources {
    group_id: GroupId,
    paths: BTreeSet<String>,
    checked_paths: HashSet<String>,
    complete: bool,
}

/// Source discovery is owned by sessions, not path strings or group representatives.
/// Group membership is a projection of those session catalogs.
#[derive(Debug, Default)]
pub(crate) struct SourceCatalog {
    sessions: RwLock<HashMap<u64, SessionSources>>,
}

impl SourceCatalog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn has_full_listing(&self, sid: u64) -> bool {
        self.sessions
            .read()
            .unwrap()
            .get(&sid)
            .is_some_and(|sources| sources.complete)
    }

    pub(crate) fn is_checked(&self, path: &str, sid: u64) -> bool {
        self.sessions
            .read()
            .unwrap()
            .get(&sid)
            .is_some_and(|sources| {
                sources.complete
                    || sources.checked_paths.contains(path)
                    || sources.paths.contains(path)
            })
    }

    pub(crate) fn group_ids(&self, path: &str) -> HashSet<GroupId> {
        self.sessions
            .read()
            .unwrap()
            .values()
            .filter(|sources| sources.paths.contains(path))
            .map(|sources| sources.group_id)
            .collect()
    }

    pub(crate) fn membership(&self, path: &str, sid: u64) -> Option<bool> {
        let sessions = self.sessions.read().unwrap();
        let sources = sessions.get(&sid)?;
        let found = sources.paths.contains(path);
        (found || sources.complete || sources.checked_paths.contains(path)).then_some(found)
    }

    pub(crate) fn session_listing(&self, sid: u64) -> Vec<String> {
        self.sessions
            .read()
            .unwrap()
            .get(&sid)
            .map(|sources| sources.paths.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn record_full_listing(
        &self,
        sid: u64,
        group_id: GroupId,
        sources: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        let replacement = SessionSources {
            group_id,
            paths: sources.into_iter().collect(),
            checked_paths: HashSet::new(),
            complete: true,
        };
        ensure_budget(&sessions, sid, &replacement)?;
        sessions.insert(sid, replacement);
        Ok(())
    }

    pub(crate) fn record_path_listing(
        &self,
        requested_path: &str,
        sid: u64,
        group_id: GroupId,
        paths: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        let mut sources = sessions
            .get(&sid)
            .cloned()
            .unwrap_or_else(|| SessionSources {
                group_id,
                paths: BTreeSet::new(),
                checked_paths: HashSet::new(),
                complete: false,
            });
        if sources.group_id != group_id {
            sources = SessionSources {
                group_id,
                paths: BTreeSet::new(),
                checked_paths: HashSet::new(),
                complete: false,
            };
        }
        sources.checked_paths.insert(requested_path.to_owned());
        sources.paths.extend(paths);
        ensure_budget(&sessions, sid, &sources)?;
        sessions.insert(sid, sources);
        Ok(())
    }

    pub(crate) fn remove_session(&self, sid: u64) {
        self.sessions.write().unwrap().remove(&sid);
    }

    pub(crate) fn remove_group(&self, group_id: GroupId) {
        self.sessions
            .write()
            .unwrap()
            .retain(|_, sources| sources.group_id != group_id);
    }
}

// Count positive and negative records across all sessions. A failed refresh
// leaves the last usable catalog intact. Limits also bound repeated negative
// path probes and accumulated partial-directory discovery.
fn ensure_budget(
    sessions: &HashMap<u64, SessionSources>,
    replaced_sid: u64,
    replacement: &SessionSources,
) -> anyhow::Result<()> {
    let mut entries = 0usize;
    let mut bytes = 0usize;
    for source in sessions
        .iter()
        .filter(|(sid, _)| **sid != replaced_sid)
        .map(|(_, source)| source)
        .chain(std::iter::once(replacement))
    {
        for path in source.paths.iter().chain(source.checked_paths.iter()) {
            entries = entries.saturating_add(1);
            bytes = bytes.saturating_add(path.len() + std::mem::size_of::<String>());
            anyhow::ensure!(
                entries <= 100_000 && bytes <= 16 * 1024 * 1024,
                "cached debugger source catalog exceeds its global resource bound"
            );
        }
    }
    anyhow::ensure!(
        sessions.len() < 4096 || sessions.contains_key(&replaced_sid),
        "cached debugger source session limit reached"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_distinguishes_unknown_negative_and_positive_and_retires_cache() {
        let catalog = SourceCatalog::new();
        let group = GroupId::new(7);
        assert_eq!(catalog.membership("/main.rs", 1), None);
        catalog
            .record_path_listing("/missing.rs", 1, group, ["/main.rs".into()])
            .unwrap();
        assert_eq!(catalog.membership("/main.rs", 1), Some(true));
        assert_eq!(catalog.membership("/missing.rs", 1), Some(false));
        assert_eq!(catalog.membership("/unknown.rs", 1), None);
        assert_eq!(catalog.membership("main.rs", 1), None);
        catalog
            .record_full_listing(1, group, ["/new.rs".into()])
            .unwrap();
        assert_eq!(catalog.membership("/main.rs", 1), Some(false));
        assert_eq!(catalog.membership("/unknown.rs", 1), Some(false));
        catalog.remove_session(1);
        assert_eq!(catalog.membership("/new.rs", 1), None);
    }

    #[test]
    fn catalog_budget_is_global_and_failed_updates_preserve_existing_entries() {
        let catalog = SourceCatalog::new();
        let group = GroupId::new(7);
        catalog
            .record_full_listing(
                1,
                group,
                (0..60_000).map(|index| format!("/source/{index}")),
            )
            .unwrap();
        assert!(catalog
            .record_full_listing(
                2,
                group,
                (0..40_001).map(|index| format!("/source/{index}"))
            )
            .is_err());
        assert_eq!(catalog.session_listing(1).len(), 60_000);
        assert!(catalog.session_listing(2).is_empty());
        assert!(catalog
            .record_path_listing(&"x".repeat(16 * 1024 * 1024), 1, group, [])
            .is_err());
        assert_eq!(catalog.session_listing(1).len(), 60_000);
    }

    #[test]
    fn sessions_in_one_group_keep_distinct_source_catalogs() {
        let catalog = SourceCatalog::new();
        let group = GroupId::new(7);
        catalog
            .record_full_listing(1, group, ["/host-a/main.rs".into()])
            .unwrap();
        catalog
            .record_full_listing(2, group, ["/host-b/main.rs".into()])
            .unwrap();
        assert_eq!(catalog.session_listing(1), ["/host-a/main.rs"]);
        assert_eq!(catalog.session_listing(2), ["/host-b/main.rs"]);
        assert_eq!(catalog.group_ids("/host-b/main.rs"), HashSet::from([group]));
    }

    #[test]
    fn refreshing_a_session_removes_obsolete_source_ownership() {
        let catalog = SourceCatalog::new();
        let group = GroupId::new(7);
        catalog
            .record_full_listing(1, group, ["/old.rs".into()])
            .unwrap();
        catalog
            .record_full_listing(1, group, ["/new.rs".into()])
            .unwrap();
        assert!(catalog.group_ids("/old.rs").is_empty());
        assert_eq!(catalog.session_listing(1), ["/new.rs"]);
    }

    #[test]
    fn negative_lookups_and_cleanup_are_session_scoped() {
        let catalog = SourceCatalog::new();
        let group = GroupId::new(7);
        catalog
            .record_path_listing("/missing.rs", 1, group, [])
            .unwrap();
        assert!(catalog.is_checked("/missing.rs", 1));
        assert!(!catalog.is_checked("/missing.rs", 2));
        catalog
            .record_full_listing(2, group, ["/main.rs".into()])
            .unwrap();
        catalog.remove_session(1);
        assert!(!catalog.is_checked("/missing.rs", 1));
        assert!(catalog.has_full_listing(2));
        catalog.remove_group(group);
        assert!(catalog.session_listing(2).is_empty());
        assert!(catalog.group_ids("/main.rs").is_empty());
    }
}
