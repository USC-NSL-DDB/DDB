use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use ddb_api_types::wkt::Timestamp;
use sha2::{Digest, Sha256};

use super::{timestamp_now, ApplicationError, ResourceIdKind};

#[derive(Clone)]
pub(crate) struct ResourceMetadata {
    pub(crate) created_at: Timestamp,
    pub(crate) revision: u64,
}

struct CatalogState {
    entries: HashMap<(ResourceIdKind, String), ResourceMetadata>,
    fingerprints: HashMap<(ResourceIdKind, String), [u8; 32]>,
    key_bytes: usize,
}

/// Bounded metadata that belongs to the public projection rather than the
/// debugger domain model.
pub(crate) struct ResourceCatalog {
    max_entries: usize,
    max_key_bytes: usize,
    max_total_key_bytes: usize,
    state: Mutex<CatalogState>,
}

impl ResourceCatalog {
    pub(crate) fn new(
        max_entries: usize,
        max_key_bytes: usize,
        max_total_key_bytes: usize,
    ) -> Self {
        assert!(max_entries > 0);
        assert!(max_key_bytes > 0);
        assert!(max_total_key_bytes >= max_key_bytes);
        Self {
            max_entries,
            max_key_bytes,
            max_total_key_bytes,
            state: Mutex::new(CatalogState {
                entries: HashMap::new(),
                fingerprints: HashMap::new(),
                key_bytes: 0,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, CatalogState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn observe(
        &self,
        kind: ResourceIdKind,
        internal_id: impl ToString,
    ) -> Result<ResourceMetadata, ApplicationError> {
        let internal_id = internal_id.to_string();
        self.validate_key(&internal_id)?;
        let mut state = self.state();
        if let Some(metadata) = state.entries.get(&(kind, internal_id.clone())) {
            return Ok(metadata.clone());
        }
        if state.entries.len() >= self.max_entries
            || state.key_bytes.saturating_add(internal_id.len()) > self.max_total_key_bytes
        {
            return Err(ApplicationError::resource_exhausted(
                "public resource metadata capacity is exhausted",
            ));
        }
        let metadata = ResourceMetadata {
            created_at: timestamp_now(),
            revision: 1,
        };
        state.key_bytes += internal_id.len();
        state.entries.insert((kind, internal_id), metadata.clone());
        Ok(metadata)
    }

    pub(crate) fn bump(
        &self,
        kind: ResourceIdKind,
        internal_id: impl ToString,
    ) -> Result<ResourceMetadata, ApplicationError> {
        let internal_id = internal_id.to_string();
        let mut metadata = self.observe(kind, &internal_id)?;
        let mut state = self.state();
        let stored = state
            .entries
            .get_mut(&(kind, internal_id))
            .expect("observe must insert resource metadata");
        stored.revision = stored.revision.checked_add(1).ok_or_else(|| {
            ApplicationError::new(
                ddb_api_types::v2::DdbErrorCode::Internal,
                "resource revision is exhausted",
            )
        })?;
        metadata.revision = stored.revision;
        Ok(metadata)
    }

    /// Observes a stable resource projection and advances its public revision
    /// exactly once when the projection fingerprint changes.
    pub(crate) fn observe_versioned(
        &self,
        kind: ResourceIdKind,
        internal_id: impl ToString,
        version: &[u8],
    ) -> Result<ResourceMetadata, ApplicationError> {
        let internal_id = internal_id.to_string();
        let mut metadata = self.observe(kind, &internal_id)?;
        let fingerprint: [u8; 32] = Sha256::digest(version).into();
        let key = (kind, internal_id);
        let mut state = self.state();
        match state.fingerprints.get(&key) {
            Some(previous) if previous == &fingerprint => {}
            Some(_) => {
                let stored = state
                    .entries
                    .get_mut(&key)
                    .expect("observe must insert resource metadata");
                stored.revision = stored.revision.checked_add(1).ok_or_else(|| {
                    ApplicationError::new(
                        ddb_api_types::v2::DdbErrorCode::Internal,
                        "resource revision is exhausted",
                    )
                })?;
                metadata.revision = stored.revision;
                state.fingerprints.insert(key, fingerprint);
            }
            None => {
                state.fingerprints.insert(key, fingerprint);
            }
        }
        Ok(metadata)
    }

    #[cfg(test)]
    fn remove(&self, kind: ResourceIdKind, internal_id: impl ToString) {
        let internal_id = internal_id.to_string();
        let mut state = self.state();
        if state.entries.remove(&(kind, internal_id.clone())).is_some() {
            state.key_bytes = state.key_bytes.saturating_sub(internal_id.len());
        }
        state.fingerprints.remove(&(kind, internal_id));
    }

    fn validate_key(&self, key: &str) -> Result<(), ApplicationError> {
        if key.is_empty() || key.len() > self.max_key_bytes {
            return Err(ApplicationError::resource_exhausted(
                "internal resource identity exceeds the public projection bound",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_stable_revisioned_and_bounded() {
        let catalog = ResourceCatalog::new(2, 8, 8);
        let first = catalog.observe(ResourceIdKind::Session, "1").unwrap();
        assert_eq!(
            catalog
                .observe(ResourceIdKind::Session, "1")
                .unwrap()
                .revision,
            first.revision
        );
        assert_eq!(
            catalog.bump(ResourceIdKind::Session, "1").unwrap().revision,
            2
        );
        catalog.observe(ResourceIdKind::Group, "22").unwrap();
        assert!(catalog.observe(ResourceIdKind::Thread, "333").is_err());
        catalog.remove(ResourceIdKind::Group, "22");
        assert!(catalog.observe(ResourceIdKind::Thread, "333").is_ok());
    }

    #[test]
    fn versioned_observation_bumps_only_when_content_changes() {
        let catalog = ResourceCatalog::new(2, 16, 32);
        assert_eq!(
            catalog
                .observe_versioned(ResourceIdKind::Selection, "current", b"a")
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            catalog
                .observe_versioned(ResourceIdKind::Selection, "current", b"a")
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            catalog
                .observe_versioned(ResourceIdKind::Selection, "current", b"b")
                .unwrap()
                .revision,
            2
        );
    }
}
