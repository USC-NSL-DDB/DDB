use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use crate::state::GroupId;

#[derive(Debug, Default)]
struct SourceIndex {
    source_groups: HashMap<String, HashSet<GroupId>>,
    checked_groups: HashMap<String, HashSet<GroupId>>,
    loaded_groups: HashSet<GroupId>,
}

#[derive(Debug, Default)]
pub(crate) struct SourceCatalog {
    state: RwLock<SourceIndex>,
}

impl SourceCatalog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn has_full_listing(&self, group_id: GroupId) -> bool {
        self.state.read().unwrap().loaded_groups.contains(&group_id)
    }

    pub(crate) fn is_checked(&self, path: &str, group_id: GroupId) -> bool {
        self.state
            .read()
            .unwrap()
            .checked_groups
            .get(path)
            .is_some_and(|groups| groups.contains(&group_id))
    }

    pub(crate) fn group_ids(&self, path: &str) -> HashSet<GroupId> {
        self.state
            .read()
            .unwrap()
            .source_groups
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn record_full_listing(
        &self,
        group_id: GroupId,
        sources: impl IntoIterator<Item = String>,
    ) -> bool {
        let mut state = self.state.write().unwrap();
        if !state.loaded_groups.insert(group_id) {
            return false;
        }

        for source in sources {
            Self::record_source(&mut state, group_id, source);
        }
        true
    }

    pub(crate) fn record_path_listing(
        &self,
        requested_path: &str,
        group_id: GroupId,
        sources: impl IntoIterator<Item = String>,
    ) {
        let mut state = self.state.write().unwrap();
        state
            .checked_groups
            .entry(requested_path.to_owned())
            .or_default()
            .insert(group_id);

        for source in sources {
            Self::record_source(&mut state, group_id, source);
        }
    }

    pub(crate) fn remove_group(&self, group_id: GroupId) {
        let mut state = self.state.write().unwrap();
        state.loaded_groups.remove(&group_id);
        state.source_groups.retain(|_, groups| {
            groups.remove(&group_id);
            !groups.is_empty()
        });
        state.checked_groups.retain(|_, groups| {
            groups.remove(&group_id);
            !groups.is_empty()
        });
    }

    fn record_source(state: &mut SourceIndex, group_id: GroupId, source: String) {
        state
            .source_groups
            .entry(source.clone())
            .or_default()
            .insert(group_id);
        state
            .checked_groups
            .entry(source)
            .or_default()
            .insert(group_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_listing_is_applied_once() {
        let catalog = SourceCatalog::new();

        assert!(catalog.record_full_listing(
            GroupId::new(7),
            ["/src/main.rs".to_owned(), "/src/lib.rs".to_owned()]
        ));
        assert!(!catalog.record_full_listing(GroupId::new(7), ["/src/ignored.rs".to_owned()]));

        assert_eq!(
            catalog.group_ids("/src/main.rs"),
            HashSet::from([GroupId::new(7)])
        );
        assert!(catalog.group_ids("/src/ignored.rs").is_empty());
    }

    #[test]
    fn empty_path_listing_is_remembered() {
        let catalog = SourceCatalog::new();

        catalog.record_path_listing("/src/missing.rs", GroupId::new(3), []);

        assert!(catalog.is_checked("/src/missing.rs", GroupId::new(3)));
        assert!(catalog.group_ids("/src/missing.rs").is_empty());
    }

    #[test]
    fn removing_group_cleans_every_reverse_index() {
        let catalog = SourceCatalog::new();
        catalog.record_full_listing(GroupId::new(3), ["/src/main.rs".to_owned()]);
        catalog.record_full_listing(GroupId::new(4), ["/src/main.rs".to_owned()]);
        catalog.record_path_listing("/src/missing.rs", GroupId::new(3), []);

        catalog.remove_group(GroupId::new(3));

        assert!(!catalog.has_full_listing(GroupId::new(3)));
        assert!(!catalog.is_checked("/src/main.rs", GroupId::new(3)));
        assert!(!catalog.is_checked("/src/missing.rs", GroupId::new(3)));
        assert_eq!(
            catalog.group_ids("/src/main.rs"),
            HashSet::from([GroupId::new(4)])
        );
    }
}
