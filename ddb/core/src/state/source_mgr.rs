use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use futures::future::join_all;

use tracing::debug;
use tracing::error;

use crate::cmd_flow::api;
use crate::cmd_flow::FinishedCmd;

use super::group_mgr::GroupId;
use super::{get_group_mgr, get_source_mgr, GroupMeta};

pub struct SourceMgr {
    // maps from source file path to corresponding session groups (a set)
    // Note: one source file can be used by multiple sessions (processes)
    source_map: DashMap<String, HashSet<GroupId>>,

    // expect the update to be infrequent
    added_groups: RwLock<HashSet<GroupId>>,

    // maps from source file path to checked binary groups.
    // If the binary group has the group, we can skip resolving the source file.
    // If not, we need to resolve the source file.
    checked_list: DashMap<String, HashSet<GroupId>>,
}

impl Debug for SourceMgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceMgr")
            .field("source_map", &self.source_map)
            .field("added_groups", &self.added_groups)
            .field("checked_list", &self.checked_list)
            .finish()
    }
}

impl SourceMgr {
    pub fn new() -> Self {
        Self {
            source_map: DashMap::new(),
            added_groups: RwLock::new(HashSet::new()),
            checked_list: DashMap::new(),
        }
    }

    #[cfg(feature = "lazy_source_map")]
    #[inline]
    pub async fn resolve_src_to_group_ids(&self, src_path: &str) -> Option<HashSet<GroupId>> {
        if let Err(err) = self.resolve_src_by_path(src_path).await {
            error!("Failed to resolve source path: {:?}", err);
            return None;
        }
        self.source_group_ids(src_path)
    }

    #[cfg(not(feature = "lazy_source_map"))]
    #[inline]
    pub fn resolve_src_to_group_ids(&self, src_path: &str) -> Option<HashSet<GroupId>> {
        self.source_group_ids(src_path)
    }

    // This gets a copy of the group meta
    #[cfg(feature = "lazy_source_map")]
    #[inline]
    pub async fn resolve_src_to_groups(&self, src_path: &str) -> Option<Vec<GroupMeta>> {
        if let Err(err) = self.resolve_src_by_path(src_path).await {
            error!("Failed to resolve source path: {:?}", err);
            return None;
        }
        self.source_groups(src_path)
    }

    // This gets a copy of the group meta
    #[cfg(not(feature = "lazy_source_map"))]
    #[inline]
    pub fn resolve_src_to_groups(&self, src_path: &str) -> Option<Vec<GroupMeta>> {
        self.source_groups(src_path)
    }

    #[allow(unused)]
    #[inline]
    pub async fn resolve_src_for(&self, sid: u64) -> Result<()> {
        if !self.group_exists_by_sid(sid) {
            debug!("Resolving sources for session: {}", sid);
            // Source is not ready for this session
            // Prepare to retrieve source files
            let result = api::send_and_return("-file-list-exec-source-files")
                .unwrap()
                .to(api::Target::Session(sid))
                .await?;

            let sources = Self::extract_source_paths(&result)?;
            debug!("Resolved sources for session: {}", sid);
            self.new_group_by_sid(sid, sources);
        } else {
            debug!("Sources already resolved for session: {}", sid);
        }
        Ok(())
    }

    #[inline]
    pub async fn resolve_src_path_by_dirname_from(
        &self,
        path: &str,
        sid: u64,
        grp_hash: &str,
    ) -> Result<()> {
        let _path = std::path::Path::new(path);
        let dirname = _path.parent().ok_or(anyhow!("Invalid path"))?;
        let dirname = dirname
            .to_str()
            .ok_or(anyhow!("Path cannot be parsed into str representation."))?;

        let result = api::send_and_return(&format!(
            "-file-list-exec-source-files --dirname {}",
            dirname
        ))
        .unwrap()
        .to(api::Target::Session(sid))
        .await?;

        let sources = Self::extract_source_paths(&result)?;

        let grp_id = get_group_mgr()
            .group_id_by_hash(grp_hash)
            .ok_or(anyhow!("Group ID not found for group hash: {}", grp_hash))?;
        if sources.is_empty() {
            // if no source files are found, we still
            // mark the path has been searched for this group
            // So we can skip the search next time.
            self.mark_source_checked(path, grp_id);
        } else {
            self.add_sources(sources, grp_id);
        }
        Ok(())
    }

