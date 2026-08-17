use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub(super) fn resolve(explicit: Option<&Path>) -> Result<PathBuf> {
    let mut checked = Vec::new();
    if let Some(path) = explicit {
        return validate(path.to_path_buf()).context("--ddb-path is not executable");
    }
    if let Some(configured) = env::var_os("DDB_BACKEND_PATH") {
        return validate(PathBuf::from(configured)).context("DDB_BACKEND_PATH is not executable");
    }

    let current = env::current_exe().context("failed to locate the ddb-tui executable")?;
    if let Some(parent) = current.parent() {
        let sibling = parent.join(backend_name());
        checked.push(sibling.clone());
        if is_executable(&sibling) {
            return sibling
                .canonicalize()
                .context("failed to canonicalize sibling ddb");
        }
    }

    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(backend_name());
            checked.push(candidate.clone());
            if is_executable(&candidate) {
                return candidate
                    .canonicalize()
                    .context("failed to canonicalize ddb from PATH");
            }
        }
    }

    anyhow::bail!(
        "ddb backend was not found; checked {}. Install the paired DDB package, pass --ddb-path, or set DDB_BACKEND_PATH",
        checked
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn validate(path: PathBuf) -> Result<PathBuf> {
    if !is_executable(&path) {
        anyhow::bail!("{} is not a regular executable file", path.display());
    }
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn backend_name() -> OsString {
    #[cfg(windows)]
    {
        OsString::from("ddb.exe")
    }
    #[cfg(not(windows))]
    {
        OsString::from("ddb")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn explicit_path_must_be_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddb");
        fs::write(&path, "not executable").unwrap();
        assert!(resolve(Some(&path))
            .unwrap_err()
            .to_string()
            .contains("--ddb-path"));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_executable_is_canonicalized() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddb");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(resolve(Some(&path)).unwrap(), path.canonicalize().unwrap());
    }
}
