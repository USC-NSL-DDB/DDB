use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tracing::debug;

use crate::debugger::gdb::parser::MIFormatter;

use super::event::ProjectedDebuggerOutput;

const EVENT_OUTPUT_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct EventPublisher {
    output: mpsc::Sender<ProjectedDebuggerOutput>,
}

impl EventPublisher {
    pub(crate) fn spawn() -> (Self, tokio::task::JoinHandle<()>) {
        let (output, receiver) = mpsc::channel(EVENT_OUTPUT_CAPACITY);
        let task = tokio::spawn(run_publisher(
            receiver,
            #[cfg(test)]
            std::time::Duration::ZERO,
        ));
        (Self { output }, task)
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_delay(
        delay: std::time::Duration,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (output, receiver) = mpsc::channel(EVENT_OUTPUT_CAPACITY);
        let task = tokio::spawn(run_publisher(receiver, delay));
        (Self { output }, task)
    }

    pub(crate) async fn publish(&self, output: ProjectedDebuggerOutput) -> Result<()> {
        self.output
            .send(output)
            .await
            .map_err(|_| anyhow!("debugger event publisher is closed"))
    }
}

async fn run_publisher(
    mut receiver: mpsc::Receiver<ProjectedDebuggerOutput>,
    #[cfg(test)] delay: std::time::Duration,
) {
    while let Some(output) = receiver.recv().await {
        #[cfg(test)]
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let formatted = output
            .records
            .iter()
            .map(|record| {
                MIFormatter::format(
                    record.prefix,
                    &record.message,
                    record.payload.as_ref(),
                    record.token,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !formatted.is_empty() {
            println!("{}", formatted);
            debug!("output: {}", formatted);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::cmd_flow::event::ProjectedDebuggerRecord;

    #[test]
    fn projected_output_is_rendered_without_state_lookups() {
        let payload = HashMap::from([("thread-id".to_string(), "9".into())]).into();
        let output = ProjectedDebuggerOutput {
            records: vec![ProjectedDebuggerRecord {
                prefix: "*",
                message: "running".into(),
                payload: Some(payload),
                token: None,
            }],
        };
        let record = &output.records[0];
        assert_eq!(
            MIFormatter::format(
                record.prefix,
                &record.message,
                record.payload.as_ref(),
                record.token,
            ),
            "*running,thread-id=\"9\""
        );
    }
}
