
use super::{
    TransportCallback,
    Transport,
    SyncCommandResult
};

use crate::launch_option::LaunchOption;

// struct StreamTransport {
//     callback: Box<dyn TransportCallback>,

// }

struct MyCallback;
impl TransportCallback for MyCallback {
    fn on_stdout_line(&self, line: &str) {
        println!("called on_stdout_line");
    }

    fn on_stderr_line(&self, line: &str) {
        todo!()
    }

    fn on_debugger_exit(&self, exit_code: &str) {
        todo!()
    }

    fn append_to_initialization_log(&self, line: &str) {
        todo!()
    }

    fn log_text(&self, line: &str) {
        todo!()
    }
}

// For the callback should we use trait object or generic?
struct StreamTransport<CB: TransportCallback> {
    callback: CB
}

impl<CB: TransportCallback> StreamTransport<CB>
{
    fn test(&mut self) {
        self.send("cmd");
        self.callback.on_stdout_line("line");
    }   
}

impl<CB: TransportCallback> Transport for StreamTransport<CB>
{
    type T = CB;

    fn new(transport_callback: CB, option: LaunchOption /* host_wait_loop */) -> Self {
        Self { callback: transport_callback }
    }

    fn send(&mut self, cmd: &str) {
        println!("called send.");
    }

    fn close(&mut self) {
        todo!()
    }

    fn is_closed(&self) -> bool {
        todo!()
    }

    fn get_debugger_id(&self) -> usize {
        todo!()
    }

    fn exec_sync_cmd(&mut self, cmd_description: &str, cmd: &str, timeout: usize) -> SyncCommandResult {
        todo!()
    }

    fn can_exec_cmd(&self) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dummy() {
        let callback = MyCallback;

        let mut stream = StreamTransport::new(callback, LaunchOption::default());
        stream.test();
    }
}
