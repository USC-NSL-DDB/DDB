pub mod dbg_session;

use std::net::Ipv4Addr;

pub use dbg_session::*;

use crate::{
    common::config::{GdbCommand, OnExit}, connection::ssh_client::SSHCred, dbg_ctrl::{DbgControllable, DbgController}, discovery::ServiceInfo
};

#[derive(Debug)]
#[allow(unused)]
pub struct DbgSessionConfig {
    // TODO: considering move this to the global config
    // as it can also be shared manually in the config
    pub mode: DbgMode,
    pub sudo: bool,
    pub on_exit: OnExit,
    // TODO: consider redesign this API,
    // as not all connection needs ssh cred.
    pub ssh_cred: Option<SSHCred>,
    pub tag: Option<String>,

    pub prerun_gdb_cmds: Vec<GdbCommand>,
    pub postrun_gdb_cmds: Vec<GdbCommand>,

    // This should be present if the service discovery is enabled.
    pub service_info: Option<ServiceInfo>,
    
    pub gdb_controller: DbgController,
}

#[derive(Debug)]
pub struct DbgSessionCfgBuilder {
    pub mode: Option<DbgMode>,
    pub sudo: bool,
    pub on_exit: OnExit,
    pub ssh_cred: Option<SSHCred>,
    pub tag: Option<String>,

    pub prerun_gdb_cmds: Vec<GdbCommand>,
    pub postrun_gdb_cmds: Vec<GdbCommand>,

    pub service_info: Option<ServiceInfo>,
    pub gdb_controller: Option<DbgController>,
}

/// Creates a new `DbgSessionCfgBuilder` initialized with values from the global configuration.
///
/// This constructor initializes a new builder with the following fields inherited from global config:
/// - `sudo`: Whether to run with sudo privileges
/// - `on_exit`: Behavior specification for program exit
/// - `prerun_gdb_cmds`: GDB commands to run before debugging session
/// - `postrun_gdb_cmds`: GDB commands to run after debugging session
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
        let prerun_gdb_cmds = gconf.prerun_gdb_cmds.clone();
        let postrun_gdb_cmds = gconf.postrun_gdb_cmds.clone();

        Self {
            mode: None,
            sudo,
            on_exit,
            ssh_cred: None,
            tag: None,
            prerun_gdb_cmds,
            postrun_gdb_cmds,
            service_info: None,
            gdb_controller: None,
            
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

    pub fn ssh_cred(mut self, host: Ipv4Addr) -> Self {
        let g = crate::common::config::Config::global();
        let ssh_cred = SSHCred::new(
            host.to_string().as_str(),
            g.ssh.port,
            g.ssh.user.as_str(),
            None,
        );

        self.ssh_cred = Some(ssh_cred);
        self
    }

    pub fn tag(mut self, tag: String) -> Self {
        self.tag = Some(tag);
        self
    }

    pub fn add_prerun_gdb_cmds(mut self, cmds: Vec<GdbCommand>) -> Self {
        self.prerun_gdb_cmds.extend(cmds);
        self
    }

    pub fn add_prerun_gdb_cmd(mut self, cmd: GdbCommand) -> Self {
        self.prerun_gdb_cmds.push(cmd);
        self
    }

    pub fn add_postrun_gdb_cmds(mut self, cmds: Vec<GdbCommand>) -> Self {
        self.postrun_gdb_cmds.extend(cmds);
        self
    }

    pub fn add_postrun_gdb_cmd(mut self, cmd: GdbCommand) -> Self {
        self.postrun_gdb_cmds.push(cmd);
        self
    }

    pub fn with_service_info(mut self, si: ServiceInfo) -> Self {
        self.service_info = Some(si);
        self
    }

    pub fn add_gdb_controller(mut self, gdb_controller: DbgController) -> Self {
        self.gdb_controller = Some(gdb_controller);
        self
    }
    pub fn build(self) -> DbgSessionConfig {
        let mode = {
            if self.mode.is_none() {
                panic!("DbgSessionConfig DbgMode is required");
            }
            self.mode.as_ref().unwrap()
        };

        let ssh_cred = {
            if let DbgMode::REMOTE(_) = mode {
                if self.ssh_cred.is_none() {
                    panic!("DbgSessionConfig ssh_cred is required for remote mode");
                }
            }
            self.ssh_cred
        };

        DbgSessionConfig {
            mode: mode.clone(),
            sudo: self.sudo,
            on_exit: self.on_exit,
            ssh_cred,
            tag: self.tag,
            prerun_gdb_cmds: self.prerun_gdb_cmds,
            postrun_gdb_cmds: self.postrun_gdb_cmds,
            service_info: self.service_info,
            gdb_controller: self.gdb_controller.unwrap(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DbgStartMode {
    ATTACH(u64),    // pid
    BINARY(String), // bin_path
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
