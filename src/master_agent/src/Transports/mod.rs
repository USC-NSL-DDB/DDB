use std::io::Write;

use crate::launch_option::LaunchOption;

mod stream_transport;

pub enum SyncCommandResult {
    Success((String, String)),
    Fail(i64)
}

pub trait Transport {
    // type T;

    fn new(transport_callback: impl TransportCallback, option: LaunchOption /* host_wait_loop */) -> Self;
    fn send(&mut self, cmd: &str);
    fn close(&mut self);

    fn is_closed(&self) -> bool;

    fn get_debugger_id(&self) -> usize;

    fn exec_sync_cmd(&mut self, cmd_description: &str, cmd: &str, timeout: usize) -> SyncCommandResult;

    fn can_exec_cmd(&self) -> bool;
}

pub trait TransportCallback {
    fn on_stdout_line(&self, line: &str);
    fn on_stderr_line(&self, line: &str);
    fn on_debugger_exit(&self, exit_code: &str);
    fn append_to_initialization_log(&self, line: &str);
    fn log_text(&self, line: &str);
}
