use crate::{
    common::default_vals::{
        DEFAULT_EMBEDDED_LLDB_BRIDGE_PATH, DEFAULT_LLDB_BRIDGE_NAME, DEFAULT_LLDB_EXT_DIR,
    },
    debugger::BundledDebuggerAsset,
};

pub const LLDB_BRIDGE_ASSET: BundledDebuggerAsset = BundledDebuggerAsset {
    embedded_path: DEFAULT_EMBEDDED_LLDB_BRIDGE_PATH,
    output_dir: DEFAULT_LLDB_EXT_DIR,
    file_name: DEFAULT_LLDB_BRIDGE_NAME,
};

pub fn lldb_start_command(sudo: bool) -> String {
    format!(
        "{}lldb --no-lldbinit --no-use-colors",
        if sudo { "sudo " } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_command_selects_a_clean_machine_runtime() {
        assert_eq!(
            lldb_start_command(false),
            "lldb --no-lldbinit --no-use-colors"
        );
        assert_eq!(
            lldb_start_command(true),
            "sudo lldb --no-lldbinit --no-use-colors"
        );
    }
}
