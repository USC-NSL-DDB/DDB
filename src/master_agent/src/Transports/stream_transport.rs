
use super::{
    TransportCallback,
    Transport,
    SyncCommandResult
};

use crate::launch_option::LaunchOption;

struct StreamTransport {
    callback: Box<dyn TransportCallback>,

}

impl Transport for StreamTransport {
    fn new(transport_callback: impl TransportCallback, option: LaunchOption /* host_wait_loop */) -> Self {
        todo!()
    }

    fn send(&mut self, cmd: &str) {
        todo!()
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