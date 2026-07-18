pub mod dbg_session;

pub use dbg_session::*;

use crate::{
    common::config::{DebuggerCommand, GdbCommand, OnExit},
    dbg_ctrl::TransportSpec,
    discovery::discovery_message_producer::ServiceMeta,
};

#[derive(Debug)]
#[allow(unused)]
pub struct DbgSessionConfig {
    // TODO: considering move this to the global config
    // as it can also be shared manually in the config
    pub mode: DbgMode,
    pub sudo: bool,
    pub on_exit: OnExit,
    pub tag: Option<String>,

    pub prerun_debugger_cmds: Vec<DebuggerCommand>,
    pub postrun_debugger_cmds: Vec<DebuggerCommand>,
    pub stop_at_entry: bool,

    // This should be present if the service discovery is enabled.
    // pub service_info: Option<ServiceInfo>,
    pub service_meta: Option<ServiceMeta>,

    pub transport: TransportSpec,
}

#[derive(Debug)]
pub struct DbgSessionCfgBuilder {
    pub mode: Option<DbgMode>,
    pub sudo: bool,
    pub on_exit: OnExit,
    pub tag: Option<String>,

    pub prerun_debugger_cmds: Vec<DebuggerCommand>,
    pub postrun_debugger_cmds: Vec<DebuggerCommand>,
    pub stop_at_entry: bool,

    pub service_meta: Option<ServiceMeta>,
    pub transport: Option<TransportSpec>,
}

/// Creates a new `DbgSessionCfgBuilder` initialized with values from the global configuration.
///
/// This constructor initializes a new builder with the following fields inherited from global config:
/// - `sudo`: Whether to run with sudo privileges
/// - `on_exit`: Behavior specification for program exit
/// - `prerun_debugger_cmds`: debugger commands to run before debugging session
/// - `postrun_debugger_cmds`: debugger commands to run after debugging session
///
/// All other fields are initialized to `None`.
///
/// # Returns
///
/// Returns a new instance of `DbgSessionCfgBuilder` with default values from global configuration.
#[allow(unused)]
impl DbgSessionCfgBuilder {
    pub fn new() -> Self {
        // fill in fields that inherit from global config
        let gconf = crate::common::config::Config::global();
        let sudo = gconf.conf.sudo;
        let on_exit = gconf.conf.on_exit.clone();
        let prerun_debugger_cmds = gconf.prerun_gdb_cmds.clone();
        let postrun_debugger_cmds = gconf.postrun_gdb_cmds.clone();

        Self {
            mode: None,
            sudo,
            on_exit,
            tag: None,
            prerun_debugger_cmds,
            postrun_debugger_cmds,
            stop_at_entry: false,
            service_meta: None,
            transport: None,
        }
    }

    pub fn mode(mut self, mode: DbgMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn sudo(mut self, sudo: bool) -> Self {
        self.sudo = sudo;
        self
    }

    pub fn on_exit(mut self, on_exit: OnExit) -> Self {
        self.on_exit = on_exit;
        self
    }

    pub fn tag(mut self, tag: String) -> Self {
        self.tag = Some(tag);
        self
    }

    pub fn add_prerun_debugger_cmds(mut self, cmds: Vec<DebuggerCommand>) -> Self {
        self.prerun_debugger_cmds.extend(cmds);
        self
    }

    pub fn add_prerun_debugger_cmd(mut self, cmd: DebuggerCommand) -> Self {
        self.prerun_debugger_cmds.push(cmd);
        self
    }

    pub fn add_prerun_gdb_cmds(self, cmds: Vec<GdbCommand>) -> Self {
        self.add_prerun_debugger_cmds(cmds)
    }

    pub fn add_prerun_gdb_cmd(self, cmd: GdbCommand) -> Self {
        self.add_prerun_debugger_cmd(cmd)
    }

    pub fn add_postrun_debugger_cmds(mut self, cmds: Vec<DebuggerCommand>) -> Self {
        self.postrun_debugger_cmds.extend(cmds);
        self
    }

    pub fn add_postrun_debugger_cmd(mut self, cmd: DebuggerCommand) -> Self {
        self.postrun_debugger_cmds.push(cmd);
        self
    }

    pub fn add_postrun_gdb_cmds(self, cmds: Vec<GdbCommand>) -> Self {
        self.add_postrun_debugger_cmds(cmds)
    }

    pub fn add_postrun_gdb_cmd(self, cmd: GdbCommand) -> Self {
        self.add_postrun_debugger_cmd(cmd)
    }

    pub fn with_service_meta(mut self, meta: ServiceMeta) -> Self {
        self.service_meta = Some(meta);
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

    pub fn build(self) -> DbgSessionConfig {
        let mode = {
            if self.mode.is_none() {
                panic!("DbgSessionConfig DbgMode is required");
            }
            self.mode.as_ref().unwrap()
        };

        DbgSessionConfig {
            mode: mode.clone(),
            sudo: self.sudo,
            on_exit: self.on_exit,
            tag: self.tag,
            prerun_debugger_cmds: self.prerun_debugger_cmds,
            postrun_debugger_cmds: self.postrun_debugger_cmds,
            stop_at_entry: self.stop_at_entry,
            service_meta: self.service_meta,
            transport: self
                .transport
                .expect("DbgSessionConfig transport is required"),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DbgStartMode {
    ATTACH(u64), // pid
    BINARY { path: String, args: Vec<String> },
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DbgMode {
    LOCAL(DbgStartMode),
    REMOTE(DbgStartMode),
}

// impl Default for DbgMode {
//     fn default() -> Self {
//         // Set attach pid mode as default for now
//         // as we dropped other supported modes
//         DbgMode::REMOTE(DbgStartMode::ATTACH)
//     }
// }
