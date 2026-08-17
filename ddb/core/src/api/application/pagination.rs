use sha2::{Digest, Sha256};
use uuid::Uuid;

use ddb_api_types::v2::{DdbErrorCode, PageInfo, PageRequest};

use super::ApplicationError;

#[derive(Debug)]
pub(crate) struct Page<T> {
    pub(crate) items: Vec<T>,
    pub(crate) info: PageInfo,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PageWindow {
    pub(crate) offset: usize,
    pub(crate) size: usize,
}

/// Instance-bound and revision-bound continuation token codec.
pub(crate) struct PageCodec {
    default_size: usize,
    max_size: usize,
    secret: [u8; 16],
}

impl PageCodec {
    pub(crate) fn new(default_size: usize, max_size: usize) -> Self {
        assert!(default_size > 0);
        assert!(default_size <= max_size);
        Self {
            default_size,
            max_size,
            secret: *Uuid::new_v4().as_bytes(),
        }
    }

    pub(crate) fn paginate<T>(
        &self,
        collection: &str,
        revision: u64,
        items: Vec<T>,
        request: Option<&PageRequest>,
    ) -> Result<Page<T>, ApplicationError> {
        let requested_size = request.map_or(0, |page| page.page_size) as usize;
        let page_size = if requested_size == 0 {
            self.default_size
        } else {
            requested_size.min(self.max_size)
        };
        let offset = match request.and_then(|page| page.page_token.as_deref()) {
            Some(token) => self.decode(collection, revision, token)?,
            None => 0,
        };
        let total_len = items.len();
        if offset > total_len {
            return Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "page token no longer addresses this collection",
            ));
        }

