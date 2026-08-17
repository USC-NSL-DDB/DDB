//! Ordered application of debugger events to the runtime model.

use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tracing::warn;

use crate::{
    cmd_flow::event::{DebuggerEvent, DebuggerEventReducer},
    cmd_flow::event_publisher::EventPublisher,
    debugger::protocol::StreamKind,
    session::lifecycle::SessionTerminationReporter,
};

pub(super) struct ProjectedEvent {
    pub sequence: u64,
    pub record: ProjectionRecord,
}

pub(super) enum ProjectionRecord {
    Event(Box<DebuggerEvent>),
    Stream { kind: StreamKind, message: String },
}

/// Applies events in arrival order and advances the `applied` watermark that
/// state-consistent command completions wait on.
pub(super) async fn run_projector(
    sid: u64,
    mut events: mpsc::Receiver<ProjectedEvent>,
    reducer: Arc<DebuggerEventReducer>,
    applied: watch::Sender<u64>,
    publisher: EventPublisher,
    termination: SessionTerminationReporter,
    #[cfg(test)] projection_delay: std::time::Duration,
) {
    while let Some(event) = events.recv().await {
        #[cfg(test)]
        if !projection_delay.is_zero() {
            tokio::time::sleep(projection_delay).await;
        }
        let projection = match event.record {
            ProjectionRecord::Event(debugger_event) => reducer.project(*debugger_event, sid).await,
            ProjectionRecord::Stream { kind, message } => {
                Ok(reducer.project_stream(sid, kind, message).await)
            }
        };
        match projection {
            Ok(projection) => {
                if let Some(output) = projection.output {
                    if let Err(error) = publisher.publish(output).await {
                        warn!(sid, ?error, "failed to publish debugger event");
                    }
                }
                if let Some(cause) = projection.lifecycle {
                    termination.terminate(cause);
                }
            }
            Err(error) => warn!(sid, ?error, "failed to project debugger event"),
        }
        let _ = applied.send(event.sequence);
    }
}
