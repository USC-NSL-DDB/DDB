use super::default_vals;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

use crate::debugger::gdb::command::{FrameFilterAddArgs, FrameFilterMatchType};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(rename = "PreTasks", default)]
    pub pre_tasks: Vec<Task>,

    #[serde(rename = "PostTasks", default)]
    pub post_tasks: Vec<Task>,

    #[serde(rename = "Framework", default)]
    pub framework: Framework,

    #[serde(rename = "PrerunGdbCommands", default)]
    pub prerun_gdb_cmds: Vec<GdbCommand>,

    #[serde(rename = "PostrunGdbCommands", default)]
    pub postrun_gdb_cmds: Vec<GdbCommand>,

    #[serde(rename = "SSH", default)]
    pub ssh: SshConfig,

    #[serde(rename = "ServiceDiscovery", default)]
    pub service_discovery: Option<ServiceDiscovery>,

    #[serde(rename = "Conf", default)]
    pub conf: Conf,

    #[serde(rename = "Plugin", default)]
    pub plugin: Option<PluginConfig>,

    #[serde(rename = "FrameFilter", default)]
    pub frame_filter: Option<FrameFilterConfig>,

    #[serde(rename = "StaticSessions", default)]
    pub static_sessions: Vec<StaticSessionConfig>,
}

