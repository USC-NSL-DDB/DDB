//! Shared configuration values. Loading a file never executes debugger commands
//! or opens source roots. Those effects belong to the owning host.

use anyhow::{ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceRoot {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub debugger_path: Option<String>,
    #[serde(default)]
    pub session_tags: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Gdb,
    Lldb,
    Mock,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NativeSetup {
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceMapping {
    pub from: String,
    pub to: String,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DebuggerConfig {
    #[serde(default)]
    pub backend: BackendKind,
    #[serde(default = "enabled")]
    pub eager_stack_warmup: bool,
    /// Enable GDB's MI visualizers. Installing trusted printer packages is explicit.
    #[serde(default = "enabled")]
    pub pretty_printing: bool,
    #[serde(default)]
    pub source_mappings: Vec<SourceMapping>,
    #[serde(default)]
    pub entry_function: Option<String>,
    #[serde(default)]
    pub gdb: NativeSetup,
    #[serde(default)]
    pub lldb: NativeSetup,
}

impl Default for DebuggerConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            eager_stack_warmup: true,
            pretty_printing: true,
            source_mappings: Vec::new(),
            entry_function: None,
            gdb: NativeSetup::default(),
            lldb: NativeSetup::default(),
        }
    }
}

impl DebuggerConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.source_mappings.len() <= 128,
            "at most 128 debugger source mappings are allowed"
        );
        for mapping in &self.source_mappings {
            validate_text(&mapping.from, "source mapping from", 4096)?;
            validate_text(&mapping.to, "source mapping to", 4096)?;
            ensure!(
                Path::new(&mapping.from).is_absolute() && Path::new(&mapping.to).is_absolute(),
                "debugger source mapping paths must be absolute on the debugger host"
            );
        }
        if let Some(function) = &self.entry_function {
            validate_text(function, "entry_function", 4096)?;
        }
        for setup in [&self.gdb, &self.lldb] {
            ensure!(
                setup.commands.len() <= 128,
                "at most 128 startup commands per backend are allowed"
            );
            for command in &setup.commands {
                validate_text(command, "startup command", 16_384)?;
            }
        }
        Ok(())
    }
}

fn validate_text(text: &str, field: &str, limit: usize) -> Result<()> {
    ensure!(
        !text.trim().is_empty() && text.len() <= limit && !text.chars().any(char::is_control),
        "{field} must be nonempty, bounded, and contain no control characters"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ValuesFormatting {
    PrettyPrinters,
    Raw,
    Disabled,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StackMode {
    Distributed,
    Local,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SourceSearch {
    Exact,
    Fuzzy,
}

/// Frontend-only settings. Never exported through DDB's source API.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    #[serde(default)]
    pub source_roots: Option<Vec<SourceRoot>>,
    pub values_formatting: Option<ValuesFormatting>,
    pub stack_mode: Option<StackMode>,
    pub source_search: Option<SourceSearch>,
    pub trace_api: Option<bool>,
    pub refresh_ms: Option<u64>,
    pub startup_timeout_secs: Option<u64>,
    pub watch_file: Option<PathBuf>,
    pub backend_log: Option<PathBuf>,
    pub ddb_path: Option<PathBuf>,
}

impl TuiConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.refresh_ms.is_none_or(|value| value > 0),
            "Tui.refresh_ms must be positive"
        );
        ensure!(
            self.startup_timeout_secs.is_none_or(|value| value > 0),
            "Tui.startup_timeout_secs must be positive"
        );
        if let Some(roots) = &self.source_roots {
            ensure!(roots.len() <= 32, "at most 32 TUI source roots are allowed");
            let mut names = std::collections::HashSet::new();
            for root in roots {
                validate_text(&root.name, "TUI source root name", 128)?;
                ensure!(
                    names.insert(&root.name),
                    "duplicate TUI source root {}",
                    root.name
                );
                ensure!(
                    !root.path.as_os_str().is_empty(),
                    "TUI source root path cannot be empty"
                );
            }
        }
        for path in [&self.watch_file, &self.backend_log, &self.ddb_path]
            .into_iter()
            .flatten()
        {
            ensure!(
                !path.as_os_str().is_empty(),
                "TUI file paths cannot be empty"
            );
        }
        Ok(())
    }

    /// Resolve frontend paths against their own containing file, not the CWD.
    pub fn resolve_paths(&mut self, base: &Path) {
        for root in self.source_roots.iter_mut().flatten() {
            resolve(&mut root.path, base);
        }
        for path in [
            &mut self.watch_file,
            &mut self.backend_log,
            &mut self.ddb_path,
        ]
        .into_iter()
        .flatten()
        {
            resolve(path, base);
        }
    }

    /// Explicit preferences replace each supplied field; source-root lists are atomic.
    pub fn overlay(&mut self, other: Self) {
        macro_rules! replace { ($($field:ident),*) => { $(if other.$field.is_some() { self.$field = other.$field; })* }; }
        replace!(
            source_roots,
            values_formatting,
            stack_mode,
            source_search,
            trace_api,
            refresh_ms,
            startup_timeout_secs,
            watch_file,
            backend_log,
            ddb_path
        );
    }
}

fn resolve(path: &mut PathBuf, base: &Path) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Read typed YAML from a bounded regular file without waiting for FIFO writers.
/// The caller supplies its format's size limit. No implicit project discovery.
pub fn read_yaml<T: DeserializeOwned>(path: &Path, max_bytes: usize) -> Result<T> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open configuration {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "configuration must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= max_bytes,
        "configuration exceeds {max_bytes} bytes"
    );
    serde_yml::from_slice(&bytes).with_context(|| format!("parse configuration {}", path.display()))
}

