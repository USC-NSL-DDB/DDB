use anyhow::Result;

use crate::{
    common::default_vals::{
        DEFAULT_EMBEDED_GDB_EXT_FRAME_FILTER_PATH, DEFAULT_EMBEDED_GDB_EXT_PATH,
        DEFAULT_GDB_EXT_DIR, DEFAULT_GDB_EXT_FRAME_FILTER_NAME, DEFAULT_GDB_EXT_NAME,
        DEFAULT_MI_VERSION, EMBEDED_PROCLET_GDB_EXT_PATH, PROCLET_GDB_EXT_NAME,
    },
    debugger::{install_bundled_asset, BundledDebuggerAsset},
};

pub const CORE_GDB_RUNTIME_ASSET: BundledDebuggerAsset = BundledDebuggerAsset {
    embedded_path: DEFAULT_EMBEDED_GDB_EXT_PATH,
    output_dir: DEFAULT_GDB_EXT_DIR,
    file_name: DEFAULT_GDB_EXT_NAME,
};

pub const FRAME_FILTER_GDB_RUNTIME_ASSET: BundledDebuggerAsset = BundledDebuggerAsset {
    embedded_path: DEFAULT_EMBEDED_GDB_EXT_FRAME_FILTER_PATH,
    output_dir: DEFAULT_GDB_EXT_DIR,
    file_name: DEFAULT_GDB_EXT_FRAME_FILTER_NAME,
};

pub const PROCLET_GDB_RUNTIME_ASSET: BundledDebuggerAsset = BundledDebuggerAsset {
    embedded_path: EMBEDED_PROCLET_GDB_EXT_PATH,
    output_dir: DEFAULT_GDB_EXT_DIR,
    file_name: PROCLET_GDB_EXT_NAME,
};

pub fn get_default_mi_arg() -> String {
    format!("--interpreter={}", DEFAULT_MI_VERSION)
}

pub fn gdb_start_cmd(sudo: bool) -> String {
    format!(
        "{} gdb {} -q",
        if sudo { "sudo" } else { "" },
        get_default_mi_arg()
    )
}

#[allow(dead_code)]
pub struct GdbStartCmdBuilder {
    sudo: bool,
    mi_version: Option<String>,
    quite: bool,
}

#[allow(dead_code)]
impl GdbStartCmdBuilder {
    pub fn new() -> Self {
        Self {
            sudo: false,
            mi_version: None,
            quite: false,
        }
    }

    pub fn sudo(mut self, sudo: bool) -> Self {
        self.sudo = sudo;
        self
    }

    pub fn mi_version(mut self, mi_version: &str) -> Self {
        self.mi_version = Some(mi_version.to_string());
        self
    }

    pub fn quite(mut self, quite: bool) -> Self {
        self.quite = quite;
        self
    }

    pub fn build(self) -> String {
        let mi_version = self
            .mi_version
            .unwrap_or_else(|| DEFAULT_MI_VERSION.to_string());
        let mi_arg = format!("--interpreter={}", mi_version);
        let quite_arg = if self.quite { "-q" } else { "" };
        format!(
            "{} gdb {} {}",
            if self.sudo { "sudo" } else { "" },
            mi_arg,
            quite_arg
        )
    }
}

#[allow(dead_code)]
pub fn setup_gdb_ext_script() -> Result<std::path::PathBuf> {
    install_bundled_asset(&CORE_GDB_RUNTIME_ASSET)
}

#[allow(dead_code)]
pub fn setup_gdb_ext_frame_filter_script() -> Result<std::path::PathBuf> {
    install_bundled_asset(&FRAME_FILTER_GDB_RUNTIME_ASSET)
}

#[allow(dead_code)]
pub fn setup_proclet_ext_script() -> Result<std::path::PathBuf> {
    install_bundled_asset(&PROCLET_GDB_RUNTIME_ASSET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gdb_start_cmd() {
        let cmd = gdb_start_cmd(true);
        assert_eq!(cmd, "sudo gdb --interpreter=mi3 -q");
    }

    #[test]
    fn test_gdb_start_cmd_builder() {
        let cmd = GdbStartCmdBuilder::new().sudo(true).quite(true).build();
        assert_eq!(cmd.trim(), "sudo gdb --interpreter=mi3 -q");

        let cmd = GdbStartCmdBuilder::new().sudo(true).quite(false).build();
        assert_eq!(cmd.trim(), "sudo gdb --interpreter=mi3");

        let cmd = GdbStartCmdBuilder::new().sudo(false).quite(false).build();
        assert_eq!(cmd.trim(), "gdb --interpreter=mi3");
    }

    #[test]
    fn test_setup_gdb_ext_script() {
        let temp_dir = std::path::Path::new("/tmp/ddb/gdb_ext");
        std::fs::create_dir_all(temp_dir).expect("Failed to create /tmp/ddb/gdb_ext");

        let path = setup_gdb_ext_script().unwrap();
        assert!(path.exists());

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let assets_path = std::path::Path::new(manifest_dir)
            .join("assets")
            .join(DEFAULT_EMBEDED_GDB_EXT_PATH);
        let expected = std::fs::read_to_string(assets_path)
            .expect("Failed to read assets/gdb_ext/runtime-gdb.py");
        assert!(!expected.is_empty(), "gdb extension script is empty");

        let actual = std::fs::read_to_string(&path)
            .expect("Failed to read written out gdb extension script");
        assert_eq!(actual, expected);
    }
}
