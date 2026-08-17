use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::arg::TuiArgs;

pub(crate) fn dispatch(args: &TuiArgs) -> Result<i32> {
    let ddb = env::current_exe().context("failed to resolve the current ddb executable")?;
    let tui = resolve_frontend(&ddb)?;
    let mut command = Command::new(&tui);
    command.arg("--ddb-path").arg(&ddb).args(&args.args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(error).with_context(|| format!("failed to execute ddb-tui {}", tui.display()))
    }

    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("failed to start ddb-tui {}", tui.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}

fn resolve_frontend(ddb_executable: &Path) -> Result<PathBuf> {
    let mut checked = Vec::new();
    if let Some(configured) = env::var_os("DDB_TUI_PATH") {
        let path = PathBuf::from(configured);
        checked.push(path.clone());
        return validate_executable(path)
            .with_context(|| "DDB_TUI_PATH does not identify an executable ddb-tui binary");
    }

    if let Some(parent) = ddb_executable.parent() {
        let sibling = parent.join(frontend_name());
        checked.push(sibling.clone());
        if is_executable(&sibling) {
            return sibling
                .canonicalize()
                .context("failed to canonicalize sibling ddb-tui");
        }
    }

    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(frontend_name());
            checked.push(candidate.clone());
            if is_executable(&candidate) {
                return candidate
                    .canonicalize()
                    .context("failed to canonicalize ddb-tui from PATH");
            }
        }
    }

    let locations = checked
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "ddb-tui was not found; checked {locations}. Install the paired DDB package or set DDB_TUI_PATH"
    )
}

fn validate_executable(path: PathBuf) -> Result<PathBuf> {
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

fn frontend_name() -> OsString {
    #[cfg(windows)]
    {
        OsString::from("ddb-tui.exe")
    }
    #[cfg(not(windows))]
    {
        OsString::from("ddb-tui")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn executable_validation_rejects_non_executable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddb-tui");
        fs::write(&path, "not executable").unwrap();
        let error = validate_executable(path).unwrap_err();
        assert!(error.to_string().contains("not a regular executable"));
    }

    #[cfg(unix)]
    #[test]
    fn executable_validation_returns_a_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddb-tui");
        make_executable(&path);
        assert_eq!(
            validate_executable(path.clone()).unwrap(),
            path.canonicalize().unwrap()
        );
    }
}
