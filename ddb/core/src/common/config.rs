use super::default_vals;
use super::mock_fixture::MockSessionConfig;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Config {
    #[serde(rename = "PreTasks", default)]
    pub pre_tasks: Vec<Task>,

    #[serde(rename = "PostTasks", default)]
    pub post_tasks: Vec<Task>,

    #[serde(rename = "Framework", default)]
    pub framework: Framework,

    #[serde(
        rename = "PrerunDebuggerCommands",
        alias = "PrerunGdbCommands",
        default
    )]
    pub prerun_debugger_cmds: Vec<DebuggerCommand>,

    #[serde(
        rename = "PostrunDebuggerCommands",
        alias = "PostrunGdbCommands",
        default
    )]
    pub postrun_debugger_cmds: Vec<DebuggerCommand>,

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameFilterMatchType {
    Exact,
    Glob,
    Regex,
}

impl FrameFilterMatchType {
    pub fn as_str(&self) -> &str {
        match self {
            FrameFilterMatchType::Exact => "exact",
            FrameFilterMatchType::Glob => "glob",
            FrameFilterMatchType::Regex => "regex",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FrameFilterPatternConfig {
    #[serde(rename = "pattern")]
    pub pattern: String,
    #[serde(rename = "match_type")]
    pub match_type: FrameFilterMatchType,
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
    /// SSH password for the pods reached through the jump host.
    #[serde(default = "default_pod_ssh_password")]
    pub pod_ssh_password: String,
}

fn default_pod_ssh_password() -> String {
    "admin123".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct GdbConf {
    #[serde(default)]
    pub logging: bool,
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
    /// Optional session-specific shutdown policy. When omitted, Conf.on_exit
    /// remains the backward-compatible default for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<OnExit>,
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
            on_exit: None,
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
    Lldb,
    Mock,
    #[serde(other)]
    Unknown,
}

impl Default for DebuggerBackendKind {
    fn default() -> Self {
        Self::Gdb
    }
}

fn default_eager_stack_warmup() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DebuggerConf {
    #[serde(default)]
    pub backend: DebuggerBackendKind,
    #[serde(default = "default_eager_stack_warmup")]
    pub eager_stack_warmup: bool,
}

impl Default for DebuggerConf {
    fn default() -> Self {
        Self {
            backend: DebuggerBackendKind::Gdb,
            eager_stack_warmup: true,
        }
    }
}

fn default_auto_shutdown() -> bool {
    true
}

fn default_api_server_bind() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn default_api_max_concurrent_requests() -> usize {
    128
}

fn default_api_requests_per_second() -> u32 {
    1_000
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ApiResourceLimits {
    /// Replayable state events retained before the oldest event is evicted.
    #[serde(default = "default_api_state_replay_events")]
    pub state_replay_events: usize,
    /// Total encoded bytes retained by the state-event journal.
    #[serde(default = "default_api_state_replay_bytes")]
    pub state_replay_bytes: usize,
    /// Maximum age of replayable state events.
    #[serde(default = "default_api_state_replay_retention_millis")]
    pub state_replay_retention_millis: u64,
    /// Per-client state-event queue capacity.
    #[serde(default = "default_api_state_subscriber_queue")]
    pub state_subscriber_queue: usize,
    /// Per-client debugger-output queue capacity. Output is not replayed.
    #[serde(default = "default_api_output_subscriber_queue")]
    pub output_subscriber_queue: usize,
    /// Maximum subscribers admitted independently by each stream lane.
    #[serde(default = "default_api_max_subscribers")]
    pub max_subscribers: usize,
    /// Retained operation-record count.
    #[serde(default = "default_api_operation_records")]
    pub operation_records: usize,
    /// Total reserved bytes for retained operation records.
    #[serde(default = "default_api_operation_bytes")]
    pub operation_bytes: usize,
    /// Encoded byte bound for one retained operation record.
    #[serde(default = "default_api_operation_record_bytes")]
    pub operation_record_bytes: usize,
    /// Maximum age of terminal operation and idempotency records.
    #[serde(default = "default_api_operation_retention_millis")]
    pub operation_retention_millis: u64,
    /// UTF-8 byte bound for one debugger-output event before truncation.
    #[serde(default = "default_api_output_event_bytes")]
    pub output_event_bytes: usize,
}

impl Default for ApiResourceLimits {
    fn default() -> Self {
        Self {
            state_replay_events: default_api_state_replay_events(),
            state_replay_bytes: default_api_state_replay_bytes(),
            state_replay_retention_millis: default_api_state_replay_retention_millis(),
            state_subscriber_queue: default_api_state_subscriber_queue(),
            output_subscriber_queue: default_api_output_subscriber_queue(),
            max_subscribers: default_api_max_subscribers(),
            operation_records: default_api_operation_records(),
            operation_bytes: default_api_operation_bytes(),
            operation_record_bytes: default_api_operation_record_bytes(),
            operation_retention_millis: default_api_operation_retention_millis(),
            output_event_bytes: default_api_output_event_bytes(),
        }
    }
}

fn default_api_state_replay_events() -> usize {
    10_000
}

fn default_api_state_replay_bytes() -> usize {
    32 * 1024 * 1024
}

fn default_api_state_replay_retention_millis() -> u64 {
    5 * 60 * 1_000
}

fn default_api_state_subscriber_queue() -> usize {
    1_024
}

fn default_api_output_subscriber_queue() -> usize {
    2_048
}

fn default_api_max_subscribers() -> usize {
    20
}

fn default_api_operation_records() -> usize {
    1_024
}

fn default_api_operation_bytes() -> usize {
    64 * 1024 * 1024
}

fn default_api_operation_record_bytes() -> usize {
    64 * 1024
}

fn default_api_operation_retention_millis() -> u64 {
    15 * 60 * 1_000
}

fn default_api_output_event_bytes() -> usize {
    256 * 1024
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
    /// Address for the HTTP API listener. Remote addresses are rejected by
    /// deployment-policy validation unless their security prerequisites are
    /// configured explicitly.
    #[serde(default = "default_api_server_bind")]
    pub api_server_bind: IpAddr,
    /// Optional loopback-only native gRPC preview listener. The preview is
    /// compiled only with the `grpc-preview` Cargo feature.
    #[serde(default)]
    pub api_grpc_preview_port: Option<u16>,
    /// JSON bearer-token file for the v2 API. Tokens stay out of the main
    /// configuration and are reduced to hashes when loaded.
    #[serde(default)]
    pub api_auth_token_file: Option<String>,
    /// Explicit local-development escape hatch. Never enables remote binding.
    #[serde(default)]
    pub api_insecure_allow_unauthenticated_v2: bool,
    /// Assert that the listener is reachable only through a trusted reverse
    /// proxy that terminates TLS. DDB bearer authentication remains mandatory
    /// for production remote binds.
    #[serde(default)]
    pub api_tls_terminated_by_trusted_proxy: bool,
    /// Explicit development-only bypass for the remote transport-security
    /// check. Authentication remains independently controlled.
    #[serde(default)]
    pub api_insecure_allow_remote: bool,
    /// Exact browser origins allowed to call the API. An empty list rejects
    /// every request carrying an Origin header.
    #[serde(default)]
    pub api_cors_allowed_origins: Vec<String>,
    /// Listener-wide admission limits. Streaming subscriber limits are
    /// enforced separately by the application service.
    #[serde(default = "default_api_max_concurrent_requests")]
    pub api_max_concurrent_requests: usize,
    #[serde(default = "default_api_requests_per_second")]
    pub api_requests_per_second: u32,
    /// Bounded API-owned journals, queues, and retained operation state.
    #[serde(rename = "ApiLimits", alias = "api_limits", default)]
    pub api_limits: ApiResourceLimits,
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
            api_server_bind: default_api_server_bind(),
            api_grpc_preview_port: None,
            api_auth_token_file: None,
            api_insecure_allow_unauthenticated_v2: false,
            api_tls_terminated_by_trusted_proxy: false,
            api_insecure_allow_remote: false,
            api_cors_allowed_origins: Vec::new(),
            api_max_concurrent_requests: default_api_max_concurrent_requests(),
            api_requests_per_second: default_api_requests_per_second(),
            api_limits: ApiResourceLimits::default(),
            log_dir: default_vals::DEFAULT_LOG_DIR.to_string(),
            base_dir: default_vals::DEFAULT_BASE_DIR.to_string(),
            support_migration: false, // TODO: default to true when testing is done.
            debugger: DebuggerConf::default(),
            gdb: GdbConf::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
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
#[allow(clippy::upper_case_acronyms)]
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

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrokerType {
    #[default]
    Emqx,
    Mosquitto,
    #[serde(other)]
    Unknown,
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

impl Config {
    /// Load configuration from a YAML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let contents = fs::read_to_string(path)?;
        Self::from_str(&contents)
    }

    /// Parse configuration from a YAML string
    pub fn from_str(contents: &str) -> Result<Self> {
        let config = serde_yml::from_str(contents)?;
        Ok(config)
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
    fn api_resource_limits_have_safe_defaults_and_allow_partial_overrides() {
        let defaults = Config::default().conf.api_limits;
        assert_eq!(defaults.state_replay_events, 10_000);
        assert_eq!(defaults.state_replay_retention_millis, 300_000);
        assert_eq!(defaults.max_subscribers, 20);

        let config = Config::from_str(
            r#"
Conf:
  ApiLimits:
    state_replay_events: 37
    max_subscribers: 3
    output_event_bytes: 4096
"#,
        )
        .expect("partial API limit overrides should parse");
        assert_eq!(config.conf.api_limits.state_replay_events, 37);
        assert_eq!(config.conf.api_limits.max_subscribers, 3);
        assert_eq!(config.conf.api_limits.output_event_bytes, 4_096);
        assert_eq!(
            config.conf.api_limits.state_replay_bytes,
            defaults.state_replay_bytes
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
    fn legacy_gdb_command_keys_deserialize_into_backend_neutral_fields() {
        let config = Config::from_str(
            r#"
PrerunGdbCommands:
  - name: before
    command: before-command
PostrunGdbCommands:
  - name: after
    command: after-command
"#,
        )
        .expect("legacy debugger command keys should remain compatible");

        assert_eq!(config.prerun_debugger_cmds[0].command, "before-command");
        assert_eq!(config.postrun_debugger_cmds[0].command, "after-command");

        let serialized = serde_yml::to_string(&config).expect("config should serialize");
        assert!(serialized.contains("PrerunDebuggerCommands:"));
        assert!(serialized.contains("PostrunDebuggerCommands:"));
        assert!(!serialized.contains("PrerunGdbCommands:"));
        assert!(!serialized.contains("PostrunGdbCommands:"));
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

    #[test]
    fn lldb_backend_parses_from_yaml() {
        let config = Config::from_str(
            r#"
Conf:
  Debugger:
    backend: lldb
    eager_stack_warmup: false
"#,
        )
        .expect("LLDB configuration should parse");

        assert_eq!(config.conf.debugger.backend, DebuggerBackendKind::Lldb);
        assert!(!config.conf.debugger.eager_stack_warmup);
        assert!(DebuggerConf::default().eager_stack_warmup);
    }

    #[test]
    fn static_session_on_exit_is_optional_and_overrides_parse() {
        let config = Config::from_str(
            r#"
Conf:
  on_exit: detach
  Debugger:
    backend: mock
StaticSessions:
  - tag: inherits
    pid: 1
  - tag: owned
    pid: 2
    on_exit: kill
"#,
        )
        .expect("per-session lifecycle configuration should parse");

        assert_eq!(config.static_sessions[0].on_exit, None);
        assert_eq!(config.static_sessions[1].on_exit, Some(OnExit::KILL));

        let serialized = serde_yml::to_string(&config).expect("configuration should serialize");
        assert_eq!(serialized.matches("on_exit: kill").count(), 1);
    }
}
