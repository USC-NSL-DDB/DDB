//! Per-session command runtime.
//!
//! Each debugger session runs one actor that owns the transport: commands are
//! encoded onto the wire ([`command`]), stdout is demultiplexed back into
//! results and events ([`demux`]), events are applied to the runtime model by
//! a projector task ([`projection`]), and in-flight commands are tracked with
//! their completion barriers ([`pending`]). [`handle`] is the client surface.

mod actor;
mod command;
mod demux;
mod handle;
mod pending;
mod projection;
#[cfg(test)]
mod tests;

use std::time::Duration;

pub use command::{CompletionConsistency, SessionCommand};
pub(crate) use handle::{PendingCommandChange, SessionPendingCommand};
pub use handle::{SessionHandle, SessionLease, SessionTicket};

const COMMAND_MAILBOX_CAPACITY: usize = 256;
const MAX_PENDING_COMMANDS: usize = 1_024;
const EVENT_MAILBOX_CAPACITY: usize = 256;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct RuntimeConfig {
    command_timeout: Duration,
    sweep_interval: Duration,
    #[cfg(test)]
    projector_delay: Duration,
    #[cfg(test)]
    publisher_delay: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            command_timeout: COMMAND_TIMEOUT,
            sweep_interval: COMMAND_SWEEP_INTERVAL,
            #[cfg(test)]
            projector_delay: Duration::ZERO,
            #[cfg(test)]
            publisher_delay: Duration::ZERO,
        }
    }
}