pub fn load_tui(path: &Path, project: bool) -> Result<TuiConfig> {
    let document: serde_yml::Value = read_yaml(path, MAX_CONFIG_BYTES)?;
    ensure!(
        document.is_mapping(),
        "configuration must be a YAML mapping"
    );
    let value = if project {
        document
            .get("Tui")
            .cloned()
            .unwrap_or(serde_yml::Value::Mapping(Default::default()))
    } else {
        document
    };
    let mut config: TuiConfig =
        serde_yml::from_value(value).context("invalid TUI configuration")?;
    config.validate()?;
    let canonical = path.canonicalize()?;
    config.resolve_paths(canonical.parent().context("configuration has no parent")?);
    Ok(config)
}

/// A dotted DDB key and a YAML value, passed as an argument, never shell code.
pub fn apply_override(
    document: &mut serde_yml::Value,
    assignment: &str,
    managed: bool,
) -> Result<()> {
    use serde_yml::Value;
    ensure!(
        assignment.len() <= 16_384 && !assignment.chars().any(char::is_control),
        "invalid configuration override"
    );
    let (key, value) = assignment
        .split_once('=')
        .context("override requires KEY=VALUE")?;
    let parts = key.split('.').collect::<Vec<_>>();
    ensure!(
        parts.len() <= 8
            && parts.iter().all(|part| !part.is_empty()
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
        "override key must be a bounded dotted field path"
    );
    ensure!(
        parts[0] != "Tui",
        "DDB overrides cannot change frontend settings; use TUI options"
    );
    if managed {
        ensure!(
            parts[0] != "Conf" || (parts.len() > 1 && !parts[1].starts_with("api_")),
            "managed DDB owns API transport and authentication; override {key} is not allowed"
        );
    }
    let parsed = serde_yml::from_str(value).context("override value must be YAML")?;
    let mut current = document;
    for part in &parts[..parts.len() - 1] {
        if current.is_null() {
            *current = Value::Mapping(Default::default());
        }
        let map = current
            .as_mapping_mut()
            .context("override path crosses a non-mapping value")?;
        current = map
            .entry(Value::String((*part).into()))
            .or_insert_with(|| Value::Mapping(Default::default()));
    }
    if current.is_null() {
        *current = Value::Mapping(Default::default());
    }
    current
        .as_mapping_mut()
        .context("override parent must be a mapping")?
        .insert(Value::String(parts.last().unwrap().to_string()), parsed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_yaml_reader_preserves_limits_and_rejects_nonregular_files() {
        let directory = tempfile::tempdir().unwrap();
        assert!(read_yaml::<serde_yml::Value>(directory.path(), MAX_CONFIG_BYTES).is_err());
        let path = directory.path().join("setup.yaml");
        let yaml = "commands: []";
        std::fs::write(&path, yaml).unwrap();
        let setup: NativeSetup = read_yaml(&path, yaml.len()).unwrap();
        assert!(setup.commands.is_empty());
        assert!(read_yaml::<NativeSetup>(&path, yaml.len() - 1).is_err());
        std::fs::write(&path, "command: []").unwrap();
        assert!(read_yaml::<NativeSetup>(&path, MAX_CONFIG_BYTES).is_err());
    }
    #[test]
    fn strict_tui_and_debugger_configuration() {
        assert!(serde_yml::from_str::<TuiConfig>("source_root: []").is_err());
        assert!(serde_yml::from_str::<TuiConfig>("stack_mode: invalid").is_err());
        assert!(serde_yml::from_str::<DebuggerConfig>("gdb: {command: foo}").is_err());
        let settings: DebuggerConfig = serde_yml::from_str("gdb: {commands: ['python print(1)']}\nlldb: {commands: ['settings set target.max-children-count 33']}").unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.gdb.commands.len(), 1);
    }
    #[test]
    fn paths_and_overlay_are_explicit() {
        let mut config: TuiConfig = serde_yml::from_str(
            "source_roots: [{name: app, path: src}]\nwatch_file: watches.json\ntrace_api: true",
        )
        .unwrap();
        config.resolve_paths(Path::new("/project"));
        assert_eq!(
            config.source_roots.as_ref().unwrap()[0].path,
            Path::new("/project/src")
        );
        assert_eq!(
            config.watch_file.as_deref(),
            Some(Path::new("/project/watches.json"))
        );
        config.overlay(serde_yml::from_str("source_roots: []\ntrace_api: false").unwrap());
        assert!(config.source_roots.unwrap().is_empty());
        assert_eq!(config.trace_api, Some(false));
    }
    #[test]
    fn overrides_preserve_types_and_managed_ownership() {
        let mut doc = serde_yml::from_str("Conf: {on_exit: detach}").unwrap();
        apply_override(&mut doc, "Conf.Debugger.backend=lldb", true).unwrap();
        assert_eq!(doc["Conf"]["Debugger"]["backend"].as_str(), Some("lldb"));
        for key in [
            "Conf={}",
            "Conf.api_server_port=80",
            "Tui.trace_api=true",
            "Conf..sudo=true",
        ] {
            assert!(apply_override(&mut doc, key, true).is_err(), "{key}");
        }
    }
    #[test]
    fn file_loader_separates_frontend_and_backend_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddb.yaml");
        std::fs::write(
            &path,
            "Framework: grpc\nTui:\n  source_roots: [{name: app, path: .}]\n  refresh_ms: 1000",
        )
        .unwrap();
        let config = load_tui(&path, true).unwrap();
        assert_eq!(config.refresh_ms, Some(1000));
        assert!(config.source_roots.unwrap()[0].path.is_absolute());
        assert!(load_tui(&path, false).is_err());
    }
}
