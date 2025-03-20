use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use futures::future::join_all;

use tracing::debug;
use tracing::error;

use crate::cmd_flow::input::ParsedInputCmd;
use crate::cmd_flow::router::Target;
use crate::cmd_flow::{get_router, NullFormatter};

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
        self.source_map.get(src_path).map(|v| v.value().clone())
    }

    #[cfg(not(feature = "lazy_source_map"))]
    #[inline]
    pub fn resolve_src_to_group_ids(&self, src_path: &str) -> Option<HashSet<GroupId>> {
        self.source_map.get(src_path).map(|v| v.value().clone())
    }

    // This gets a copy of the group meta
    #[cfg(feature = "lazy_source_map")]
    #[inline]
    pub async fn resolve_src_to_groups(&self, src_path: &str) -> Option<Vec<GroupMeta>> {
        if let Err(err) = self.resolve_src_by_path(src_path).await {
            error!("Failed to resolve source path: {:?}", err);
            return None;
        }
        self.source_map.get(src_path).map(|v| {
            v.value()
                .iter()
                .filter_map(|group_id| super::get_group_mgr().get_group(group_id))
                .collect::<Vec<_>>()
        })
    }

    // This gets a copy of the group meta
    #[cfg(not(feature = "lazy_source_map"))]
    #[inline]
    pub fn resolve_src_to_groups(&self, src_path: &str) -> Option<Vec<GroupMeta>> {
        self.source_map.get(src_path).map(|v| {
            v.value()
                .iter()
                .filter_map(|group_id| super::get_group_mgr().get_group(group_id.clone()))
                .collect::<Vec<_>>()
        })
    }

    #[allow(unused)]
    #[inline]
    pub async fn resolve_src_for(&self, sid: u64) -> Result<()> {
        if !self.group_exists_by_sid(sid) {
            debug!("Resolving sources for session: {}", sid);
            // Source is not ready for this session
            // Prepare to retrieve source files
            let cmd: ParsedInputCmd = "-file-list-exec-source-files".try_into().unwrap();
            let (_, cmd) = cmd.to_command(NullFormatter);
            let result = get_router().send_to_ret(Target::Session(sid), cmd).await?;

            let sources = result
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
                .collect::<Vec<_>>();
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
        grp_id: &str,
    ) -> Result<()> {
        let _path = std::path::Path::new(path);
        let dirname = _path.parent().ok_or(anyhow!("Invalid path"))?;
        let dirname = dirname
            .to_str()
            .ok_or(anyhow!("Path cannot be parsed into str representation."))?;

        let cmd: ParsedInputCmd = format!("-file-list-exec-source-files --dirname {}", dirname)
            .try_into()
            .unwrap();
        let (_, cmd) = cmd.to_command(NullFormatter);
        let result = get_router().send_to_ret(Target::Session(sid), cmd).await?;

        let sources = result
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
            .collect::<Vec<_>>();

        if sources.is_empty() {
            // if no source files are found, we still
            // mark the path has been searched for this group
            // So we can skip the search next time.
            self.checked_list
                .entry(path.to_string())
                .or_insert(HashSet::new())
                .insert(grp_id.to_string());
        } else {
            for source_path in sources {
                self.add_source(source_path, grp_id.to_string());
            }
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
        let grps = get_group_mgr().get_all_groups_if(|group_id, g_meta| {
            let sids = g_meta.get_sids();
            // no session is present in the group, skip
            if sids.is_empty() {
                return false;
            }
            // if the group has been resolve for this source path, skip
            if self.is_source_resolved_for_group(path, group_id) {
                debug!("Source already resolved for group: {}", group_id);
                return false;
            }
            true
        });

        let jobs = grps
            .iter()
            .filter_map(|(grp_id, grp)| {
                if let Some(sid) = grp.get_sids().iter().next() {
                    // filter out group if that group has no active sessions
                    Some((grp_id.clone(), *sid))
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
    pub fn group_exists(&self, group_id: &str) -> bool {
        self.added_groups.read().unwrap().contains(group_id)
    }

    #[inline]
    pub fn group_exists_by_sid(&self, sid: u64) -> bool {
        if let Some(group_id) = super::get_group_mgr().get_group_id(sid) {
            return self.group_exists(&group_id);
        }
        false
    }

    #[inline]
    pub fn new_group(&self, group_id: String, sources: Vec<String>) {
        if self.group_exists(&group_id) {
            // fast path: group already exists
            return;
        }

        // slow path
        let mut added_groups = self.added_groups.write().unwrap();
        for source in sources {
            self.add_source(source, group_id.clone());
        }
        added_groups.insert(group_id);
    }

    #[inline]
    pub fn new_group_by_sid(&self, sid: u64, sources: Vec<String>) {
        if let Some(group_id) = super::get_group_mgr().get_group_id(sid) {
            self.new_group(group_id, sources);
        }
    }

    #[inline]
    pub fn add_source(&self, source_path: String, group_id: String) {
        self.checked_list
            .entry(source_path.clone())
            .or_insert(HashSet::new())
            .insert(group_id.clone());
        self.source_map
            .entry(source_path.clone())
            .or_insert(HashSet::new())
            .insert(group_id.clone());
    }

    #[inline]
    pub fn is_source_resolved_for_group(&self, source_path: &str, group_id: &str) -> bool {
        self.checked_list
            .get(source_path)
            .map(|v| v.contains(group_id))
            .unwrap_or(false)
    }
}
