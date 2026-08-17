use std::{fs, net::SocketAddr, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

const PROTOCOL_VERSION: u32 = 1;
const MAX_REPORT_BYTES: u64 = 16 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct StartupReport {
    protocol_version: u32,
    status: String,
    phase: Option<String>,
    code: Option<String>,
    message: Option<String>,
    pid: Option<u32>,
    endpoint: Option<String>,
    server_instance_id: Option<String>,
    #[serde(default)]
    api_versions: Vec<String>,
    backend_version: Option<String>,
}

#[derive(Debug)]
pub(super) struct ReadyReport {
    pub endpoint: String,
    pub server_instance_id: String,
    pub backend_version: String,
    pub api_versions: Vec<String>,
}

pub(super) fn read(path: &Path, expected_pid: u32) -> Result<ReadyReport> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect startup report {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("startup report {} is not a regular file", path.display());
    }
    if metadata.len() > MAX_REPORT_BYTES {
        anyhow::bail!("startup report exceeds the {MAX_REPORT_BYTES} byte limit");
    }
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read startup report {}", path.display()))?;
    let report: StartupReport =
        serde_json::from_slice(&bytes).context("startup report is not valid bounded JSON")?;
    if report.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported DDB startup report version {}; ddb-tui supports {}",
            report.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if report.pid != Some(expected_pid) {
        anyhow::bail!(
            "startup report PID {:?} does not match managed DDB PID {}",
            report.pid,
            expected_pid
        );
    }
    match report.status.as_str() {
        "failed" => anyhow::bail!(
            "DDB startup failed in phase {} with {}: {}",
            report.phase.as_deref().unwrap_or("unknown"),
            report.code.as_deref().unwrap_or("STARTUP_FAILED"),
            report
                .message
                .as_deref()
                .unwrap_or("no diagnostic was provided")
        ),
        "ready" => {}
        other => anyhow::bail!("unknown DDB startup status {other:?}"),
    }

    let endpoint = report
        .endpoint
        .context("ready startup report omitted endpoint")?;
    validate_loopback_endpoint(&endpoint)?;
    if !report.api_versions.iter().any(|version| version == "v2") {
        anyhow::bail!("managed DDB did not advertise API v2");
    }
    let api_versions = report.api_versions;
    let server_instance_id = report
        .server_instance_id
        .filter(|value| !value.is_empty())
        .context("ready startup report omitted server_instance_id")?;
    let backend_version = report
        .backend_version
        .filter(|value| !value.is_empty())
        .context("ready startup report omitted backend_version")?;

    Ok(ReadyReport {
        endpoint,
        server_instance_id,
        backend_version,
        api_versions,
    })
}

fn validate_loopback_endpoint(endpoint: &str) -> Result<()> {
    let address = endpoint
        .strip_prefix("http://")
        .context("managed DDB endpoint must use plain HTTP on loopback")?
        .parse::<SocketAddr>()
        .context("managed DDB endpoint is not a socket address")?;
    if !address.ip().is_loopback() {
        anyhow::bail!("managed DDB reported a non-loopback endpoint");
    }
    if address.port() == 0 {
        anyhow::bail!("managed DDB reported unresolved port 0");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_endpoint_validation_is_fail_closed() {
        validate_loopback_endpoint("http://127.0.0.1:43210").unwrap();
        validate_loopback_endpoint("http://[::1]:43210").unwrap();
        assert!(validate_loopback_endpoint("https://127.0.0.1:43210").is_err());
        assert!(validate_loopback_endpoint("http://0.0.0.0:43210").is_err());
        assert!(validate_loopback_endpoint("http://127.0.0.1:0").is_err());
    }

    #[test]
    fn ready_report_checks_pid_protocol_and_api() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup.json");
        fs::write(
            &path,
            r#"{
                "protocol_version": 1,
                "status": "ready",
                "pid": 42,
                "endpoint": "http://127.0.0.1:43210",
                "server_instance_id": "instance",
                "api_versions": ["v2"],
                "backend_version": "0.1.15"
            }"#,
        )
        .unwrap();
        let report = read(&path, 42).unwrap();
        assert_eq!(report.endpoint, "http://127.0.0.1:43210");
        assert_eq!(report.server_instance_id, "instance");
        assert!(read(&path, 43).is_err());
    }

    #[test]
    fn failed_report_keeps_phase_and_code_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup.json");
        fs::write(
            &path,
            r#"{
                "protocol_version": 1,
                "status": "failed",
                "pid": 42,
                "phase": "config_validation",
                "code": "CONFIG_INVALID",
                "message": "bad session"
            }"#,
        )
        .unwrap();
        let error = read(&path, 42).unwrap_err().to_string();
        assert!(error.contains("config_validation"));
        assert!(error.contains("CONFIG_INVALID"));
        assert!(error.contains("bad session"));
    }
}
