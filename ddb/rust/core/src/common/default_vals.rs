use super::config::Framework;
use super::utils::expand_path;
use std::{env, path::PathBuf};

pub const DEFAULT_BASE_DIR: &str = "/tmp/ddb";
pub const DEFAULT_LOG_DIR: &str = "/tmp/ddb/logs";
pub const DEFAULT_SERVICE_DISCOVER_CONF_DIR: &str = "/tmp/ddb/service_discovery";

pub const DEFAULT_API_SVR_PORT: u16 = 5000;

pub const DEFAULT_EMBEDED_GDB_EXT_PATH: &str = "gdb_ext/runtime-gdb.py";
pub const DEFAULT_GDB_EXT_DIR: &str = "/tmp/ddb/gdb_ext";
pub const DEFAULT_GDB_EXT_NAME: &str = "runtime-gdb.py";

pub const DEFAULT_MI_VERSION: &str = "mi3";

pub const DEFAULT_FRAMEWORK: Framework = Framework::Nu;

lazy_static::lazy_static! {
    pub static ref DEFAULT_SSH_USER: String = env::var("USER").unwrap_or_else(|_| "root".to_string());
}
pub const DEFAULT_SSH_PORT: u16 = 22;
lazy_static::lazy_static! {
    pub static ref DEFAULT_SSH_PRIVATE_KEY_PATH: PathBuf = expand_path("~/.ssh/id_rsa");
}
