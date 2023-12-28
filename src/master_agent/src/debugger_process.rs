
struct DebuggerProcess {
    option: LaunchOption
}

fn start_process(option: LaunchOption) -> Child {
    let child = Command::new(option.mi_debugger_path)
        // .arg("../bin/hello_world")
        // .arg("--interpreter=mi")
        .args(option.get_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // .stdin(cfg)
        // .stdin(Stdio::piped())
        .spawn()
        // .output()
        .expect("Failed to start process");

    child
}
