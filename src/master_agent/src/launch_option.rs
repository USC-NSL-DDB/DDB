
struct LaunchOption {
    mi_debugger_path: &'static str,
    args: LaunchOptionArgs
}

impl LaunchOption {
    fn new(
        mi_debugger_path: &'static str, 
        args: &'static [&'static str]
    ) -> LaunchOption {
        LaunchOption {
            mi_debugger_path,
            args: LaunchOptionArgs::new(args)
        }
    }

    fn get_args(&self) -> Vec<&str> {
        self.args.get_args()
    }
}

struct LaunchOptionArgs {
    args: &'static [&'static str]
}

impl LaunchOptionArgs {
    fn new(args: &'static [&'static str]) -> LaunchOptionArgs {
        LaunchOptionArgs { args }
    }

    fn get_args(&self) -> Vec<&str> {
        let mut arg_vec = self.args.to_vec();
        arg_vec.push("--interpreter=mi");
        arg_vec
    }
}