    #[inline]
    pub async fn resolve_src_by_path(&self, path: &str) -> Result<()> {
        // Given a source file path,
        // - Get all existing groups
        // - Check the `checked_list` to filter out all checked groups
        // - For the remaining groups, resolve the source file
        // - Update the `checked_list` correspondingly
        let grps = get_group_mgr().matching_groups(|group| {
            let sids = group.session_ids();
            // no session is present in the group, skip
            if sids.is_empty() {
                return false;
            }
            // if the group has been resolve for this source path, skip
            if self.is_source_resolved_for_group(path, group.id()) {
                debug!(
                    "Source already resolved for group: id={}, hash={}",
                    group.id(),
                    group.hash()
                );
                return false;
            }
            true
        });

        let jobs = grps
            .into_iter()
            .filter_map(|group| {
                if let Some(sid) = group.session_ids().iter().next() {
                    // filter out group if that group has no active sessions
                    Some((group.hash().clone(), *sid))
                } else {
                    None
                }
            })
            .map(|(grp_id, sid)| {
                let grp_id = grp_id.clone();
                let path = path.to_string();
                tokio::spawn(async move {
                    get_source_mgr()
                        .resolve_src_path_by_dirname_from(&path, sid, &grp_id)
                        .await
                })
            })
            .collect::<Vec<_>>();

        let rs = join_all(jobs).await;
        for r in rs {
            match r {
                Ok(_) => {}
                Err(e) => {
                    debug!("Failed to resolve source path: {:?}", e);
                }
            }
        }
        Ok(())
    }

    #[inline]
    pub fn group_exists(&self, group_id: u64) -> bool {
        self.added_groups.read().unwrap().contains(&group_id)
    }

    #[inline]
    pub fn group_exists_by_sid(&self, sid: u64) -> bool {
        self.group_id_by_session(sid)
            .is_some_and(|group_id| self.group_exists(group_id))
    }

    #[inline]
    pub fn new_group(&self, group_id: u64, sources: Vec<String>) {
        if self.group_exists(group_id) {
            // fast path: group already exists
            return;
        }

        // slow path
        self.add_sources(sources, group_id);
        self.added_groups.write().unwrap().insert(group_id);
    }

    #[inline]
    pub fn new_group_by_sid(&self, sid: u64, sources: Vec<String>) {
        if let Some(group_id) = self.group_id_by_session(sid) {
            self.new_group(group_id, sources);
        }
    }

    #[inline]
    pub fn add_source(&self, source_path: String, group_id: u64) {
        self.mark_source_checked(&source_path, group_id);
        self.source_map
            .entry(source_path)
            .or_insert(HashSet::new())
            .insert(group_id);
    }

    #[inline]
    pub fn is_source_resolved_for_group(&self, source_path: &str, group_id: u64) -> bool {
        self.checked_list
            .get(source_path)
            .map(|v| v.contains(&group_id))
            .unwrap_or(false)
    }

    #[inline]
    fn source_group_ids(&self, src_path: &str) -> Option<HashSet<GroupId>> {
        self.source_map.get(src_path).map(|v| v.value().clone())
    }

    #[inline]
    fn source_groups(&self, src_path: &str) -> Option<Vec<GroupMeta>> {
        self.source_group_ids(src_path).map(|group_ids| {
            group_ids
                .into_iter()
                .filter_map(|group_id| get_group_mgr().group_by_id(group_id))
                .collect()
        })
    }

    #[inline]
    fn group_id_by_session(&self, sid: u64) -> Option<GroupId> {
        get_group_mgr().group_id_by_session(sid)
    }

    #[inline]
    fn mark_source_checked(&self, source_path: &str, group_id: GroupId) {
        self.checked_list
            .entry(source_path.to_string())
            .or_insert(HashSet::new())
            .insert(group_id);
    }

    #[inline]
    fn add_sources<I>(&self, sources: I, group_id: GroupId)
    where
        I: IntoIterator<Item = String>,
    {
        for source in sources {
            self.add_source(source, group_id);
        }
    }

    fn extract_source_paths(result: &FinishedCmd) -> Result<Vec<String>> {
        Ok(result
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .ok_or(anyhow!("No payload found in response."))?
            .get("files")
            .ok_or(anyhow!("No files found in response."))?
            .expect_list_ref()?
            .iter()
            .filter_map(|f_dict| {
                let f_dict = f_dict.expect_dict_ref().unwrap();
                // If gdb cannot find the source files for some reason,
                // it will not have a "fullname" field.
                // In this case, we will skip the source file.
                f_dict
                    .get("fullname")
                    .map(|f| f.expect_string_ref().unwrap().to_string())
            })
            .collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_source_marks_group_as_checked_and_resolved() {
        let mgr = SourceMgr::new();

        mgr.add_source("/tmp/main.rs".to_string(), 7);

        assert!(mgr.is_source_resolved_for_group("/tmp/main.rs", 7));
        assert_eq!(
            mgr.source_group_ids("/tmp/main.rs"),
            Some(HashSet::from([7]))
        );
    }

    #[test]
    fn new_group_registers_all_sources_once() {
        let mgr = SourceMgr::new();

        mgr.new_group(9, vec!["a.rs".to_string(), "b.rs".to_string()]);
        mgr.new_group(9, vec!["c.rs".to_string()]);

        assert!(mgr.group_exists(9));
        assert_eq!(mgr.source_group_ids("a.rs"), Some(HashSet::from([9])));
        assert_eq!(mgr.source_group_ids("b.rs"), Some(HashSet::from([9])));
        assert!(mgr.source_group_ids("c.rs").is_none());
    }
}
