//! Demultiplexes raw debugger stdout into events and command results.

use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use gdbmi::parser::{Message, Response};
use tokio::sync::{mpsc, watch};
use tracing::{trace, warn};

use crate::{
    cmd_flow::event::decode_event, cmd_flow::response::ParsedSessionResponse,
    debugger::gdb::parser::GdbParser,
};

use super::{
    pending::{complete_after_events, PendingCommands},
    projection::ProjectedEvent,
};

/// Buffers stdout until complete records arrive, then routes notifications to
/// the projector and results to their pending commands.
pub(super) struct OutputDemux {
    sid: u64,
    buffer: BytesMut,
    event_sequence: u64,
    event_tx: mpsc::Sender<ProjectedEvent>,
    applied: watch::Receiver<u64>,
}

impl OutputDemux {
    pub(super) fn new(
        sid: u64,
        event_tx: mpsc::Sender<ProjectedEvent>,
        applied: watch::Receiver<u64>,
    ) -> Self {
        Self {
            sid,
            buffer: BytesMut::new(),
            event_sequence: 0,
            event_tx,
            applied,
        }
    }

    pub(super) async fn process(
        &mut self,
        bytes: Bytes,
        pending: &mut PendingCommands,
    ) -> Result<()> {
        let sid = self.sid;
        self.buffer.extend_from_slice(&bytes);
        let Some(last_newline) = self.buffer.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(());
        };
        let complete = self.buffer.split_to(last_newline + 1);
        let text = std::str::from_utf8(&complete)?;
        for message in GdbParser::parse_multiple(text) {
            match message {
                Message::Response(Response::Notify {
                    token,
                    message,
                    payload,
                }) => {
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
                Message::Response(Response::Result {
                    token: Some(token),
                    message,
                    payload,
                }) => {
                    let token = token.0 as u64;
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
                Message::Response(Response::Result { token: None, .. }) => {
                    trace!(sid, "received tokenless result");
                }
                Message::General(message) => {
                    trace!(sid, ?message, "received general debugger output");
                }
            }
        }
        Ok(())
    }
}
