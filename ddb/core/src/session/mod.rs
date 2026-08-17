pub(crate) mod activation;
pub(crate) mod factory;
pub(crate) mod lifecycle;
pub mod process;
pub(crate) mod supervisor;

pub use process::*;

use anyhow::{anyhow, Result};

use crate::{
    common::config::{Config, DebuggerCommand, OnExit},
    dbg_ctrl::TransportSpec,
    state::ServiceIdentity,
};

/// Validated, source-independent description of a debugger session to admit.
#[derive(Debug)]
pub struct SessionRequest {
    pub mode: SessionMode,
    pub sudo: bool,
    pub on_exit: OnExit,
    pub tag: Option<String>,
    pub prerun_debugger_cmds: Vec<DebuggerCommand>,
    pub postrun_debugger_cmds: Vec<DebuggerCommand>,
    pub stop_at_entry: bool,
    pub service_identity: Option<ServiceIdentity>,
    pub transport: TransportSpec,
    pub caladan_ip: Option<u32>,
}

/// Builds a request from explicit application configuration.
///
/// This keeps admission deterministic and testable: constructing a request no
/// longer reaches through the global configuration singleton.
#[derive(Debug)]
pub struct SessionRequestBuilder {
    mode: Option<SessionMode>,
    sudo: bool,
    on_exit: OnExit,
    tag: Option<String>,
    prerun_debugger_cmds: Vec<DebuggerCommand>,
    postrun_debugger_cmds: Vec<DebuggerCommand>,
    stop_at_entry: bool,
    service_identity: Option<ServiceIdentity>,
    transport: Option<TransportSpec>,
    caladan_ip: Option<u32>,
}

impl SessionRequestBuilder {
    pub fn from_config(config: &Config) -> Self {
        Self {
            mode: None,
            sudo: config.conf.sudo,
            on_exit: config.conf.on_exit.clone(),
            tag: None,
            prerun_debugger_cmds: config.prerun_debugger_cmds.clone(),
            postrun_debugger_cmds: config.postrun_debugger_cmds.clone(),
            stop_at_entry: false,
            service_identity: None,
            transport: None,
            caladan_ip: None,
        }
    }

    pub fn mode(mut self, mode: SessionMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn on_exit(mut self, on_exit: OnExit) -> Self {
        self.on_exit = on_exit;
        self
    }

    pub fn stop_at_entry(mut self, stop_at_entry: bool) -> Self {
        self.stop_at_entry = stop_at_entry;
        self
    }

    pub fn transport(mut self, transport: TransportSpec) -> Self {
        self.transport = Some(transport);
        self
    }

    pub fn service_identity(mut self, identity: ServiceIdentity) -> Self {
        self.service_identity = Some(identity);
        self
    }

    pub fn caladan_ip(mut self, caladan_ip: Option<u32>) -> Self {
        self.caladan_ip = caladan_ip;
        self
    }

    pub fn build(self) -> Result<SessionRequest> {
        Ok(SessionRequest {
            mode: self
                .mode
                .ok_or_else(|| anyhow!("session start mode is required"))?,
            sudo: self.sudo,
            on_exit: self.on_exit,
            tag: self.tag,
            prerun_debugger_cmds: self.prerun_debugger_cmds,
            postrun_debugger_cmds: self.postrun_debugger_cmds,
            stop_at_entry: self.stop_at_entry,
            service_identity: self.service_identity,
            transport: self
                .transport
                .ok_or_else(|| anyhow!("session transport is required"))?,
            caladan_ip: self.caladan_ip,
        })
    }
}

#[derive(Debug, Clone)]
pub enum SessionStart {
    Attach(u64),
    Binary { path: String, args: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum SessionMode {
    Local(SessionStart),
    Remote(SessionStart),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_requires_mode() {
        let error = SessionRequestBuilder::from_config(&Config::default())
            .transport(TransportSpec::Local)
            .build()
            .expect_err("mode should be required");

        assert_eq!(error.to_string(), "session start mode is required");
    }

    #[test]
    fn request_requires_transport() {
        let error = SessionRequestBuilder::from_config(&Config::default())
            .mode(SessionMode::Local(SessionStart::Attach(42)))
            .build()
            .expect_err("transport should be required");

        assert_eq!(error.to_string(), "session transport is required");
    }

    #[test]
    fn request_inherits_explicit_config_defaults() {
        let mut config = Config::default();
        config.conf.sudo = true;
        config.conf.on_exit = OnExit::KILL;
        config.prerun_debugger_cmds.push(DebuggerCommand {
            name: "setup".to_string(),
            command: "set pagination off".to_string(),
        });

        let request = SessionRequestBuilder::from_config(&config)
            .mode(SessionMode::Local(SessionStart::Attach(42)))
            .transport(TransportSpec::Local)
            .build()
            .expect("request should be valid");

        assert!(request.sudo);
        assert_eq!(request.on_exit, OnExit::KILL);
        assert_eq!(request.prerun_debugger_cmds.len(), 1);
    }
}
