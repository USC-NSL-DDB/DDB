pub struct LaunchOption {
    mi_debugger_path: &'static str,
    args: LaunchOptionArgs
}

impl LaunchOption {
    pub fn new(
        mi_debugger_path: &'static str, 
        args: &'static [&'static str]
    ) -> LaunchOption {
        LaunchOption {
            mi_debugger_path,
            args: LaunchOptionArgs::new(args)
        }
    }

    pub fn get_args(&self) -> Vec<&str> {
        self.args.get_args()
    }

    pub fn get_debugger_path(&self) -> &str {
        self.mi_debugger_path
    }
}

impl Default for LaunchOption {
    fn default() -> Self {
        LaunchOption {
            mi_debugger_path: "gdb",
            args: Default::default()
        }
    }
}

struct LaunchOptionArgs {
    args: &'static [&'static str]
}

impl LaunchOptionArgs {
    pub fn new(args: &'static [&'static str]) -> LaunchOptionArgs {
        LaunchOptionArgs { args }
    }

    pub fn get_args(&self) -> Vec<&str> {
        let mut arg_vec = self.args.to_vec();
        arg_vec.push("--interpreter=mi");
        arg_vec
    }
}

impl Default for LaunchOptionArgs {
    fn default() -> Self {
        LaunchOptionArgs {
            args: &[
                "--interpreter=mi"
            ]
        }
    }
}
