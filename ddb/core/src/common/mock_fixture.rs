//! Fixture topology for mock debugger sessions.
//!
//! These types describe the synthetic sessions the mock backend serves; they
//! are configuration for test and benchmark topologies, kept out of the core
//! application configuration surface.

use std::{collections::BTreeMap, net::Ipv4Addr};

use serde::{Deserialize, Serialize};

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
    /// Number of deterministic root variables returned for each frame.
    #[serde(default = "default_mock_variables_per_frame")]
    pub variables_per_frame: usize,
    #[serde(default)]
    pub executable: String,
    #[serde(default)]
    pub exit_on_continue: bool,
    #[serde(default)]
    pub exit_on_bootstrap: bool,
    #[serde(default)]
    pub reject_commands: Vec<String>,
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
            variables_per_frame: default_mock_variables_per_frame(),
            executable: String::new(),
            exit_on_continue: false,
            exit_on_bootstrap: false,
            reject_commands: Vec::new(),
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

fn default_mock_variables_per_frame() -> usize {
    2
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
