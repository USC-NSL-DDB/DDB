//! Demultiplexes raw debugger stdout into events and command results.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tracing::{trace, warn};

use crate::{
    cmd_flow::event::decode_event,
    cmd_flow::response::ParsedSessionResponse,
    debugger::protocol::{DebuggerProtocol, ProtocolRecord},
};

use super::{
    pending::{complete_after_events, PendingCommands},
    projection::ProjectedEvent,
};

/// Buffers stdout until complete records arrive, then routes notifications to
/// the projector and results to their pending commands.
pub(super) struct OutputDemux {
    sid: u64,
    event_sequence: u64,
    event_tx: mpsc::Sender<ProjectedEvent>,
    applied: watch::Receiver<u64>,
    ready: watch::Sender<bool>,
}

impl OutputDemux {
    pub(super) fn new(
        sid: u64,
        event_tx: mpsc::Sender<ProjectedEvent>,
        applied: watch::Receiver<u64>,
        ready: watch::Sender<bool>,
    ) -> Self {
        Self {
            sid,
            event_sequence: 0,
            event_tx,
            applied,
            ready,
        }
    }

    pub(super) async fn process(
        &mut self,
        bytes: Bytes,
        pending: &mut PendingCommands,
        protocol: &mut dyn DebuggerProtocol,
    ) -> Result<()> {
        let sid = self.sid;
        for record in protocol.push_stdout(bytes)? {
            match record {
                ProtocolRecord::Ready => {
                    self.ready.send_replace(true);
                }
                ProtocolRecord::Event {
                    token,
                    message,
                    payload,
                } => {
                    let event = match decode_event(token, message, payload) {
                        Ok(event) => event,
                        Err(error) => {
                            warn!(sid, ?error, "discarding malformed debugger event");
                            continue;
                        }
                    };
                    self.event_sequence += 1;
                    self.event_tx
                        .send(ProjectedEvent {
                            sequence: self.event_sequence,
                            event,
                        })
                        .await
                        .map_err(|_| anyhow!("session {} event projector is closed", sid))?;
                }
                ProtocolRecord::Result {
                    token: Some(token),
                    message,
                    payload,
                } => {
                    if let Some(command) = pending.take(token) {
                        complete_after_events(
                            sid,
                            token,
                            ParsedSessionResponse::new(sid, message, payload),
                            command,
                            self.event_sequence,
                            self.applied.clone(),
                            pending.in_flight(),
                        );
                    } else {
                        trace!(sid, token, "received result for unknown command");
                    }
                }
                ProtocolRecord::Result { token: None, .. } => {
                    trace!(sid, "received tokenless result");
                }
                ProtocolRecord::Stream { kind, message } => {
                    trace!(sid, ?kind, ?message, "received debugger stream output");
                }
            }
        }
        Ok(())
    }
}
