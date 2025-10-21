use anyhow::{bail, Result};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

pub mod mqtt {
    use core::panic;

    use rumqttc::Transport;
    use tracing::error;

    pub fn str_to_transport(s: &str) -> Transport {
        match s {
            "tcp" => Transport::Tcp,
            // "udp" => Transport::Ws,
            _ => {
                error!("Invalid transport type: {}", s);
                panic!("Invalid transport type");
            }
        }
    }
}

#[allow(dead_code)]
pub mod gdb {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::common::default_vals::{
        DEFAULT_EMBEDED_GDB_EXT_PATH, DEFAULT_GDB_EXT_DIR, DEFAULT_GDB_EXT_NAME, DEFAULT_MI_VERSION, EMBEDED_PROCLET_GDB_EXT_PATH, PROCLET_GDB_EXT_NAME,
    };
    use crate::Asset;
    use anyhow::{Context, Result};

    pub fn get_default_mi_arg() -> String {
        format!("--interpreter={}", DEFAULT_MI_VERSION)
    }

    pub fn gdb_start_cmd(sudo: bool) -> String {
        format!(
            "{} gdb {} -q",
            if sudo { "sudo" } else { "" },
            get_default_mi_arg()
        )
    }

    pub struct GdbStartCmdBuilder {
        sudo: bool,
        mi_version: Option<String>,
        quite: bool,
    }

    impl GdbStartCmdBuilder {
        pub fn new() -> Self {
            Self {
                sudo: false,
                mi_version: None,
                quite: false,
            }
        }

        pub fn sudo(mut self, sudo: bool) -> Self {
            self.sudo = sudo;
            self
        }

        pub fn mi_version(mut self, mi_version: &str) -> Self {
            self.mi_version = Some(mi_version.to_string());
            self
        }

        pub fn quite(mut self, quite: bool) -> Self {
            self.quite = quite;
            self
        }

        pub fn build(self) -> String {
            let mi_version = self
                .mi_version
                .unwrap_or_else(|| DEFAULT_MI_VERSION.to_string());
            let mi_arg = format!("--interpreter={}", mi_version);
            let quite_arg = if self.quite { "-q" } else { "" };
            format!(
                "{} gdb {} {}",
                if self.sudo { "sudo" } else { "" },
                mi_arg,
                quite_arg
            )
        }
    }
    
    fn write_gdb_ext_script(file_path: &PathBuf, content: &[u8]) -> Result<PathBuf> {
        // Write the content to the script file
        // NOTE: this file should be shared across
        // all nodes in the cluster and be mounted
        // to the same path on each node
        fs::write(&file_path, content)
            .context("Failed to create gdb extension script")?;

        // Return the absolute file path
        Ok(file_path
            .canonicalize()
            .context("Failed to canonicalize file path")?)
    }

    pub fn setup_gdb_ext_script() -> Result<PathBuf> {
        let script_content = Asset::get(DEFAULT_EMBEDED_GDB_EXT_PATH)
            .context("Failed to get gdb extension script")?;
        let path = Path::new(DEFAULT_GDB_EXT_DIR);
        let file_path = path.join(DEFAULT_GDB_EXT_NAME);
        Ok(write_gdb_ext_script(&file_path, &script_content.data)?)
    }
    
    pub fn setup_proclet_ext_script() -> Result<PathBuf> {
        let script_content = Asset::get(EMBEDED_PROCLET_GDB_EXT_PATH)
            .context("Failed to get embeded proclet.py")?;
        let path = Path::new(DEFAULT_GDB_EXT_DIR);
        let file_path = path.join(PROCLET_GDB_EXT_NAME);
        Ok(write_gdb_ext_script(&file_path, &script_content.data)?)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_gdb_start_cmd() {
            let cmd = gdb_start_cmd(true);
            assert_eq!(cmd, "sudo gdb --interpreter=mi3 -q");
        }

        #[test]
        fn test_gdb_start_cmd_builder() {
            let cmd = GdbStartCmdBuilder::new().sudo(true).quite(true).build();
            assert_eq!(cmd.trim(), "sudo gdb --interpreter=mi3 -q");

            let cmd = GdbStartCmdBuilder::new().sudo(true).quite(false).build();
            assert_eq!(cmd.trim(), "sudo gdb --interpreter=mi3");

            let cmd = GdbStartCmdBuilder::new().sudo(false).quite(false).build();
            assert_eq!(cmd.trim(), "gdb --interpreter=mi3");
        }

        #[test]
        fn test_setup_gdb_ext_script() {
            let temp_dir = std::path::Path::new("/tmp/ddb/gdb_ext");
            std::fs::create_dir_all(temp_dir).expect("Failed to create /tmp/ddb/gdb_ext");

            let path = setup_gdb_ext_script().unwrap();
            assert!(path.exists());

            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let assets_path = std::path::Path::new(manifest_dir)
                .join("assets")
                .join(DEFAULT_EMBEDED_GDB_EXT_PATH);
            let expected = fs::read_to_string(assets_path)
                .expect("Failed to read assets/gdb_ext/runtime-gdb.py");
            assert!(!expected.is_empty(), "gdb extension script is empty");

            let real =
                fs::read_to_string(&path).expect("Failed to read written out gdb extension script");
            assert_eq!(expected, real);
            fs::remove_file(path).unwrap();
        }
    }
}

pub fn run_command<const VERBOSE: bool>(cmd: &str, args: &[&str]) -> Result<()> {
    use tracing::debug;
    let full_cmd = format!("{} {}", cmd, args.join(" "));
    let child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        if VERBOSE {
            debug!(
                "Command {} success with stdout: {}, stderr: {}",
                full_cmd, stdout, stderr
            );
        }
        return Ok(());
    } else {
        let msg = format!(
            "Command {} success with stdout: {}, stderr: {}",
            full_cmd, stdout, stderr
        );
        if VERBOSE {
            debug!(msg);
        }
        bail!(msg);
    }
}

#[allow(unused)]
pub fn run_command_quite(cmd: &str, args: &[&str]) -> Result<()> {
    run_command::<false>(cmd, args)
}

pub fn expand_path(path: &str) -> PathBuf {
    // Expand `~` and `$VAR` environment variables
    let expanded = shellexpand::full(path).expect("Failed to expand path");

    // Convert to an absolute canonicalized path
    fs::canonicalize(&*expanded).unwrap_or_else(|_| PathBuf::from(&*expanded)) // Fallback if the path doesn't exist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_path_tilde() {
        let path = "~/test";
        let expanded = expand_path(path);
        let home_dir = std::env::var("HOME").expect("Failed to get home directory");
        let home_dir = PathBuf::from(home_dir);
        let expected_path = home_dir.join("test");
        assert_eq!(expanded, expected_path);
    }

    #[test]
    fn test_expand_path_env_var() {
        let path = "$HOME/test";
        let expanded = expand_path(path);
        let home_dir = std::env::var("HOME").expect("Failed to get home directory");
        let home_dir = PathBuf::from(home_dir);
        let expected_path = home_dir.join("test");
        assert_eq!(expanded, expected_path);
    }
}
