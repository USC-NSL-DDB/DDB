use anyhow::Result;
use std::{
    fs, io,
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

pub fn run_command<const VERBOSE: bool, const WAIT_RESULT: bool>(
    cmd: &str,
    args: &[&str],
) -> Result<(), io::Error> {
    use tracing::debug;
    let full_cmd = format!("{} {}", cmd, args.join(" "));
    let child = Command::new(cmd)
        .args(args)
        .stdout(if WAIT_RESULT {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if WAIT_RESULT {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()?;
    if WAIT_RESULT {
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            if VERBOSE {
                debug!(
                    "Command {} succeeded with stdout: {}, stderr: {}",
                    full_cmd, stdout, stderr
                );
            }
            Ok(())
        } else {
            let msg = format!(
                "Command {} failed with stdout: {}, stderr: {}",
                full_cmd, stdout, stderr
            );
            if VERBOSE {
                debug!(msg);
            }
            Err(io::Error::other(msg))
        }
    } else {
        Ok(())
    }
}

pub fn expand_path(path: &str) -> PathBuf {
    // Expand `~` and `$VAR` environment variables
    let expanded = shellexpand::full(path).expect("Failed to expand path");

    // Convert to an absolute canonicalized path
    fs::canonicalize(&*expanded).unwrap_or_else(|_| PathBuf::from(&*expanded)) // Fallback if the path doesn't exist
}

/// Get the hostname of the current machine.
///
/// Returns the hostname as a `String`. If the hostname cannot be determined,
/// returns an error with context information.
///
/// # Examples
///
/// ```
/// use ddb::common::utils::get_hostname;
///
/// let hostname = get_hostname().expect("Failed to get hostname");
/// println!("Current hostname: {}", hostname);
/// ```
pub fn get_hostname() -> Result<String> {
    gethostname::gethostname()
        .into_string()
        .map_err(|_| anyhow::anyhow!("Failed to convert hostname to valid UTF-8 string"))
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

    #[test]
    fn test_get_hostname() {
        let hostname = get_hostname().expect("Failed to get hostname");
        assert!(!hostname.is_empty(), "Hostname should not be empty");
        // Hostname should be valid UTF-8 and contain only valid characters
        assert!(hostname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'));
    }
}