        let page_items = items
            .into_iter()
            .skip(offset)
            .take(page_size)
            .collect::<Vec<_>>();
        let next_offset = offset.saturating_add(page_items.len());
        let next_page_token =
            (next_offset < total_len).then(|| self.encode(collection, revision, next_offset));
        Ok(Page {
            items: page_items,
            info: PageInfo { next_page_token },
        })
    }

    /// Decodes a revision-bound page request before backend I/O. Callers use
    /// the returned window to request at most `size + 1` records, avoiding an
    /// unbounded materialization merely to discover whether another page
    /// exists.
    pub(crate) fn window(
        &self,
        collection: &str,
        revision: u64,
        request: Option<&PageRequest>,
    ) -> Result<PageWindow, ApplicationError> {
        let requested_size = request.map_or(0, |page| page.page_size) as usize;
        let size = if requested_size == 0 {
            self.default_size
        } else {
            requested_size.min(self.max_size)
        };
        let offset = match request.and_then(|page| page.page_token.as_deref()) {
            Some(token) => self.decode(collection, revision, token)?,
            None => 0,
        };
        Ok(PageWindow { offset, size })
    }

    /// Finishes a bounded backend window. `items` may contain one lookahead
    /// record; that record is never returned and only drives the next token.
    pub(crate) fn finish_window<T>(
        &self,
        collection: &str,
        revision: u64,
        window: PageWindow,
        items: Vec<T>,
    ) -> Page<T> {
        self.finish_window_with_more(collection, revision, window, items, false)
    }

    pub(crate) fn finish_window_with_more<T>(
        &self,
        collection: &str,
        revision: u64,
        window: PageWindow,
        mut items: Vec<T>,
        backend_has_more: bool,
    ) -> Page<T> {
        let has_more = backend_has_more || items.len() > window.size;
        items.truncate(window.size);
        let next_page_token = has_more.then(|| {
            self.encode(
                collection,
                revision,
                window.offset.saturating_add(items.len()),
            )
        });
        Page {
            items,
            info: PageInfo { next_page_token },
        }
    }

    fn encode(&self, collection: &str, revision: u64, offset: usize) -> String {
        let payload = format!("v1:{offset}:{revision}");
        format!("{payload}:{}", self.signature(collection, &payload))
    }

    fn decode(
        &self,
        collection: &str,
        expected_revision: u64,
        token: &str,
    ) -> Result<usize, ApplicationError> {
        let mut parts = token.split(':');
        let (Some("v1"), Some(offset), Some(revision), Some(signature), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(invalid_token());
        };
        let offset = offset.parse::<usize>().map_err(|_| invalid_token())?;
        let revision = revision.parse::<u64>().map_err(|_| invalid_token())?;
        let payload = format!("v1:{offset}:{revision}");
        if signature != self.signature(collection, &payload) {
            return Err(invalid_token());
        }
        if revision != expected_revision {
            return Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "page token expired because the collection changed",
            ));
        }
        Ok(offset)
    }

    fn signature(&self, collection: &str, payload: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(self.secret);
        digest.update([0]);
        digest.update(collection.as_bytes());
        digest.update([0]);
        digest.update(payload.as_bytes());
        digest
            .finalize()
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn invalid_token() -> ApplicationError {
    ApplicationError::invalid(
        "page.page_token",
        "is malformed or belongs to another collection",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_without_duplicates_and_clamps_requested_size() {
        let codec = PageCodec::new(2, 3);
        let first = codec
            .paginate(
                "sessions",
                7,
                vec![1, 2, 3, 4],
                Some(&PageRequest {
                    page_size: 99,
                    page_token: None,
                }),
            )
            .unwrap();
        assert_eq!(first.items, vec![1, 2, 3]);
        let second = codec
            .paginate(
                "sessions",
                7,
                vec![1, 2, 3, 4],
                Some(&PageRequest {
                    page_size: 99,
                    page_token: first.info.next_page_token,
                }),
            )
            .unwrap();
        assert_eq!(second.items, vec![4]);
        assert!(second.info.next_page_token.is_none());
    }

    #[test]
    fn rejects_tampered_cross_collection_and_stale_tokens() {
        let codec = PageCodec::new(1, 2);
        let token = codec
            .paginate("sessions", 7, vec![1, 2], None)
            .unwrap()
            .info
            .next_page_token
            .unwrap();

        let mut tampered = token.clone();
        tampered.push('0');
        assert_eq!(
            codec
                .paginate(
                    "sessions",
                    7,
                    vec![1, 2],
                    Some(&PageRequest {
                        page_size: 1,
                        page_token: Some(tampered),
                    }),
                )
                .unwrap_err()
                .code(),
            DdbErrorCode::InvalidArgument
        );
        assert!(codec
            .paginate(
                "groups",
                7,
                vec![1, 2],
                Some(&PageRequest {
                    page_size: 1,
                    page_token: Some(token.clone()),
                }),
            )
            .is_err());
        assert_eq!(
            codec
                .paginate(
                    "sessions",
                    8,
                    vec![1, 2],
                    Some(&PageRequest {
                        page_size: 1,
                        page_token: Some(token),
                    }),
                )
                .unwrap_err()
                .code(),
            DdbErrorCode::Expired
        );
    }

    #[test]
    fn bounded_windows_use_one_lookahead_record() {
        let codec = PageCodec::new(2, 3);
        let window = codec.window("frames:thread", 11, None).unwrap();
        assert_eq!(window.offset, 0);
        assert_eq!(window.size, 2);
        let first = codec.finish_window("frames:thread", 11, window, vec![1, 2, 3]);
        assert_eq!(first.items, vec![1, 2]);

        let window = codec
            .window(
                "frames:thread",
                11,
                Some(&PageRequest {
                    page_size: 2,
                    page_token: first.info.next_page_token,
                }),
            )
            .unwrap();
        assert_eq!(window.offset, 2);
        let last = codec.finish_window("frames:thread", 11, window, vec![3]);
        assert_eq!(last.items, vec![3]);
        assert!(last.info.next_page_token.is_none());
    }
}
