use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result};
use tempfile::TempDir;

const LOG_TAIL_BYTES: u64 = 16 * 1024;

pub(super) struct RuntimeFiles {
    _directory: TempDir,
    token_path: PathBuf,
    report_path: PathBuf,
    log_path: PathBuf,
    log: File,
    remove_log_on_drop: bool,
    control_token: String,
    admin_token: String,
}

impl RuntimeFiles {
    pub(super) fn new(backend_log: Option<&Path>) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("ddb-tui-runtime-")
            .tempdir()
            .context("failed to create the private DDB runtime directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
                .context("failed to restrict the DDB runtime directory")?;
        }

        let control_token = random_token();
        let admin_token = random_token();
        let token_path = directory.path().join("api-tokens.json");
        let token_document = serde_json::to_vec(&serde_json::json!({
            "tokens": [
                {"token": control_token, "scope": "control"},
                {"token": admin_token, "scope": "admin"}
            ]
        }))
        .context("failed to encode managed DDB credentials")?;
        write_new_private(&token_path, &token_document)
            .context("failed to create managed DDB credentials")?;

        let report_path = directory.path().join("startup.json");
        let (log_path, remove_log_on_drop) = match backend_log {
            Some(path) => (path.to_path_buf(), false),
            None => (
                std::env::temp_dir().join(format!(
                    "ddb-tui-backend-{}.log",
                    uuid::Uuid::new_v4().simple()
                )),
                true,
            ),
        };
        let log = create_new_private(&log_path)
            .with_context(|| format!("failed to create backend log {}", log_path.display()))?;

        Ok(Self {
            _directory: directory,
            token_path,
            report_path,
            log_path,
            log,
            remove_log_on_drop,
            control_token,
            admin_token,
        })
    }

    pub(super) fn token_path(&self) -> &Path {
        &self.token_path
    }

    pub(super) fn report_path(&self) -> &Path {
        &self.report_path
    }

    pub(super) fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub(super) fn control_token(&self) -> &str {
        &self.control_token
    }

    pub(super) fn admin_token(&self) -> &str {
        &self.admin_token
    }

    pub(super) fn child_stdio(&self) -> Result<(Stdio, Stdio)> {
        let stdout = self
            .log
            .try_clone()
            .context("failed to clone backend log for stdout")?;
        let stderr = self
            .log
            .try_clone()
            .context("failed to clone backend log for stderr")?;
        Ok((Stdio::from(stdout), Stdio::from(stderr)))
    }

    pub(super) fn tail_log(&mut self) -> String {
        let _ = self.log.flush();
        let Ok(mut file) = File::open(&self.log_path) else {
            return String::new();
        };
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let start = length.saturating_sub(LOG_TAIL_BYTES);
        if file.seek(SeekFrom::Start(start)).is_err() {
            return String::new();
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub(super) fn preserve_log(&mut self) {
        self.remove_log_on_drop = false;
    }
}

impl fmt::Debug for RuntimeFiles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeFiles")
            .field("token_path", &self.token_path)
            .field("report_path", &self.report_path)
            .field("log_path", &self.log_path)
            .field("control_token", &"<redacted>")
            .field("admin_token", &"<redacted>")
            .finish()
    }
}

impl Drop for RuntimeFiles {
    fn drop(&mut self) {
        if self.remove_log_on_drop {
            let _ = fs::remove_file(&self.log_path);
        }
    }
}

fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn create_new_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))
}

fn write_new_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = create_new_private(path)?;
    file.write_all(contents)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_distinct_long_and_never_debugged() {
        let files = RuntimeFiles::new(None).unwrap();
        assert_ne!(files.control_token(), files.admin_token());
        assert!(files.control_token().len() >= 32);
        let debug = format!("{files:?}");
        assert!(!debug.contains(files.control_token()));
        assert!(!debug.contains(files.admin_token()));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_and_token_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let files = RuntimeFiles::new(None).unwrap();
        let directory_mode = fs::metadata(files._directory.path())
            .unwrap()
            .permissions()
            .mode();
        let token_mode = fs::metadata(files.token_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(directory_mode & 0o077, 0);
        assert_eq!(token_mode & 0o077, 0);
    }

    #[test]
    fn default_log_is_removed_unless_preserved() {
        let path = {
            let files = RuntimeFiles::new(None).unwrap();
            let path = files.log_path().to_path_buf();
            assert!(path.is_file());
            path
        };
        assert!(!path.exists());

        let preserved = {
            let mut files = RuntimeFiles::new(None).unwrap();
            let path = files.log_path().to_path_buf();
            files.preserve_log();
            path
        };
        assert!(preserved.exists());
        fs::remove_file(preserved).unwrap();
    }

    #[test]
    fn token_document_contains_no_debug_metadata() {
        let files = RuntimeFiles::new(None).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(files.token_path()).unwrap()).unwrap();
        assert_eq!(document["tokens"].as_array().unwrap().len(), 2);
        assert_eq!(document["tokens"][0]["scope"], "control");
        assert_eq!(document["tokens"][1]["scope"], "admin");
    }
}
