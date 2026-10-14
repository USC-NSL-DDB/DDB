//! The per-session actor loop owning the transport.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::{anyhow, Result};
use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

use crate::{
    cmd_flow::event::DebuggerEventReducer,
    cmd_flow::event_publisher::EventPublisher,
    connection::{RunningTransport, TransportEvent},
    debugger::protocol::{DebuggerProtocol, ProtocolCommand, StreamKind},
    session::lifecycle::SessionTerminationCause,
};

use super::{
    demux::OutputDemux,
    handle::{ControlRequest, RuntimeRequest},
    pending::{PendingCommand, PendingCommands},
    projection::run_projector,
    RuntimeConfig, EVENT_MAILBOX_CAPACITY,
};

pub(super) struct RuntimeShared {
    pub in_flight: Arc<AtomicUsize>,
    pub closed: Arc<AtomicBool>,
    pub termination: crate::session::lifecycle::SessionTerminationReporter,
}

pub(super) struct RuntimeWire {
    pub transport: RunningTransport,
    pub protocol: Box<dyn DebuggerProtocol>,
    pub ready: watch::Sender<bool>,
}

enum WriteOwner {
    Command(u64),
    Raw(oneshot::Sender<Result<()>>),
}

type WriteCompletion = Pin<Box<dyn Future<Output = (WriteOwner, Result<()>)> + Send>>;

fn await_write(
    owner: WriteOwner,
    acknowledgement: oneshot::Receiver<Result<()>>,
) -> WriteCompletion {
    Box::pin(async move {
        let result = acknowledgement
            .await
            .map_err(|_| anyhow!("transport stopped before confirming write"))
            .and_then(|result| result);
        (owner, result)
    })
}

fn finish_write(owner: WriteOwner, result: Result<()>, pending: &mut PendingCommands) {
    match owner {
        WriteOwner::Command(token) => {
            if let Err(error) = result {
                pending.fail(token, error);
            }
        }
        WriteOwner::Raw(written) => {
            let _ = written.send(result);
        }
    }
}

async fn process_transport_event(
    sid: u64,
    event: Option<TransportEvent>,
    demux: &mut OutputDemux,
    pending: &mut PendingCommands,
    protocol: &mut dyn DebuggerProtocol,
) -> std::result::Result<(), SessionTerminationCause> {
    match event {
        Some(TransportEvent::Stdout(bytes)) => {
            if let Err(error) = demux.process(bytes, pending, protocol).await {
                warn!(sid, ?error, "failed to process debugger output");
                pending.fail_all(&error.to_string());
                return Err(SessionTerminationCause::ProtocolFault {
                    message: error.to_string(),
                });
            }
        }
        Some(TransportEvent::Stderr(bytes)) => {
            let message = String::from_utf8_lossy(&bytes).into_owned();
            debug!(sid, stderr = %message, "debugger stderr");
            if let Err(error) = demux.forward_stream(StreamKind::Log, message).await {
                warn!(sid, ?error, "failed to publish debugger stderr");
            }
        }
        Some(TransportEvent::Exited(status)) => {
            pending.fail_all(&format!("transport exited with status {:?}", status));
            return Err(SessionTerminationCause::TransportExited { status });
        }
        Some(TransportEvent::Fault(error)) => {
            pending.fail_all(&error);
            return Err(SessionTerminationCause::TransportFault { message: error });
        }
        None => {
            pending.fail_all("transport event stream closed");
            return Err(SessionTerminationCause::EventStreamClosed);
        }
    }
    Ok(())
}

enum Guarded<T> {
    Completed(T),
    ShutdownRequested(Option<oneshot::Sender<()>>),
}

/// Races `work` against the control lane so a shutdown request always
/// interrupts a blocked await instead of waiting behind it.
async fn guard_against_shutdown<T>(
    control: &mut mpsc::UnboundedReceiver<ControlRequest>,
    work: impl Future<Output = T>,
) -> Guarded<T> {
    tokio::select! {
        biased;
        control_request = control.recv() => {
            let acknowledgement =
                control_request.map(|ControlRequest::Shutdown { stopped }| stopped);
            Guarded::ShutdownRequested(acknowledgement)
        }
        result = work => Guarded::Completed(result),
    }
}

