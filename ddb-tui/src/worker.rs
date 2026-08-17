use std::{collections::VecDeque, time::Duration};

use anyhow::{bail, Context};
use ddb_api_client::v2::{
    self, breakpoint_spec, extension_payload, operation_result, output_event, resource_upsert,
    state_event,
};
use tokio::{sync::mpsc, task::JoinSet};

use crate::{
    api::{
        thread_target, ApiClient, CapabilitiesExt, OutputSyncItem, OutputSyncOptions,
        ProjectedStateSyncItem, StateSyncOptions, V2ApiClient,
    },
    model::{BackendRequest, DebuggerActivity, DebuggerEvent, EventStreamStatus, UiMessage},
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const OPERATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_COLLECTION_ITEMS: usize = 10_000;

pub async fn run(
    client: ApiClient,
    mut requests: mpsc::Receiver<BackendRequest>,
    messages: mpsc::Sender<UiMessage>,
) {
    let mut inspections = JoinSet::new();
    let mut deferred = VecDeque::new();
    let mut inspection_generation = None;
    loop {
        while let Some(result) = inspections.try_join_next() {
            report_inspection_failure(result, &messages).await;
        }
        let request = match deferred.pop_front() {
            Some(request) => request,
            None => match requests.recv().await {
                Some(request) => request,
                None => break,
            },
        };
        if is_recovery(&request) {
            while let Ok(queued) = requests.try_recv() {
                if !is_recovery(&queued) {
                    deferred.push_back(queued);
                }
            }
        }

        if is_inspection(&request) {
            let (generation, thread_id) = inspection_context(&request)
                .expect("inspection requests always carry generation context");
            if inspection_generation.is_some_and(|active| active != generation) {
                inspections.abort_all();
            }
            inspection_generation = Some(generation);
            while inspections.len() >= 4 {
                if let Some(result) = inspections.join_next().await {
                    report_inspection_failure(result, &messages).await;
                }
            }
            let client = client.clone();
            let messages = messages.clone();
            inspections.spawn(async move {
                if let Err(error) = handle(&client, &messages, request).await {
                    let _ = messages
                        .send(UiMessage::InspectionError {
                            generation,
                            thread_id,
                            error: format!("{error:#}"),
                        })
                        .await;
                }
            });
        } else {
            let connection_probe = is_recovery(&request);
            if let Err(error) = handle(&client, &messages, request).await {
                let message = format!("{error:#}");
                let update = if connection_probe {
                    UiMessage::BackendUnavailable(message)
                } else {
                    UiMessage::Error(message)
                };
                let _ = messages.send(update).await;
            }
        }
    }
    inspections.shutdown().await;
}

async fn report_inspection_failure(
    result: Result<(), tokio::task::JoinError>,
    messages: &mpsc::Sender<UiMessage>,
) {
    if let Err(error) = result {
        if error.is_cancelled() {
            return;
        }
        let _ = messages
            .send(UiMessage::Error(format!(
                "inspection task failed unexpectedly: {error}"
            )))
            .await;
    }
}

fn is_recovery(request: &BackendRequest) -> bool {
    matches!(request, BackendRequest::Bootstrap | BackendRequest::Refresh)
}

fn is_inspection(request: &BackendRequest) -> bool {
    matches!(
        request,
        BackendRequest::InspectThread { .. }
            | BackendRequest::InspectFrame { .. }
            | BackendRequest::LoadSource { .. }
            | BackendRequest::ExpandVariable { .. }
            | BackendRequest::ReadMemory { .. }
    )
}

fn inspection_context(request: &BackendRequest) -> Option<(u64, String)> {
    match request {
        BackendRequest::InspectThread {
            thread_id,
            generation,
        }
        | BackendRequest::InspectFrame {
            thread_id,
            generation,
            ..
        }
        | BackendRequest::ReadMemory {
            thread_id,
            generation,
            ..
        }
        | BackendRequest::LoadSource {
            thread_id,
            generation,
            ..
        } => Some((*generation, thread_id.clone())),
        _ => None,
    }
}

async fn handle(
    client: &ApiClient,
    messages: &mpsc::Sender<UiMessage>,
    request: BackendRequest,
) -> anyhow::Result<()> {
    let ApiClient::V2(client) = client else {
        let ApiClient::V1Fallback(client) = client else {
            unreachable!()
        };
        return crate::legacy_v1::handle(client, messages, request).await;
    };
    handle_v2(client, messages, request).await
}

async fn handle_v2(
    client: &V2ApiClient,
    messages: &mpsc::Sender<UiMessage>,
    request: BackendRequest,
) -> anyhow::Result<()> {
    match request {
        BackendRequest::Bootstrap => {
            let (_, capabilities) = client.handshake().await?;
            capabilities.validate_for_tui()?;
            messages.send(UiMessage::Capabilities(capabilities)).await?;
            refresh(client, messages).await?;
        }
        BackendRequest::Refresh => refresh(client, messages).await?,
        BackendRequest::InspectThread {
            thread_id,
            generation,
        } => inspect_thread(client, messages, thread_id, generation).await?,
        BackendRequest::InspectFrame {
            thread_id,
            owner_thread_id,
            generation,
            frame_id,
            source,
            line,
        } => {
            let variables = list_frame_variables(client, &frame_id).await?;
            messages
                .send(UiMessage::Variables {
                    generation,
                    thread_id: thread_id.clone(),
                    variables,
                })
                .await?;
            let registers = list_frame_registers(client, &frame_id).await?;
            messages
                .send(UiMessage::Registers {
                    generation,
                    thread_id: thread_id.clone(),
                    registers,
                })
                .await?;
            if let (Some(source), Some(line)) = (source, line) {
                let content =
                    source_content(client, &owner_thread_id, &source, line as usize).await?;
                messages
                    .send(UiMessage::Source {
                        generation,
                        thread_id,
                        source: content,
                        line: line as usize,
                    })
                    .await?;
            }
        }
        BackendRequest::LoadSource {
            thread_id,
            generation,
            source,
            line,
        } => {
            let content = source_content(client, &thread_id, &source, line).await?;
            messages
                .send(UiMessage::Source {
                    generation,
                    thread_id,
                    source: content,
                    line,
                })
                .await?;
        }
        BackendRequest::ListSignals { target } => {
            let signals = client
                .collect_signals(
                    v2::ListSignalsRequest {
                        context: None,
                        target: Some(target.clone()),
                        page: None,
                    },
                    4_096,
                )
                .await?;
            messages
                .send(UiMessage::Signals { target, signals })
                .await?;
        }

        BackendRequest::Control(control, target) => {
            let action = execution_action(control)
                .context("only execution controls reach the backend worker")?;
            let label = control
                .action_name()
                .expect("execution action has a stable UI name")
                .to_string();
            let receipt = complete_operation(
                client,
                client
                    .execute(v2::ExecuteRequest {
                        context: None,
                        target: Some(target),
                        action: action as i32,
                        jump_location: None,
                        signal_name: None,
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages.send(UiMessage::Receipt(label, receipt)).await?;
        }
        BackendRequest::Jump { location, target } => {
            let jump_location = parse_jump_location(&location)?;
            let receipt = complete_operation(
                client,
                client
                    .execute(v2::ExecuteRequest {
                        context: None,
                        target: Some(target),
                        action: v2::ExecutionAction::Jump as i32,
                        jump_location: Some(jump_location),
                        signal_name: None,
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(format!("jump to {location}"), receipt))
                .await?;
        }
        BackendRequest::SendSignal { signal, target } => {
            let receipt = complete_operation(
                client,
                client
                    .execute(v2::ExecuteRequest {
                        context: None,
                        target: Some(target),
                        action: v2::ExecutionAction::Signal as i32,
                        jump_location: None,
                        signal_name: Some(signal.clone()),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(format!("signal {signal}"), receipt))
                .await?;
        }
        BackendRequest::CreateBreakpoint {
            source,
            line,
            target,
            options,
        } => {
            let line = u32::try_from(line).context("breakpoint line exceeds the API limit")?;
            let receipt = complete_operation(
                client,
                client
                    .create_breakpoint(v2::CreateBreakpointRequest {
                        context: None,
                        target: Some(target.into_api_target()),
                        breakpoint: Some(v2::BreakpointSpec {
                            location: Some(breakpoint_spec::Location::Source(
                                v2::SourceBreakpointLocation {
                                    source: source.clone(),
                                    line,
                                    column: 0,
                                },
                            )),
                            enabled: Some(true),
                            condition: options.condition,
                            ignore_count: None,
                            temporary: options.temporary,
                            hardware: options.hardware,
                        }),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(
                    format!("breakpoint {source}:{line}"),
                    receipt,
                ))
                .await?;
        }
        BackendRequest::DeleteBreakpoint { id, target } => {
            let receipt = complete_operation(
                client,
                client
                    .delete_breakpoint(v2::DeleteBreakpointRequest {
                        context: None,
                        breakpoint_id: id.clone(),
                        target: Some(target),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(
                    format!("delete breakpoint {id}"),
                    receipt,
                ))
                .await?;
        }
        BackendRequest::SetBreakpointEnabled {
            id,
            target,
            enabled,
        } => {
            let receipt = complete_operation(
                client,
                client
                    .update_breakpoint(v2::UpdateBreakpointRequest {
                        context: None,
                        breakpoint_id: id.clone(),
                        target: Some(target),
                        breakpoint: Some(v2::BreakpointSpec {
                            enabled: Some(enabled),
                            ..Default::default()
                        }),
                        update_mask: Some(ddb_api_client::wkt::FieldMask {
                            paths: vec!["enabled".to_string()],
                        }),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            let action = if enabled { "enable" } else { "disable" };
            messages
                .send(UiMessage::Receipt(
                    format!("{action} breakpoint {id}"),
                    receipt,
                ))
                .await?;
        }
        BackendRequest::Evaluate {
            expression,
            thread_id,
            frame_id,
        } => {
            let receipt = complete_operation(
                client,
                client
                    .evaluate(v2::EvaluateRequest {
                        context: None,
                        target: Some(thread_target(thread_id)),
                        expression: expression.clone(),
                        frame_id,
                        evaluation_context: v2::EvaluationContext::Repl as i32,
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(
                    format!("evaluate {expression}"),
                    receipt,
                ))
                .await?;
        }
        BackendRequest::ExpandVariable {
            variable_id,
            thread_id,
            generation,
        } => {
            let variables = client
                .collect_variable_children(
                    v2::ExpandVariableRequest {
                        context: None,
                        variable_id: variable_id.clone(),
                        page: None,
                    },
                    MAX_COLLECTION_ITEMS,
                )
                .await?;
            messages
                .send(UiMessage::VariableChildren {
                    generation,
                    thread_id,
                    parent_id: variable_id,
                    variables,
                })
                .await?;
        }
        BackendRequest::ReadMemory {
            address,
            count,
            thread_id,
            generation,
        } => {
            let memory = client
                .read_memory(v2::ReadMemoryRequest {
                    context: None,
                    target: Some(thread_target(thread_id.clone())),
                    address: address.clone(),
                    byte_count: count,
                })
                .await?
                .memory
                .context("ReadMemory omitted memory")?;
            messages
                .send(UiMessage::Memory {
                    generation,
                    thread_id,
                    memory,
                })
                .await?;
            messages
                .send(UiMessage::Notice(format!(
                    "memory {address} ({count} bytes)"
                )))
                .await?;
        }
        BackendRequest::InvokeExtensionAction {
            extension_id,
            extension_version,
            action_id,
            request_schema_uri,
            payload_json,
            target,
        } => {
            let label = format!("{extension_id}/{action_id}");
            let receipt = complete_operation(
                client,
                client
                    .invoke_extension_action(v2::InvokeExtensionActionRequest {
                        context: None,
                        extension_id: extension_id.clone(),
                        action_id,
                        payload: Some(v2::ExtensionPayload {
                            extension_id,
                            schema_version: extension_version,
                            schema_uri: request_schema_uri,
                            media_type: "application/json".to_string(),
                            payload: Some(extension_payload::Payload::PayloadJson(payload_json)),
                        }),
                        target: Some(target),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(format!("extension {label}"), receipt))
                .await?;
        }
        BackendRequest::RawCommand { command, target } => {
            let receipt = complete_operation(
                client,
                client
                    .execute_raw_command(v2::ExecuteRawCommandRequest {
                        context: None,
                        target: Some(target),
                        // DDB's public compatibility command language is its
                        // GDB/MI-shaped facade, regardless of the selected
                        // debugger backend. BackendNative is intentionally
                        // reserved for a future true backend-native path.
                        dialect: v2::RawCommandDialect::GdbMi as i32,
                        command: command.clone(),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages.send(UiMessage::Receipt(command, receipt)).await?;
        }
    }
    Ok(())
}

async fn refresh(client: &V2ApiClient, messages: &mpsc::Sender<UiMessage>) -> anyhow::Result<()> {
    let snapshot = client
        .get_snapshot(v2::GetSnapshotRequest {
            context: None,
            sections: Vec::new(),
            target: None,
        })
        .await?
        .snapshot
        .context("GetSnapshot omitted snapshot")?;
    let threads = snapshot.threads.clone();
    messages.send(UiMessage::Snapshot(snapshot)).await?;
    messages.send(UiMessage::Threads(threads)).await?;
    Ok(())
}

async fn inspect_thread(
    client: &V2ApiClient,
    messages: &mpsc::Sender<UiMessage>,
    thread_id: String,
    generation: u64,
) -> anyhow::Result<()> {
    let _ = complete_operation(
        client,
        client
            .select_thread(v2::SelectThreadRequest {
                context: None,
                target: Some(thread_target(thread_id.clone())),
                preconditions: None,
            })
            .await?,
    )
    .await?;
    let result = distributed_frames(client, &thread_id).await?;
    let first_frame = result.frames.iter().find_map(|distributed| {
        distributed
            .frame
            .clone()
            .map(|frame| (distributed.thread_id.clone(), frame))
    });
    messages
        .send(UiMessage::DistributedFrames {
            generation,
            thread_id: thread_id.clone(),
            result,
        })
        .await?;
    if let Some((frame_thread_id, frame)) = first_frame {
        let variables = list_frame_variables(client, &frame.frame_id).await?;
        messages
            .send(UiMessage::Variables {
                generation,
                thread_id: thread_id.clone(),
                variables,
            })
            .await?;
        let registers = list_frame_registers(client, &frame.frame_id).await?;
        messages
            .send(UiMessage::Registers {
                generation,
                thread_id: thread_id.clone(),
                registers,
            })
            .await?;
        if let Some(location) = frame.location.as_ref() {
            if let Some(path) = location.path.as_deref() {
                let content =
                    source_content(client, &frame_thread_id, path, location.line as usize)
                        .await
                        .with_context(|| format!("failed to load source '{path}'"))?;
                messages
                    .send(UiMessage::Source {
                        generation,
                        thread_id,
                        source: content,
                        line: location.line as usize,
                    })
                    .await?;
            }
        }
    }
    Ok(())
}

async fn distributed_frames(
    client: &V2ApiClient,
    thread_id: &str,
) -> anyhow::Result<v2::DistributedBacktraceResult> {
    let operation = complete_operation(
        client,
        client
            .run_distributed_backtrace(v2::RunDistributedBacktraceRequest {
                context: None,
                target: Some(thread_target(thread_id.to_string())),
                max_frames: 0,
                preconditions: None,
            })
            .await?,
    )
    .await?;
    match operation.result.and_then(|result| result.value) {
        Some(operation_result::Value::DistributedBacktrace(result)) => Ok(result),
        _ => bail!("distributed backtrace completed without a typed frame result"),
    }
}

async fn list_frame_registers(
    client: &V2ApiClient,
    frame_id: &str,
) -> anyhow::Result<Vec<v2::Register>> {
    Ok(client
        .collect_registers(
            v2::ListRegistersRequest {
                context: None,
                frame_id: frame_id.to_string(),
                format: v2::RegisterFormat::Natural as i32,
                page: None,
            },
            MAX_COLLECTION_ITEMS,
        )
        .await?)
}

async fn list_frame_variables(
    client: &V2ApiClient,
    frame_id: &str,
) -> anyhow::Result<Vec<v2::Variable>> {
    let scopes = client
        .collect_scopes(
            v2::ListScopesRequest {
                context: None,
                frame_id: frame_id.to_string(),
                page: None,
            },
            MAX_COLLECTION_ITEMS,
        )
        .await?;
    let mut variables = Vec::new();
    for scope in scopes {
        let remaining = MAX_COLLECTION_ITEMS.saturating_sub(variables.len());
        let page = client
            .collect_variables(
                v2::ListVariablesRequest {
                    context: None,
                    scope_id: scope.scope_id,
                    page: None,
                },
                remaining,
            )
            .await?;
        variables.extend(page);
    }
    Ok(variables)
}

async fn source_content(
    client: &V2ApiClient,
    thread_id: &str,
    path: &str,
    line: usize,
) -> anyhow::Result<v2::SourceContent> {
    let line = u32::try_from(line).context("source line exceeds the API limit")?;
    let source = client
        .resolve_source(v2::ResolveSourceRequest {
            context: None,
            target: Some(thread_target(thread_id)),
            location: Some(v2::SourceLocation {
                path: Some(path.to_string()),
                line,
                ..Default::default()
            }),
        })
        .await?
        .source
        .context("ResolveSource omitted source")?;
    client
        .read_source(v2::ReadSourceRequest {
            context: None,
            source_reference: source.source_reference,
            start_line: line.saturating_sub(100).max(1),
            max_lines: 240,
        })
        .await?
        .source
        .context("ReadSource omitted source")
}

async fn complete_operation(
    client: &V2ApiClient,
    admission: v2::OperationAdmissionResponse,
) -> anyhow::Result<v2::Operation> {
    let operation = admission
        .operation
        .context("mutation admission omitted operation")?;
    if operation_is_terminal(operation.state) {
        return Ok(operation);
    }
    client
        .wait_operation(
            operation.operation_id,
            OPERATION_TIMEOUT,
            OPERATION_POLL_INTERVAL,
        )
        .await
        .map_err(Into::into)
}

fn operation_is_terminal(state: i32) -> bool {
    matches!(
        v2::OperationState::try_from(state),
        Ok(v2::OperationState::Completed
            | v2::OperationState::Failed
            | v2::OperationState::Cancelled)
    )
}

fn execution_action(control: crate::model::Control) -> Option<v2::ExecutionAction> {
    match control {
        crate::model::Control::Continue => Some(v2::ExecutionAction::Continue),
        crate::model::Control::Interrupt => Some(v2::ExecutionAction::Interrupt),
        crate::model::Control::Next => Some(v2::ExecutionAction::Next),
        crate::model::Control::StepIn => Some(v2::ExecutionAction::StepIn),
        crate::model::Control::StepOut => Some(v2::ExecutionAction::StepOut),
        crate::model::Control::CycleScope
        | crate::model::Control::Refresh
        | crate::model::Control::RefreshStack => None,
    }
}

fn parse_jump_location(value: &str) -> anyhow::Result<v2::SourceLocation> {
    let value = value.trim();
    if value.is_empty() {
        bail!("jump location must not be empty");
    }
    if value.starts_with('*') || value.starts_with("0x") {
        return Ok(v2::SourceLocation {
            address: Some(value.trim_start_matches('*').to_string()),
            ..Default::default()
        });
    }
    let (path, line) = value
        .rsplit_once(':')
        .context("jump location must be FILE:LINE or *ADDRESS")?;
    let line = line
        .parse::<u32>()
        .context("jump line must be a positive integer")?;
    if path.trim().is_empty() || line == 0 {
        bail!("jump location must be FILE:LINE with a positive line");
    }
    Ok(v2::SourceLocation {
        path: Some(path.trim().to_string()),
        line,
        ..Default::default()
    })
}

/// Runs the SDK-owned snapshot/replay convergence loop. The UI receives only
/// detached, already-converged public snapshots.
pub async fn watch_events(client: ApiClient, messages: mpsc::Sender<UiMessage>) {
    let ApiClient::V2(client) = client else {
        let ApiClient::V1Fallback(client) = client else {
            unreachable!()
        };
        crate::legacy_v1::watch_events(client, messages).await;
        return;
    };
    let mut sync = match client.projected_state_sync(StateSyncOptions::default()) {
        Ok(sync) => sync,
        Err(error) => {
            let _ = messages.send(UiMessage::Error(error.to_string())).await;
            return;
        }
    };
    loop {
        match sync.next().await {
            Ok(ProjectedStateSyncItem::Snapshot) => {
                let Some(snapshot) = sync.current_snapshot() else {
                    let _ = messages
                        .send(UiMessage::Error(
                            "DDB SDK reported hydration without a projection".to_string(),
                        ))
                        .await;
                    return;
                };
                let threads = snapshot.threads.clone();
                if messages
                    .send(UiMessage::EventStream(EventStreamStatus::Connected))
                    .await
                    .is_err()
                    || messages.send(UiMessage::Snapshot(snapshot)).await.is_err()
                    || messages.send(UiMessage::Threads(threads)).await.is_err()
                {
                    return;
                }
            }
            Ok(ProjectedStateSyncItem::Event(event)) => {
                let summary = state_event_summary(&event);
                let activity = state_event_activity(&event);
                let Some(snapshot) = sync.current_snapshot() else {
                    let _ = messages
                        .send(UiMessage::Error(
                            "DDB SDK reported an event without a projection".to_string(),
                        ))
                        .await;
                    return;
                };
                let threads = snapshot.threads.clone();
                if messages.send(UiMessage::Snapshot(snapshot)).await.is_err()
                    || messages.send(UiMessage::Threads(threads)).await.is_err()
                    || messages
                        .send(UiMessage::DebuggerEvent(DebuggerEvent {
                            summary,
                            refresh: false,
                            activity,
                        }))
                        .await
                        .is_err()
                {
                    return;
                }
            }
            Ok(ProjectedStateSyncItem::Reconnecting { reason, .. }) => {
                if messages
                    .send(UiMessage::EventStream(EventStreamStatus::Reconnecting(
                        reason,
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(ProjectedStateSyncItem::Rehydrating { reason }) => {
                let reason = reason
                    .map(|reason| reason.message)
                    .unwrap_or_else(|| "state replay requires a fresh snapshot".to_string());
                if messages
                    .send(UiMessage::EventStream(EventStreamStatus::Reconnecting(
                        reason,
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = messages.send(UiMessage::Error(error.to_string())).await;
                return;
            }
        }
    }
}

pub async fn watch_output(client: ApiClient, messages: mpsc::Sender<UiMessage>) {
    let ApiClient::V2(client) = client else {
        std::future::pending::<()>().await;
        return;
    };
    let mut sync = match client.output_sync(OutputSyncOptions::default()) {
        Ok(sync) => sync,
        Err(error) => {
            let _ = messages.send(UiMessage::Error(error.to_string())).await;
            return;
        }
    };
    loop {
        match sync.next().await {
            Ok(OutputSyncItem::Event(event)) => {
                if messages
                    .send(UiMessage::Output(output_event_summary(event)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(OutputSyncItem::Reconnecting { reason, .. }) => {
                if messages
                    .send(UiMessage::Output(format!(
                        "output stream reconnecting: {reason}"
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(OutputSyncItem::Restarting { reason }) => {
                let reason = reason
                    .map(|reason| reason.message)
                    .unwrap_or_else(|| "output replay is unavailable".to_string());
                if messages
                    .send(UiMessage::Output(format!(
                        "output gap: {reason}; continuing from live output"
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = messages.send(UiMessage::Error(error.to_string())).await;
                return;
            }
        }
    }
}

fn state_event_summary(event: &v2::StateEvent) -> String {
    if let Some(state_event::Payload::Upsert(upsert)) = event.payload.as_ref() {
        match upsert.resource.as_ref() {
            Some(resource_upsert::Resource::Thread(thread)) => {
                let state = match v2::ThreadState::try_from(thread.state) {
                    Ok(v2::ThreadState::Running) => "running",
                    Ok(v2::ThreadState::Stopped) => "stopped",
                    Ok(v2::ThreadState::Exited) => "exited",
                    Ok(v2::ThreadState::Unavailable) => "unavailable",
                    _ => "unknown",
                };
                return format!("exec · {state} · {}", thread.thread_id);
            }
            Some(resource_upsert::Resource::ExecutionState(state)) => {
                return format!(
                    "exec · {}",
                    if state.running { "running" } else { "stopped" }
                );
            }
            Some(resource_upsert::Resource::Breakpoint(breakpoint)) => {
                return format!("breakpoint · updated · {}", breakpoint.breakpoint_id);
            }
            _ => {}
        }
    }
    let kind = v2::StateEventKind::try_from(event.kind)
        .map(|kind| kind.as_str_name())
        .unwrap_or("STATE_EVENT_KIND_UNSPECIFIED");
    let resource = v2::ResourceKind::try_from(event.resource_kind)
        .map(|kind| kind.as_str_name())
        .unwrap_or("RESOURCE_KIND_UNSPECIFIED");
    format!(
        "{} {} {}",
        short_enum(kind),
        short_enum(resource),
        event.resource_id
    )
}

fn state_event_activity(event: &v2::StateEvent) -> DebuggerActivity {
    let Some(state_event::Payload::Upsert(upsert)) = event.payload.as_ref() else {
        return DebuggerActivity::None;
    };
    match upsert.resource.as_ref() {
        Some(resource_upsert::Resource::ExecutionState(state)) if state.running => {
            DebuggerActivity::Running(target_thread_id(state.target.as_ref()))
        }
        Some(resource_upsert::Resource::ExecutionState(state)) => {
            let thread_id = state
                .stop_reason
                .as_ref()
                .and_then(|reason| reason.thread_id.clone())
                .or_else(|| target_thread_id(state.target.as_ref()));
            DebuggerActivity::Stopped(thread_id)
        }
        _ => DebuggerActivity::None,
    }
}

fn target_thread_id(target: Option<&v2::Target>) -> Option<String> {
    match target?.selector.as_ref()? {
        v2::target::Selector::Thread(target) => Some(target.thread_id.clone()),
        _ => None,
    }
}

fn output_event_summary(event: v2::OutputEvent) -> String {
    if let Some(gap) = event.gap {
        return format!("output gap: {}", gap.reason);
    }
    let stream = v2::OutputStreamKind::try_from(event.stream)
        .map(|stream| short_enum(stream.as_str_name()))
        .unwrap_or_else(|_| "output".to_string());
    let mut text = match event.content {
        Some(output_event::Content::Text(text)) => text,
        Some(output_event::Content::Data(data)) => format!("{} binary bytes", data.len()),
        None => String::new(),
    };
    if event.truncated {
        text.push_str(" …[truncated]");
    }
    format!("{stream} · {}", text.trim_end())
}

fn short_enum(name: &str) -> String {
    name.rsplit('_').next().unwrap_or(name).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_parser_accepts_source_and_address_forms() {
        let source = parse_jump_location("src/main.rs:42").unwrap();
        assert_eq!(source.path.as_deref(), Some("src/main.rs"));
        assert_eq!(source.line, 42);
        let address = parse_jump_location("*0x401000").unwrap();
        assert_eq!(address.address.as_deref(), Some("0x401000"));
        assert!(parse_jump_location("42").is_err());
    }

    #[test]
    fn state_activity_uses_opaque_thread_id() {
        let event = v2::StateEvent {
            payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::ExecutionState(
                    v2::ExecutionState {
                        target: Some(thread_target("thread/mock:alpha")),
                        running: false,
                        ..Default::default()
                    },
                )),
            })),
            ..Default::default()
        };
        assert_eq!(
            state_event_activity(&event),
            DebuggerActivity::Stopped(Some("thread/mock:alpha".to_string()))
        );
    }

    #[test]
    fn thread_resource_updates_do_not_impersonate_execution_transitions() {
        let event = v2::StateEvent {
            payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::Thread(v2::Thread {
                    thread_id: "thread/mock:alpha".to_string(),
                    state: v2::ThreadState::Stopped as i32,
                    ..Default::default()
                })),
            })),
            ..Default::default()
        };

        assert_eq!(state_event_activity(&event), DebuggerActivity::None);
    }
}