impl Config {
    pub fn handle_migration(&self) -> bool {
        match self.framework {
            Framework::Nu | Framework::Quicksand => self.conf.support_migration,
            _ => false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FrameFilterPatternConfig {
    #[serde(rename = "pattern")]
    pattern: String,
    #[serde(rename = "match_type")]
    match_type: FrameFilterMatchType,
}

impl From<FrameFilterPatternConfig> for FrameFilterAddArgs {
    fn from(config: FrameFilterPatternConfig) -> Self {
        FrameFilterAddArgs::new(&config.pattern, config.match_type)
    }
}

impl From<&FrameFilterPatternConfig> for FrameFilterAddArgs {
    fn from(config: &FrameFilterPatternConfig) -> Self {
        FrameFilterAddArgs::new(&config.pattern, config.match_type.clone())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FrameFilterConfig {
    #[serde(default)]
    pub filter_file: Vec<FrameFilterPatternConfig>,
    #[serde(default)]
    pub filter_function: Vec<FrameFilterPatternConfig>,
    #[serde(default)]
    pub filter_preset: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServiceWeaverConf {
    pub service_name: String,
    pub kubectl_config_path: String,
    pub jump_client_host: String,
    pub jump_client_port: u16,
    pub jump_client_user: String,
    pub jump_client_password: String,
    pub jump_client_key_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct GdbConf {
    #[serde(default)]
    pub logging: bool,
}

impl Default for GdbConf {
    fn default() -> Self {
        Self { logging: false }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MockThreadConfig {
    #[serde(default = "default_mock_thread_id")]
    pub id: u64,
    #[serde(default = "default_mock_thread_name")]
    pub name: String,
}

impl Default for MockThreadConfig {
    fn default() -> Self {
        Self {
            id: default_mock_thread_id(),
            name: default_mock_thread_name(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MockSessionConfig {
    #[serde(default = "default_mock_thread_group")]
    pub thread_group: String,
    #[serde(default = "default_mock_threads")]
    pub threads: Vec<MockThreadConfig>,
    #[serde(default = "default_mock_source_file")]
    pub source_file: String,
    #[serde(default = "default_mock_source_line")]
    pub source_line: u64,
    #[serde(default = "default_mock_function")]
    pub function: String,
    #[serde(default)]
    pub executable: String,
    #[serde(default)]
    pub exit_on_continue: bool,
    #[serde(default)]
    pub exit_on_bootstrap: bool,
    #[serde(default)]
    pub stack_frames: Vec<MockStackFrameConfig>,
    #[serde(default)]
    pub dbt_parent: Option<MockDbtParentConfig>,
    #[serde(default = "default_mock_context_regs")]
    pub context_regs: BTreeMap<String, u64>,
}

impl Default for MockSessionConfig {
    fn default() -> Self {
        Self {
            thread_group: default_mock_thread_group(),
            threads: default_mock_threads(),
            source_file: default_mock_source_file(),
            source_line: default_mock_source_line(),
            function: default_mock_function(),
            executable: String::new(),
            exit_on_continue: false,
            exit_on_bootstrap: false,
            stack_frames: Vec::new(),
            dbt_parent: None,
            context_regs: default_mock_context_regs(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MockStackFrameConfig {
    #[serde(default = "default_mock_stack_frame_function")]
    pub function: String,
    #[serde(default = "default_mock_stack_frame_file")]
    pub file: String,
    #[serde(default = "default_mock_stack_frame_line")]
    pub line: u64,
}

impl Default for MockStackFrameConfig {
    fn default() -> Self {
        Self {
            function: default_mock_stack_frame_function(),
            file: default_mock_stack_frame_file(),
            line: default_mock_stack_frame_line(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MockDbtParentConfig {
    pub ip: Ipv4Addr,
    pub pid: u64,
    #[serde(default = "default_mock_dbt_parent_tid")]
    pub tid: u64,
    #[serde(default)]
    pub proclet_id: String,
    #[serde(default = "default_mock_dbt_parent_context")]
    pub caller_ctx: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StaticSessionConfig {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub pid: u64,
    #[serde(default = "default_static_session_ip")]
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub start_delay_ms: u64,
    #[serde(default)]
    pub start_mode: StaticSessionStartMode,
    #[serde(default)]
    pub binary_path: String,
    #[serde(default)]
    pub binary_args: Vec<String>,
    #[serde(default)]
    pub stop_at_entry: bool,
    #[serde(default)]
    pub mock: MockSessionConfig,
}

impl Default for StaticSessionConfig {
    fn default() -> Self {
        Self {
            tag: String::new(),
            alias: String::new(),
            hash: String::new(),
            pid: 0,
            ip: default_static_session_ip(),
            start_delay_ms: 0,
            start_mode: StaticSessionStartMode::default(),
            binary_path: String::new(),
            binary_args: Vec::new(),
            stop_at_entry: false,
            mock: MockSessionConfig::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StaticSessionStartMode {
    Attach,
    Binary,
}

impl Default for StaticSessionStartMode {
    fn default() -> Self {
        Self::Attach
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DebuggerBackendKind {
    Gdb,
    Mock,
    #[serde(other)]
    Unknown,
}

impl Default for DebuggerBackendKind {
    fn default() -> Self {
        Self::Gdb
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DebuggerConf {
    #[serde(default)]
    pub backend: DebuggerBackendKind,
}

impl Default for DebuggerConf {
    fn default() -> Self {
        Self {
            backend: DebuggerBackendKind::Gdb,
        }
    }
}

fn default_auto_shutdown() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Conf {
    #[serde(default)]
    pub sudo: bool,
    #[serde(default)]
    pub on_exit: OnExit,
    #[serde(default = "default_auto_shutdown")]
    pub auto_shutdown: bool, // whether to auto shutdown the DDB when all debuggee processes exit.
    #[serde(default = "default_api_svr_port")]
    pub api_server_port: u16,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    #[serde(default = "default_base_dir")]
    pub base_dir: String,
    #[serde(default)]
    pub support_migration: bool,
    #[serde(rename = "Debugger", default)]
    pub debugger: DebuggerConf,
    #[serde(default)]
    pub gdb: GdbConf,
}

impl Default for Conf {
    fn default() -> Self {
        Self {
            sudo: false,
            on_exit: OnExit::default(),
            auto_shutdown: true,
            api_server_port: default_vals::DEFAULT_API_SVR_PORT,
            log_dir: default_vals::DEFAULT_LOG_DIR.to_string(),
            base_dir: default_vals::DEFAULT_BASE_DIR.to_string(),
            support_migration: false, // TODO: default to true when testing is done.
            debugger: DebuggerConf::default(),
            gdb: GdbConf::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnExit {
    DETACH,
    KILL,
}

impl Default for OnExit {
    fn default() -> Self {
        Self::DETACH
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    Nu,
    Quicksand,
    ServiceWeaverKube,
    GRPC,
    #[serde(other)]
    Unspecified,
}

impl Default for Framework {
    fn default() -> Self {
        default_vals::DEFAULT_FRAMEWORK
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Task {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct DebuggerCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
}

impl DebuggerCommand {
    pub fn render(&self) -> String {
        let command = self.command.trim();
        if command.ends_with('\n') {
            command.to_string()
        } else {
            format!("{}\n", command)
        }
    }
}

pub type GdbCommand = DebuggerCommand;

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PluginConfig {
    #[serde(rename = "DebuggerScripts", default)]
    pub debugger_scripts: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SshConfig {
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ServiceDiscovery {
    #[serde(rename = "Broker", default)]
    pub broker: BrokerConfig,
    #[serde(rename = "ConfigPath", default = "default_sd_config_path")]
    pub config_path: String,
    #[serde(rename = "ServiceWeaverConf", default)]
    pub service_weaver_conf: Option<ServiceWeaverConf>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct BrokerConfig {
    #[serde(default = "default_broker_hostname")]
    pub hostname: String,
    #[serde(default = "default_broker_port")]
    pub port: u16,
    #[serde(default)]
    pub managed: Option<ManagedBrokerConfig>,
    #[serde(default)]
    pub max_timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BrokerType {
    Emqx,
    Mosquitto,
    #[serde(other)]
    Unknown,
}

impl Default for BrokerType {
    fn default() -> Self {
        BrokerType::Emqx
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ManagedBrokerConfig {
    #[serde(rename = "type", default)]
    pub broker_type: BrokerType,
    #[serde(rename = "emqx_flags", default)]
    pub emqx_flags: Vec<String>,
    #[serde(rename = "emqx_image", default = "default_emqx_image")]
    pub emqx_image: String,
    #[serde(rename = "config_path", default)]
    pub config_path: Option<String>,
}

fn default_emqx_image() -> String {
    "emqx/emqx:5.8.4".to_string()
}

fn default_ssh_user() -> String {
    default_vals::DEFAULT_SSH_USER.clone()
}

fn default_ssh_port() -> u16 {
    default_vals::DEFAULT_SSH_PORT
}

fn default_api_svr_port() -> u16 {
    default_vals::DEFAULT_API_SVR_PORT
}

fn default_log_dir() -> String {
    default_vals::DEFAULT_LOG_DIR.to_string()
}

fn default_base_dir() -> String {
    default_vals::DEFAULT_BASE_DIR.to_string()
}

fn default_static_session_ip() -> Ipv4Addr {
    Ipv4Addr::new(127, 0, 0, 1)
}

fn default_mock_thread_id() -> u64 {
    1
}

fn default_mock_thread_name() -> String {
    "main".to_string()
}

fn default_mock_thread_group() -> String {
    "i1".to_string()
}

fn default_mock_threads() -> Vec<MockThreadConfig> {
    vec![MockThreadConfig::default()]
}

fn default_mock_source_file() -> String {
    "main.rs".to_string()
}

fn default_mock_source_line() -> u64 {
    1
}

fn default_mock_function() -> String {
    "main".to_string()
}

fn default_mock_stack_frame_function() -> String {
    "main".to_string()
}

fn default_mock_stack_frame_file() -> String {
    "main.rs".to_string()
}

fn default_mock_stack_frame_line() -> u64 {
    1
}

fn default_mock_dbt_parent_tid() -> u64 {
    1
}

fn default_mock_context_regs() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("pc".to_string(), 0x401000),
        ("sp".to_string(), 0x7fff_0000),
        ("fp".to_string(), 0x7fff_1000),
    ])
}

fn default_mock_dbt_parent_context() -> BTreeMap<String, u64> {
    default_mock_context_regs()
}

fn default_broker_hostname() -> String {
    use super::sd_defaults;
    sd_defaults::DEFAULT_BROKER_HOSTNAME.to_string()
}

fn default_broker_port() -> u16 {
    use super::sd_defaults;
    sd_defaults::BROKER_PORT
}

fn default_sd_config_path() -> String {
    use super::sd_defaults;
    sd_defaults::SERVICE_DISCOVERY_INI_FILEPATH.to_string()
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            user: default_ssh_user(),
            port: default_ssh_port(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pre_tasks: Vec::new(),
            post_tasks: Vec::new(),
            framework: Framework::default(),
            prerun_gdb_cmds: Vec::new(),
            postrun_gdb_cmds: Vec::new(),
            ssh: SshConfig::default(),
            service_discovery: None,
            conf: Conf::default(),
            plugin: None,
            frame_filter: None,
            static_sessions: Vec::new(),
        }
    }
}

#[allow(dead_code)]
impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load configuration from a YAML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let contents = fs::read_to_string(path)?;
        let conf = Self::from_str(&contents);
        conf
    }

    /// Parse configuration from a YAML string
    pub fn from_str(contents: &str) -> Result<Self> {
        let config = serde_yml::from_str(contents)?;
        Ok(config)
    }

    /// Save configuration to a YAML file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let yaml = serde_yml::to_string(self)?;
        fs::write(path, yaml)?;
        Ok(())
    }

    /// Load configuration from a path, or use defaults when no path is supplied.
    pub fn load<P: AsRef<Path>>(path: Option<P>) -> Result<Self> {
        match path {
            Some(path) => Self::from_file(path),
            None => Ok(Self::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn load_repo_config(file_name: &str) -> Config {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("configs")
            .join(file_name);
        Config::from_file(path).expect("config in repository should parse")
    }

    #[test]
    fn lower_case_framework_values_in_repo_configs_parse_correctly() {
        let quicksand = Config::from_str("Framework: quicksand\n")
            .expect("lower-case quicksand framework should parse");
        assert_eq!(quicksand.framework, Framework::Quicksand);

        let nu = Config::from_str("Framework: nu\n").expect("lower-case nu framework should parse");
        assert_eq!(nu.framework, Framework::Nu);

        assert_eq!(
            load_repo_config("dbg_pass_signal.yaml").framework,
            Framework::GRPC
        );
        assert_eq!(
            load_repo_config("serviceweaverConfig.yaml").framework,
            Framework::ServiceWeaverKube
        );
        assert_eq!(
            load_repo_config("pidconfig.yaml").framework,
            Framework::Unspecified
        );
    }

    #[test]
    fn handle_migration_only_applies_to_supported_frameworks() {
        let mut config = Config::default();
        config.conf.support_migration = true;

        config.framework = Framework::Nu;
        assert!(config.handle_migration());

        config.framework = Framework::Quicksand;
        assert!(config.handle_migration());

        config.framework = Framework::GRPC;
        assert!(!config.handle_migration());

        config.framework = Framework::ServiceWeaverKube;
        assert!(!config.handle_migration());
    }

    #[test]
    fn debugger_command_render_trims_and_ensures_single_trailing_newline() {
        let cmd = DebuggerCommand {
            name: "load".to_string(),
            command: "  source /tmp/runtime.py  \n".to_string(),
        };

        assert_eq!(cmd.render(), "source /tmp/runtime.py\n");
    }

    #[test]
    fn frame_filter_pattern_conversion_preserves_match_type() {
        let pattern = FrameFilterPatternConfig {
            pattern: "runtime::*".to_string(),
            match_type: FrameFilterMatchType::Glob,
        };

        let add_args: FrameFilterAddArgs = (&pattern).into();
        assert_eq!(add_args.to_string(), "runtime::* --match-type glob");
    }

    #[test]
    fn mock_backend_and_static_sessions_parse_from_yaml() {
        let config = Config::from_str(
            r#"
Conf:
  Debugger:
    backend: mock
StaticSessions:
  - tag: svc-a
    alias: api
    hash: grp-a
    pid: 101
    start_delay_ms: 25
    mock:
      source_file: src/main.rs
      source_line: 44
      function: worker
      exit_on_continue: true
"#,
        )
        .expect("mock test configuration should parse");

        assert_eq!(config.conf.debugger.backend, DebuggerBackendKind::Mock);
        assert_eq!(config.static_sessions.len(), 1);
        assert_eq!(config.static_sessions[0].tag, "svc-a");
        assert_eq!(config.static_sessions[0].start_delay_ms, 25);
        assert_eq!(config.static_sessions[0].mock.source_line, 44);
        assert!(config.static_sessions[0].mock.exit_on_continue);
    }

    #[test]
    fn static_binary_session_parses_launch_configuration() {
        let config = Config::from_str(
            r#"
Conf:
  Debugger:
    backend: gdb
StaticSessions:
  - tag: real-a
    alias: real-a
    hash: grp-real
    start_mode: binary
    binary_path: /tmp/ddb-real-example
    stop_at_entry: true
    binary_args:
      - --mode
      - loop
"#,
        )
        .expect("static binary configuration should parse");

        assert_eq!(config.static_sessions.len(), 1);
        assert_eq!(
            config.static_sessions[0].start_mode,
            StaticSessionStartMode::Binary
        );
        assert_eq!(
            config.static_sessions[0].binary_path,
            "/tmp/ddb-real-example"
        );
        assert_eq!(
            config.static_sessions[0].binary_args,
            vec!["--mode".to_string(), "loop".to_string()]
        );
        assert!(config.static_sessions[0].stop_at_entry);
    }
}
