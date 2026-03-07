use anyhow::Result;
use opentelemetry::KeyValue;
use quanta::Clock;
use std::{
    fs, io,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

#[allow(unused)]
static COMMAND_HANDLING_HIST: OnceLock<opentelemetry::metrics::Histogram<u64>> = OnceLock::new();
#[allow(unused)]
static COMMAND_COUNT_COUNTER: OnceLock<opentelemetry::metrics::Counter<u64>> = OnceLock::new();

#[allow(unused)]
pub struct Timer {
    clock: Clock,
    start: u64,
}

impl Timer {
    #[allow(unused)]
    pub fn start() -> Self {
        let clock = Clock::new();
        let start = clock.raw();
        Timer { clock, start }
    }

    #[allow(unused)]
    pub fn elapsed(&self) -> std::time::Duration {
        let now = self.clock.raw();
        self.clock.delta(self.start, now)
    }

    #[allow(unused)]
    pub fn elapsed_nanos(&self) -> u64 {
        let now = self.clock.raw();
        self.clock.delta_as_nanos(self.start, now)
    }

    #[allow(unused)]
    pub fn elapsed_micros(&self) -> u64 {
        let now = self.clock.raw();
        self.clock.delta_as_nanos(self.start, now) / 1_000
    }

    #[allow(unused)]
    pub fn log_command_handle_lat(&self, command: &str) {
        let elapsed_micros = self.elapsed_micros();
        let histogram = COMMAND_HANDLING_HIST.get_or_init(|| {
            let meter = opentelemetry::global::meter("ddb");
            meter
                .u64_histogram("command_handle_duration")
                .with_unit("us")
                .with_description("Latencies of command handling")
                .build()
        });
        histogram.record(
            elapsed_micros,
            &[KeyValue::new("command", command.to_string())],
        );
    }
}

#[allow(unused)]
pub struct Counter;
impl Counter {
    #[allow(unused)]
    pub fn log_command_count(command: &str) {
        let counter = COMMAND_COUNT_COUNTER.get_or_init(|| {
            let meter = opentelemetry::global::meter("ddb");
            meter
                .u64_counter("command_count")
                .with_description("Count of commands handled")
                .build()
        });
        counter.add(1, &[KeyValue::new("command", command.to_string())]);
    }
}

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
    #[allow(unused_imports)]
    pub use crate::debugger::gdb::runtime::*;
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
            return Ok(());
        } else {
            let msg = format!(
                "Command {} failed with stdout: {}, stderr: {}",
                full_cmd, stdout, stderr
            );
            if VERBOSE {
                debug!(msg);
            }
            return Err(io::Error::new(io::ErrorKind::Other, msg));
        }
    } else {
        Ok(())
    }
}

#[allow(unused)]
pub fn run_command_quite(cmd: &str, args: &[&str]) -> Result<(), io::Error> {
    run_command::<false, true>(cmd, args)
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
