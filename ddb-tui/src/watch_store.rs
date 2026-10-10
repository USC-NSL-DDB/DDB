use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    expressions: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    _lock: LockGuard,
    expected: Option<Vec<u8>>,
}

struct LockGuard(File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Closing only this descriptor is insufficient while a child between
        // fork and exec retains the shared open file description.
        loop {
            match self.0.unlock() {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<(Self, Vec<String>)> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .context("watch file parent directory must exist")?;
        let name = path
            .file_name()
            .context("watch file must name a regular file")?;
        let path = parent.join(name);
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lock = options
            .open(parent.join(lock_name))
            .context("could not open watch-file lock")?;
        ensure!(
            lock.metadata()?.is_file(),
            "watch-file lock must be a regular file"
        );
        lock.try_lock()
            .context("could not exclusively lock watch file; another TUI may be using it")?;
        let lock = LockGuard(lock);
        let expected = read(&path)?;
        let expressions = match expected.as_deref() {
            None => Vec::new(),
            Some(bytes) => {
                let document: Document = serde_json::from_slice(bytes)
                    .map_err(|_| anyhow::anyhow!("watch file must contain valid versioned JSON with only version and expressions fields"))?;
                ensure!(
                    document.version == 1,
                    "unsupported watch file version {}",
                    document.version
                );
                validate(&document.expressions)?;
                document.expressions
            }
        };
        Ok((
            Self {
                path,
                _lock: lock,
                expected,
            },
            expressions,
        ))
    }

    fn save(&mut self, expressions: Vec<String>) -> Result<()> {
        validate(&expressions)?;
        ensure!(
            read(&self.path)? == self.expected,
            "watch file changed outside this TUI; refusing to overwrite it"
        );
        let mut bytes = serde_json::to_vec_pretty(&Document {
            version: 1,
            expressions,
        })?;
        bytes.push(b'\n');
        ensure!(bytes.len() as u64 <= MAX_BYTES, "watch file exceeds 1 MiB");
        let parent = self.path.parent().context("watch file parent missing")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        if self.expected.is_some() {
            temporary
                .persist(&self.path)
                .context("could not replace watch file atomically")?;
        } else {
            temporary
                .persist_noclobber(&self.path)
                .context("could not create watch file without overwriting an existing file")?;
        }
        self.expected = Some(bytes);
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.is_file(),
            "watch path must be a regular file, not a symlink or directory"
        ),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "watch path must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "watch file exceeds 1 MiB");
    Ok(Some(bytes))
}

fn validate(expressions: &[String]) -> Result<()> {
    ensure!(expressions.len() <= 64, "watch file exceeds 64 expressions");
    for (index, expression) in expressions.iter().enumerate() {
        ensure!(
            !expression.trim().is_empty()
                && expression.len() <= 4096
                && !expression.chars().any(char::is_control),
            "watch expression {} must be nonempty, single-line, and at most 4096 bytes",
            index + 1
        );
    }
    Ok(())
}

pub struct Saver {
    updates: watch::Sender<Vec<String>>,
    task: tokio::task::JoinHandle<Result<()>>,
    error: Arc<Mutex<Option<String>>>,
}

impl Saver {
    pub fn start(mut store: Store) -> Self {
        let (updates, mut receiver) = watch::channel(Vec::new());
        let error = Arc::new(Mutex::new(None));
        let reported_error = error.clone();
        let task = tokio::spawn(async move {
            let mut last_error = None;
            while receiver.changed().await.is_ok() {
                let expressions = receiver.borrow_and_update().clone();
                let (returned, result) = tokio::task::spawn_blocking(move || {
                    let result = store.save(expressions);
                    (store, result)
                })
                .await
                .context("watch save task failed")?;
                store = returned;
                last_error = result.err().map(|error| {
                    format!(
                        "Could not save watches to {}: {error:#}",
                        store.path.display()
                    )
                });
                if let Some(error) = &last_error {
                    *reported_error.lock().unwrap() = Some(error.clone());
                }
            }
            if let Some(error) = last_error {
                bail!(error);
            }
            Ok(())
        });
        Self {
            updates,
            task,
            error,
        }
    }

    pub fn update(&self, expressions: Vec<String>) -> Result<()> {
        self.updates
            .send(expressions)
            .context("watch persistence worker stopped")
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }

    pub async fn finish(self) -> Result<()> {
        drop(self.updates);
        self.task.await.context("watch persistence task failed")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_drop_releases_lock_even_with_an_inherited_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watches.json");
        let (store, _) = Store::open(&path).unwrap();
        // A fork before exec temporarily retains the same open file description,
        // even when the descriptor is close-on-exec. Duplication models that
        // lifetime without forking a multithreaded Rust test process.
        let inherited = store._lock.0.try_clone().unwrap();
        drop(store);
        assert!(
            Store::open(&path).is_ok(),
            "owner drop must release the advisory lock"
        );
        drop(inherited);
    }

    #[test]
    fn round_trip_stores_only_expressions_and_releases_exclusive_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watches.json");
        let (mut store, expressions) = Store::open(&path).unwrap();
        assert!(expressions.is_empty());
        assert!(!path.exists());
        assert!(Store::open(&path).is_err());
        store
            .save(vec!["counter + 1".into(), "变量".into()])
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2);
        assert_eq!(json["version"], 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(store);
        let (mut reopened, expressions) = Store::open(&path).unwrap();
        assert_eq!(expressions, ["counter + 1", "变量"]);
        reopened.save(Vec::new()).unwrap();
        drop(reopened);
        assert!(Store::open(&path).unwrap().1.is_empty());
    }

    #[test]
    fn malformed_future_oversized_and_externally_modified_files_are_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watches.json");
        for bytes in [
            b"{".to_vec(),
            br#"{"version":2,"expressions":[]}"#.to_vec(),
            br#"{"version":1,"expressions":[],"handles":[]}"#.to_vec(),
            br#"{"version":1,"expressions":[""]}"#.to_vec(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(Store::open(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::write(&path, br#"{"version":1,"expressions":["original"]}"#).unwrap();
        let (mut store, _) = Store::open(&path).unwrap();
        let external = br#"{"version":1,"expressions":["external"]}"#;
        std::fs::write(&path, external).unwrap();
        assert!(store.save(vec!["overwrite".into()]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), external);
        assert!(validate(&vec!["x".into(); 65]).is_err());
        assert!(validate(&["x".repeat(4097)]).is_err());
        assert!(validate(&["line\nbreak".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_watch_and_lock_paths_are_rejected() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, b"do not touch").unwrap();
        let watch = directory.path().join("watches.json");
        symlink(&target, &watch).unwrap();
        assert!(Store::open(&watch).is_err());
        let other = directory.path().join("other.json");
        symlink(&target, directory.path().join("other.json.lock")).unwrap();
        assert!(Store::open(&other).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"do not touch");
    }

    #[tokio::test]
    async fn background_saver_flushes_latest_update_and_reports_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watches.json");
        let saver = Saver::start(Store::open(&path).unwrap().0);
        for index in 0..100 {
            saver.update(vec![format!("value_{index}")]).unwrap();
        }
        saver.finish().await.unwrap();
        let (store, expressions) = Store::open(&path).unwrap();
        assert_eq!(expressions, ["value_99"]);
        let external = br#"{"version":1,"expressions":["external"]}"#;
        std::fs::write(&path, external).unwrap();
        let saver = Saver::start(store);
        saver.update(vec!["not_saved".into()]).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(error) = saver.take_error() {
                    break error;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(error.contains("changed outside"));
        assert!(saver.finish().await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), external);
    }
}
