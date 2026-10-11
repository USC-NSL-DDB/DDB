//! Confined directory browsing and bounded, versioned source reads.
//!
//! A root is an explicitly granted directory capability. Opening the root is
//! the only ambient filesystem operation. Every later lookup is relative to
//! that open directory, including symlink resolution. Callers should run these
//! synchronous operations on a bounded blocking executor.

use std::{
    collections::VecDeque,
    fs::{File, Metadata},
    io::{self, Read},
    path::{Component, Path},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};

use cap_std::{ambient_authority, fs::Dir};
use sha2::{Digest, Sha256};

mod search;
#[cfg(test)]
mod tests;

pub use search::{SearchMatch, SearchMode, SearchQuery};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid source search: {0}")]
    InvalidSearch(&'static str),
    #[error("source paths must be relative to their root and cannot contain parent traversal")]
    InvalidPath,
    #[error("source resource exceeds the configured {0} limit")]
    Limit(&'static str),
    #[error("source resource is not a regular UTF-8 text file")]
    NotText,
    #[error("source changed during the read; retry with a fresh revision")]
    Changed,
    #[error("source filesystem operation failed: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Limits apply before allocation or reading whenever the filesystem permits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_file_bytes: usize,
    pub max_directory_entries: usize,
    pub max_directory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024,
            max_directory_entries: 100_000,
            max_directory_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EntryKind {
    Directory,
    File,
    /// Links remain visible, but must be resolved through the root to open.
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub relative_path: String,
    pub kind: EntryKind,
}

/// A bounded, sorted, nonrecursive directory snapshot. A changed entry set has
/// a different revision; callers can bind pagination tokens to this revision.
#[derive(Clone, Debug)]
pub struct Directory {
    pub entries: Vec<Entry>,
    pub revision: String,
    pub unsupported_names: usize,
}

#[derive(Clone, Debug)]
pub struct Window {
    pub content: String,
    pub start_line: u32,
    pub line_count: u32,
    pub total_lines: u32,
    pub has_more: bool,
    pub content_hash: String,
}

/// Immutable file contents and their line index. Repeated windows do not read,
/// split, or hash the entire file again. Ownership of this value pins that
/// version, even if the filesystem subsequently changes.
#[derive(Debug)]
pub struct Document {
    text: String,
    starts: Vec<usize>,
    hash: String,
}

impl Document {
    pub fn content_hash(&self) -> &str {
        &self.hash
    }

    pub fn total_lines(&self) -> u32 {
        self.starts.len() as u32
    }

    /// Lines are one-based. Beyond-EOF requests return an empty window with
    /// the actual total line count. A final newline terminates the last line;
    /// it does not create a phantom line. CRLF is normalized in returned text.
    pub fn window(&self, start_line: u32, max_lines: u32) -> Result<Window> {
        if start_line == 0 || max_lines == 0 {
            return Err(Error::Limit("nonzero source window"));
        }
        let start = (start_line - 1) as usize;
        let end = start
            .saturating_add(max_lines as usize)
            .min(self.starts.len());
        let mut content = String::new();
        for index in start..end {
            if index > start {
                content.push('\n');
            }
            let end = self
                .starts
                .get(index + 1)
                .copied()
                .unwrap_or(self.text.len());
            let line = &self.text[self.starts[index]..end];
            let line = match line.strip_suffix('\n') {
                Some(line) => line.strip_suffix('\r').unwrap_or(line),
                None => line,
            };
            content.push_str(line);
        }
        Ok(Window {
            content,
            start_line,
            line_count: end.saturating_sub(start) as u32,
            total_lines: self.total_lines(),
            has_more: end < self.starts.len(),
            content_hash: self.hash.clone(),
        })
    }

    fn bytes(&self) -> usize {
        self.text.capacity()
            + self.starts.capacity() * std::mem::size_of::<usize>()
            + self.hash.capacity()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileVersion {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileVersion {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

struct Cached {
    root_id: u64,
    path: String,
    version: FileVersion,
    document: Arc<Document>,
}

#[derive(Default)]
struct Cache {
    files: VecDeque<Cached>,
    bytes: usize,
    directories: VecDeque<CachedDirectory>,
    directory_bytes: usize,
}

struct CachedDirectory {
    created: Instant,
    root_id: u64,
    path: String,
    include_hidden: bool,
    version: FileVersion,
    listing: Arc<Directory>,
    bytes: usize,
}

/// Shared bounded LRUs of immutable documents and directory snapshots.
/// Content and directory snapshots each have the configured byte/count budget.
/// Root identity is part of every
/// cache key; sharing the budget never grants access to another root's files.
pub struct SourceCache {
    state: Mutex<Cache>,
    max_bytes: usize,
    max_files: usize,
    next_root_id: AtomicU64,
}

impl SourceCache {
    pub fn new(max_bytes: usize, max_files: usize) -> Self {
        Self {
            state: Mutex::new(Cache::default()),
            max_bytes,
            max_files,
            next_root_id: AtomicU64::new(1),
        }
    }
}

impl Default for SourceCache {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024, 32)
    }
}

/// An open root pins the granted directory even if its original path is
/// renamed or replaced. Separate roots never share path-keyed cache entries.
pub struct SourceRoot {
    directory: Dir,
    limits: Limits,
    cache: Arc<SourceCache>,
    root_id: u64,
}

impl SourceRoot {
    pub fn open(path: impl AsRef<Path>, limits: Limits) -> Result<Self> {
        Self::open_with_cache(path, limits, Arc::new(SourceCache::default()))
    }

    pub fn open_with_cache(
        path: impl AsRef<Path>,
        limits: Limits,
        cache: Arc<SourceCache>,
    ) -> Result<Self> {
        if limits.max_file_bytes == 0 || limits.max_file_bytes > u32::MAX as usize {
            return Err(Error::Limit("file byte"));
        }
        Ok(Self {
            directory: Dir::open_ambient_dir(path, ambient_authority())?,
            limits,
            root_id: cache.next_root_id.fetch_add(1, Ordering::Relaxed),
            cache,
        })
    }

    pub fn list(&self, relative_path: &str, include_hidden: bool) -> Result<Arc<Directory>> {
        self.list_directory(relative_path, include_hidden, false)
    }

    /// Continues a directory snapshot for at most 30 seconds, rechecking access
    /// and invalidating on observed metadata changes. Metadata is only a hint:
    /// filesystems can coalesce timestamps. Use `list` to explicitly refresh.
    pub fn list_cached(&self, relative_path: &str, include_hidden: bool) -> Result<Arc<Directory>> {
        self.list_directory(relative_path, include_hidden, true)
    }

    fn list_directory(
        &self,
        relative_path: &str,
        include_hidden: bool,
        reuse_snapshot: bool,
    ) -> Result<Arc<Directory>> {
        validate_path(relative_path)?;
        let path = if relative_path.is_empty() {
            "."
        } else {
            relative_path
        };
        let directory = self.directory.open_dir(path)?;
        // Reopen the directory and its iterator before considering cached data.
        // Metadata-only access must not bypass current enumeration permissions.
        let iterator = directory.entries()?;
        let handle = directory.try_clone()?.into_std_file();
        let version = FileVersion::of(&handle.metadata()?);
        {
            let mut cache = self
                .cache
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(index) = cache.directories.iter().position(|entry| {
                reuse_snapshot
                    && entry.created.elapsed() < Duration::from_secs(30)
                    && entry.root_id == self.root_id
                    && entry.path == relative_path
                    && entry.include_hidden == include_hidden
                    && entry.version == version
            }) {
                let entry = cache.directories.remove(index).expect("cache index exists");
                let listing = Arc::clone(&entry.listing);
                cache.directories.push_back(entry);
                return Ok(listing);
            }
        }
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut unsupported_names = 0usize;
        for (visited, item) in iterator.enumerate() {
            if visited >= self.limits.max_directory_entries {
                return Err(Error::Limit("directory entry"));
            }
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                unsupported_names += 1;
                continue;
            };
            if name.chars().any(char::is_control) {
                unsupported_names += 1;
                continue;
            }
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let kind = item.file_type()?;
            let kind = if kind.is_dir() {
                EntryKind::Directory
            } else if kind.is_file() {
                EntryKind::File
            } else if kind.is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::Other
            };
            let relative_path = Path::new(relative_path)
                .join(&name)
                .to_str()
                .ok_or(Error::InvalidPath)?
                .to_owned();
            bytes = bytes
                .saturating_add(name.len() + relative_path.len() + std::mem::size_of::<Entry>());
            if bytes > self.limits.max_directory_bytes {
                return Err(Error::Limit("directory byte"));
            }
            entries.push(Entry {
                name,
                relative_path,
                kind,
            });
        }
        if FileVersion::of(&handle.metadata()?) != version {
            return Err(Error::Changed);
        }
        entries.sort_unstable_by(|left, right| {
            (&left.kind, &left.name).cmp(&(&right.kind, &right.name))
        });
        let mut hash = Sha256::new();
        for entry in &entries {
            hash.update([entry.kind as u8]);
            hash.update(entry.relative_path.as_bytes());
            hash.update([0]);
        }
        hash.update((unsupported_names as u64).to_le_bytes());
        let listing = Arc::new(Directory {
            entries,
            revision: format!("sha256:{:x}", hash.finalize()),
            unsupported_names,
        });
        let bytes = listing.entries.capacity() * std::mem::size_of::<Entry>()
            + listing
                .entries
                .iter()
                .map(|entry| entry.name.capacity() + entry.relative_path.capacity())
                .sum::<usize>()
            + listing.revision.capacity();
        let mut cache = self
            .cache
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = cache.directories.iter().position(|entry| {
            entry.root_id == self.root_id
                && entry.path == relative_path
                && entry.include_hidden == include_hidden
        }) {
            let stale = cache.directories.remove(index).expect("cache index exists");
            cache.directory_bytes -= stale.bytes;
        }
        if self.cache.max_files > 0 && bytes <= self.cache.max_bytes {
            while cache.directories.len() >= self.cache.max_files
                || cache.directory_bytes.saturating_add(bytes) > self.cache.max_bytes
            {
                if let Some(oldest) = cache.directories.pop_front() {
                    cache.directory_bytes -= oldest.bytes;
                }
            }
            cache.directory_bytes += bytes;
            cache.directories.push_back(CachedDirectory {
                created: Instant::now(),
                root_id: self.root_id,
                path: relative_path.into(),
                include_hidden,
                version,
                listing: Arc::clone(&listing),
                bytes,
            });
        }
        Ok(listing)
    }

    /// Opens the file through the root on every call, so changed permissions or
    /// a replaced symlink cannot reuse cached content without authorization.
    pub fn check_file(&self, relative_path: &str) -> Result<()> {
        let file = self.open_file(relative_path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::NotText);
        }
        if metadata.len() > self.limits.max_file_bytes as u64 {
            return Err(Error::Limit("file byte"));
        }
        Ok(())
    }

    pub fn read(&self, relative_path: &str) -> Result<Arc<Document>> {
        self.read_file(relative_path, self.open_file(relative_path)?)
    }

    fn open_file(&self, relative_path: &str) -> Result<File> {
        validate_path(relative_path)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        Ok(self
            .directory
            .open_with(relative_path, &options)?
            .into_std())
    }

    fn read_file(&self, relative_path: &str, mut file: File) -> Result<Arc<Document>> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::NotText);
        }
        if metadata.len() > self.limits.max_file_bytes as u64 {
            return Err(Error::Limit("file byte"));
        }
        let version = FileVersion::of(&metadata);
        {
            let mut cache = self
                .cache
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(index) = cache.files.iter().position(|entry| {
                entry.root_id == self.root_id
                    && entry.path == relative_path
                    && entry.version == version
            }) {
                let entry = cache.files.remove(index).expect("cache index exists");
                let document = Arc::clone(&entry.document);
                cache.files.push_back(entry);
                return Ok(document);
            }
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(self.limits.max_file_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.limits.max_file_bytes {
            return Err(Error::Limit("file byte"));
        }
        if FileVersion::of(&file.metadata()?) != version {
            return Err(Error::Changed);
        }
        let text = String::from_utf8(bytes).map_err(|_| Error::NotText)?;
        if text.contains('\0') {
            return Err(Error::NotText);
        }
        let mut starts = Vec::new();
        if !text.is_empty() {
            starts.push(0);
            starts.extend(text.bytes().enumerate().filter_map(|(index, byte)| {
                (byte == b'\n' && index + 1 < text.len()).then_some(index + 1)
            }));
        }
        let hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        let document = Arc::new(Document { text, starts, hash });
        let mut cache = self
            .cache
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = cache
            .files
            .iter()
            .position(|entry| entry.root_id == self.root_id && entry.path == relative_path)
        {
            let stale = cache.files.remove(index).expect("cache index exists");
            cache.bytes -= stale.document.bytes();
        }
        let size = document.bytes();
        if self.cache.max_files > 0 && size <= self.cache.max_bytes {
            while cache.files.len() >= self.cache.max_files
                || cache.bytes.saturating_add(size) > self.cache.max_bytes
            {
                if let Some(oldest) = cache.files.pop_front() {
                    cache.bytes -= oldest.document.bytes();
                }
            }
            cache.bytes += size;
            cache.files.push_back(Cached {
                root_id: self.root_id,
                path: relative_path.into(),
                version,
                document: Arc::clone(&document),
            });
        }
        Ok(document)
    }
}

fn validate_path(path: &str) -> Result<()> {
    if path.len() > 4096
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::InvalidPath);
    }
    Ok(())
}