macro_rules! guarded {
    ($control:expr, $work:expr, $shutdown_ack:ident, $label:lifetime) => {
        match guard_against_shutdown($control, $work).await {
            Guarded::Completed(result) => result,
            Guarded::ShutdownRequested(acknowledgement) => {
                $shutdown_ack = acknowledgement;
                break $label;
            }
        }
    };
}

pub(super) async fn run_session(
    sid: u64,
    mut requests: mpsc::Receiver<RuntimeRequest>,
    mut control: mpsc::UnboundedReceiver<ControlRequest>,
    wire: RuntimeWire,
    shared: RuntimeShared,
    reducer: Arc<DebuggerEventReducer>,
    config: RuntimeConfig,
) {
    let RuntimeWire {
        transport,
        mut protocol,
        ready,
    } = wire;
    let RuntimeShared {
        in_flight,
        closed,
        termination,
    } = shared;
    let (writer, events) = transport.into_parts();
    let (event_tx, event_rx) = mpsc::channel(EVENT_MAILBOX_CAPACITY);
    let (applied_tx, applied) = watch::channel(0_u64);
    #[cfg(not(test))]
    let (publisher, publisher_task) = EventPublisher::spawn();
    #[cfg(test)]
    let (publisher, publisher_task) = EventPublisher::spawn_with_delay(config.publisher_delay);
    let projector = tokio::spawn(run_projector(
        sid,
        event_rx,
        reducer,
        applied_tx,
        publisher,
        termination.clone(),
        #[cfg(test)]
        config.projector_delay,
    ));
    let mut pending = PendingCommands::new(in_flight);
    let mut demux = OutputDemux::new(sid, event_tx, applied, ready);
    let mut sweeper = tokio::time::interval(config.sweep_interval);
    let mut shutdown_ack = None;
    let mut write_completions = FuturesUnordered::<WriteCompletion>::new();

    'runtime: loop {
        tokio::select! {
            control_request = control.recv() => {
                match control_request {
                    Some(ControlRequest::Shutdown { stopped }) => {
                        shutdown_ack = Some(stopped);
                        break;
                    }
                    None => break,
                }
            }
            request = requests.recv() => {
                match request {
                    Some(RuntimeRequest::Execute { token, command, permit, completion, registration }) => {
                        if pending.contains(token) {
                            let _ = completion.send(Err(anyhow!(
                                "session {} already has command token {} in flight",
                                sid,
                                token
                            )));
                            continue;
                        }
                        registration.mark_running();
                        pending.insert(token, PendingCommand {
                            completion,
                            permit,
                            consistency: command.consistency,
                            created_at: Instant::now(),
                            _registration: registration,
                        });
                        let wire = match protocol.encode_command(ProtocolCommand {
                            token,
                            command: &command.command,
                            thread_id: command.thread_id,
                        }) {
                            Ok(wire) => wire,
                            Err(error) => {
                                pending.fail(token, error);
                                continue;
                            }
                        };
                        if let Some(guard) = &command.metadata.dispatch_guard {
                            // The select loop may choose a request while native
                            // events are already queued. Fence those events, not
                            // just the records previously decoded by the actor.
                            for _ in 0..events.len() {
                                let event = match events.try_recv() {
                                    Ok(event) => Some(event),
                                    Err(flume::TryRecvError::Empty) => break,
                                    Err(flume::TryRecvError::Disconnected) => None,
                                };
                                let result = guarded!(
                                    &mut control,
                                    process_transport_event(sid, event, &mut demux, &mut pending, protocol.as_mut()),
                                    shutdown_ack,
                                    'runtime
                                );
                                if let Err(cause) = result {
                                    termination.terminate(cause);
                                    break 'runtime;
                                }
                            }
                            // Spare queue capacity does not imply an idle writer.
                            // Do not validate a stop while an earlier write can
                            // still delay this command after its guard succeeds.
                            while let Some(Some((owner, result))) = write_completions.next().now_or_never() {
                                finish_write(owner, result, &mut pending);
                            }
                            if !write_completions.is_empty() {
                                pending.fail(token, anyhow!("guarded transport write is still pending"));
                                continue;
                            }
                            let result = guarded!(
                                &mut control,
                                async {
                                    demux.fence().await?;
                                    guard.prepare().await
                                },
                                shutdown_ack,
                                'runtime
                            );
                            let mut permit = match result {
                                Ok(permit) => permit,
                                Err(error) => {
                                    pending.fail(token, error);
                                    continue;
                                }
                            };
                            // Validation may await model locks. The actor cannot
                            // decode new native events during that await, so any
                            // newly queued event invalidates this dispatch.
                            // Native state stays locked until admission or veto.
                            let mut admitted = None;
                            let mut interrupted = false;
                            let result = permit.dispatch(&mut || {
                                match control.try_recv() {
                                    Ok(ControlRequest::Shutdown { stopped }) => {
                                        shutdown_ack = Some(stopped);
                                        interrupted = true;
                                        return Err(crate::cmd_flow::input::StaleExecution.into());
                                    }
                                    Err(mpsc::error::TryRecvError::Disconnected) => {
                                        interrupted = true;
                                        return Err(crate::cmd_flow::input::StaleExecution.into());
                                    }
                                    Err(mpsc::error::TryRecvError::Empty) => {}
                                }
                                if !events.is_empty() || events.is_disconnected() {
                                    return Err(crate::cmd_flow::input::StaleExecution.into());
                                }
                                Ok(())
                            }, &mut || {
                                admitted = Some(writer.try_start_write(wire.clone())?);
                                Ok(())
                            });
                            if interrupted {
                                break 'runtime;
                            }
                            match result {
                                Ok(()) => {
                                    guard.dispatched();
                                    write_completions.push(await_write(
                                        WriteOwner::Command(token),
                                        admitted.expect("dispatch admitted a write"),
                                    ));
                                }
                                Err(error) => pending.fail(token, error),
                            }
                            continue;
                        }
                        let acknowledgement = {
                            guarded!(
                                &mut control,
                                writer.start_write(wire),
                                shutdown_ack,
                                'runtime
                            )
                        };
                        match acknowledgement {
                            Ok(acknowledgement) => {
                                write_completions.push(await_write(
                                    WriteOwner::Command(token),
                                    acknowledgement,
                                ));
                            }
                            Err(error) => pending.fail(token, error),
                        }
                    }
                    Some(RuntimeRequest::WriteRaw { data, written }) => {
                        let acknowledgement = guarded!(
                            &mut control,
                            writer.start_write(data),
                            shutdown_ack,
                            'runtime
                        );
                        match acknowledgement {
                            Ok(acknowledgement) => write_completions
                                .push(await_write(WriteOwner::Raw(written), acknowledgement)),
                            Err(error) => {
                                let _ = written.send(Err(error));
                            }
                        }
                    }
                    None => break,
                }
            }
            Some((owner, result)) = write_completions.next(), if !write_completions.is_empty() => {
                finish_write(owner, result, &mut pending);
            }
            event = events.recv_async() => {
                let result = guarded!(
                    &mut control,
                    process_transport_event(sid, event.ok(), &mut demux, &mut pending, protocol.as_mut()),
                    shutdown_ack,
                    'runtime
                );
                if let Err(cause) = result {
                    termination.terminate(cause);
                    break;
                }
            }
            _ = sweeper.tick() => {
                pending.sweep_expired(config.command_timeout);
            }
        }
    }

    closed.store(true, Ordering::Release);
    pending.fail_all("session runtime stopped");
    drop(demux);
    if shutdown_ack.is_some() {
        projector.abort();
    }
    let _ = projector.await;
    let _ = publisher_task.await;
    if let Some(stopped) = shutdown_ack {
        let _ = stopped.send(());
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn queued_shutdown_wins_over_ready_guard_work() {
        let (sender, mut control) = mpsc::unbounded_channel();
        let (stopped, _) = oneshot::channel();
        sender.send(ControlRequest::Shutdown { stopped }).unwrap();
        assert!(matches!(
            guard_against_shutdown(&mut control, std::future::ready(())).await,
            Guarded::ShutdownRequested(Some(_))
        ));
    }
}
