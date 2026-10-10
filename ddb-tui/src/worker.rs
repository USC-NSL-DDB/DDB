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
        ProjectedStateSyncItem, StateSyncOptions,
    },
    model::{BackendRequest, DebuggerActivity, DebuggerEvent, EventStreamStatus, UiMessage},
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const OPERATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_COLLECTION_ITEMS: usize = 10_000;
const MAX_ACTIVE_INSPECTIONS: usize = 4;
const MAX_PENDING_INSPECTIONS: usize = 64;

#[cfg(test)]
#[path = "source_explorer_worker_tests.rs"]
mod source_explorer_tests;

pub async fn run(
    client: ApiClient,
    workspace: std::sync::Arc<crate::source_workspace::Workspace>,
    mut requests: mpsc::Receiver<BackendRequest>,
    messages: mpsc::Sender<UiMessage>,
) {
    let mut inspections = JoinSet::new();
    let mut sources = JoinSet::new();
    let mut discoveries = JoinSet::new();
    let mut logpoint_tasks = JoinSet::new();
    let mut logpoints = crate::logpoints::Registry::default();
    let mut deferred = VecDeque::new();
    let mut pending_inspections = VecDeque::new();
    let mut inspection_generation = None;
    loop {
        while let Some(result) = inspections.try_join_next() {
            report_inspection_failure(result, &messages).await;
        }
        while let Some(result) = logpoint_tasks.try_join_next() {
            report_inspection_failure(result, &messages).await;
        }
        let ready = deferred.pop_front().or_else(|| {
            (inspections.len() < MAX_ACTIVE_INSPECTIONS)
                .then(|| pending_inspections.pop_front())
                .flatten()
        });
        let request = match ready {
            Some(request) => request,
            None => tokio::select! {
                result = logpoint_tasks.join_next(), if !logpoint_tasks.is_empty() => {
                    if let Some(result) = result { report_inspection_failure(result, &messages).await; }
                    continue;
                }
                result = inspections.join_next(), if !inspections.is_empty() => {
                    if let Some(result) = result {
                        report_inspection_failure(result, &messages).await;
                    }
                    continue;
                }
                request = requests.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            },
        };
        let request = match request {
            BackendRequest::ConfigureLogpoint {
                id,
                template,
                instance,
            } => {
                let result =
                    configure_logpoint(&client, &mut logpoints, id, template, &instance).await;
                let update = match result {
                    Ok(ids) => UiMessage::LogpointsConfigured { instance, ids },
                    Err(error) => {
                        UiMessage::Error(format!("Logpoint configuration failed: {error:#}"))
                    }
                };
                let _ = messages.send(update).await;
                continue;
            }
            BackendRequest::ClassifyStop(stop) => {
                if let Some((stop, template)) = logpoints.claim(stop) {
                    if let Some(template) = template {
                        if logpoint_tasks.len() >= 16 {
                            let _ = messages
                                .send(UiMessage::ClassifiedStop {
                                    stop,
                                    error: Some(
                                        "Too many pending logpoints; left thread paused".into(),
                                    ),
                                })
                                .await;
                        } else {
                            let client = client.clone();
                            let messages = messages.clone();
                            logpoint_tasks.spawn(async move {
                                let output = messages.clone();
                                let result = crate::logpoints::run(
                                    &client,
                                    &template,
                                    &stop,
                                    |text| async move {
                                        output.send(UiMessage::LogpointOutput(text)).await?;
                                        Ok(())
                                    },
                                )
                                .await;
                                if !matches!(result, Ok(true)) {
                                    let error = result.err().map(|error| {
                                        format!("Logpoint failed; left thread paused: {error:#}")
                                    });
                                    let _ = messages
                                        .send(UiMessage::ClassifiedStop { stop, error })
                                        .await;
                                }
                            });
                        }
                    } else {
                        let _ = messages
                            .send(UiMessage::ClassifiedStop { stop, error: None })
                            .await;
                    }
                }
                continue;
            }
            request => request,
        };
        if is_recovery(&request) {
            while let Ok(queued) = requests.try_recv() {
                if !is_recovery(&queued) {
                    deferred.push_back(queued);
                }
            }
        }

        if let BackendRequest::LoadSource(request) = request {
            sources.abort_all();
            while let Some(result) = sources.try_join_next() {
                report_inspection_failure(result, &messages).await;
            }
            let client = client.clone();
            let messages = messages.clone();
            let workspace = workspace.clone();
            sources.spawn(async move {
                let generation = request.generation;
                let update = match crate::source_provider::read(&client, &workspace, request).await
                {
                    Ok(source) => UiMessage::Source { generation, source },
                    Err(error) => UiMessage::SourceError {
                        generation,
                        error: format!("{error:#}"),
                    },
                };
                let _ = messages.send(update).await;
            });
        } else if let BackendRequest::ExploreSources(request) = request {
            discoveries.abort_all();
            while let Some(result) = discoveries.try_join_next() {
                report_inspection_failure(result, &messages).await;
            }
            let client = client.clone();
            let messages = messages.clone();
            let workspace = workspace.clone();
            discoveries.spawn(async move {
                let generation = request.generation;
                let result = crate::source_provider::discover(&client, &workspace, request)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = messages
                    .send(UiMessage::SourceDiscovery { generation, result })
                    .await;
            });
        } else if matches!(request, BackendRequest::CancelSourceDiscovery) {
            discoveries.abort_all();
        } else if let Some((generation, _)) = inspection_context(&request) {
            if inspection_generation.is_some_and(|active| active != generation) {
                inspections.abort_all();
                pending_inspections.clear();
            }
            inspection_generation = Some(generation);
            if inspections.len() >= MAX_ACTIVE_INSPECTIONS {
                // Keep reading navigation and control requests while inspection
                // slots are occupied. Bound the backlog independently of HTTP
                // timeouts and report overload through the normal freshness path.
                if pending_inspections.len() < MAX_PENDING_INSPECTIONS {
                    pending_inspections.push_back(request);
                } else {
                    reject_inspection(
                        request,
                        "Inspection queue is full; retry after pending inspections complete".into(),
                        &messages,
                    )
                    .await;
                }
                continue;
            }
            let client = client.clone();
            let messages = messages.clone();
            inspections.spawn(async move {
                let error_target = inspection_error_target(&request);
                if let Err(error) = handle(&client, &messages, request).await {
                    report_inspection_error(error_target, format!("{error:#}"), &messages).await;
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
    sources.shutdown().await;
    discoveries.shutdown().await;
    logpoint_tasks.shutdown().await;
}

async fn configure_logpoint(
    client: &ApiClient,
    registry: &mut crate::logpoints::Registry,
    id: String,
    template: Option<crate::logpoints::Template>,
    instance: &str,
) -> anyhow::Result<Vec<String>> {
    let snapshot = client
        .get_snapshot(v2::GetSnapshotRequest {
            sections: vec![v2::SnapshotSection::Breakpoints as i32],
            ..Default::default()
        })
        .await?
        .snapshot
        .context("DDB omitted breakpoint snapshot")?;
    anyhow::ensure!(
        snapshot.server_instance_id == instance,
        "DDB instance changed during logpoint configuration"
    );
    anyhow::ensure!(
        snapshot.breakpoints.iter().any(|bp| bp.breakpoint_id == id),
        "Breakpoint no longer exists"
    );
    registry.reset(instance);
    registry.retain_active(
        &snapshot
            .breakpoints
            .iter()
            .map(|bp| bp.breakpoint_id.clone())
            .collect(),
    );
    registry
        .configure(instance, id, template)
        .map_err(anyhow::Error::msg)?;
    Ok(registry.ids())
}

async fn reject_inspection(
    request: BackendRequest,
    error: String,
    messages: &mpsc::Sender<UiMessage>,
) {
    let message = match request {
        BackendRequest::InspectExpression(request) => UiMessage::ExpressionInspected {
            request,
            result: Err(error),
        },
        BackendRequest::EvaluateWatch(request) => UiMessage::WatchEvaluated {
            request,
            result: Err(error),
        },
        request => {
            report_inspection_error(inspection_error_target(&request), error, messages).await;
            return;
        }
    };
    let _ = messages.send(message).await;
}

fn inspection_error_target(request: &BackendRequest) -> (u64, String, Option<String>) {
    let (generation, thread_id) = inspection_context(request).expect("inspection request");
    let parent_id = match request {
        BackendRequest::ExpandVariable { variable_id, .. } => Some(variable_id.clone()),
        BackendRequest::ExpandScope { scope_id, .. } => Some(scope_id.clone()),
        _ => None,
    };
    (generation, thread_id, parent_id)
}

async fn report_inspection_error(
    (generation, thread_id, parent_id): (u64, String, Option<String>),
    error: String,
    messages: &mpsc::Sender<UiMessage>,
) {
    let update = match parent_id {
        Some(parent_id) => UiMessage::ExpansionError {
            generation,
            thread_id,
            parent_id,
            error,
        },
        None => UiMessage::InspectionError {
            generation,
            thread_id,
            error,
        },
    };
    let _ = messages.send(update).await;
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

fn inspection_context(request: &BackendRequest) -> Option<(u64, String)> {
    match request {
        BackendRequest::EvaluateWatch(request) => {
            Some((request.generation, request.origin_thread.clone()))
        }
        BackendRequest::InspectExpression(request) => {
            Some((request.generation, request.origin_thread.clone()))
        }
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
        | BackendRequest::ExpandScope {
            thread_id,
            generation,
            ..
        }
        | BackendRequest::ExpandVariable {
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
    match request {
        BackendRequest::ConfigureLogpoint { .. } | BackendRequest::ClassifyStop(_) => {
            bail!("logpoint requests require the serialized worker registry");
        }
        BackendRequest::ConfigureSession { session_id, setup } => {
            setup.run(client, messages, &session_id).await?;
        }
        BackendRequest::EvaluateWatch(request) => {
            let result = evaluate_value(
                client,
                &request.owner_thread,
                &request.frame_id,
                &request.watch.expression,
                v2::EvaluationContext::Watch,
            )
            .await;
            messages
                .send(UiMessage::WatchEvaluated { request, result })
                .await?;
        }
        BackendRequest::InspectExpression(request) => {
            let result = evaluate_value(
                client,
                &request.owner_thread,
                &request.frame_id,
                &request.expression,
                v2::EvaluationContext::Hover,
            )
            .await;
            messages
                .send(UiMessage::ExpressionInspected { request, result })
                .await?;
        }
        BackendRequest::AssignValue { edit, value } => {
            let result = async {
                let admission = match &edit.target {
                    crate::model::ValueEditTarget::Variable(variable_id) => {
                        client
                            .set_variable(v2::SetVariableRequest {
                                target: Some(thread_target(&edit.owner_thread)),
                                variable_id: variable_id.clone(),
                                value,
                                ..Default::default()
                            })
                            .await?
                    }
                    crate::model::ValueEditTarget::Register(name) => {
                        client
                            .evaluate(v2::EvaluateRequest {
                                target: Some(thread_target(&edit.owner_thread)),
                                frame_id: Some(edit.frame_id.clone()),
                                expression: format!("(${name}) = ({value})"),
                                evaluation_context: v2::EvaluationContext::Repl as i32,
                                ..Default::default()
                            })
                            .await?
                    }
                };
                complete_operation(client, admission).await
            }
            .await
            .map_err(|error| format!("{error:#}"));
            messages
                .send(UiMessage::ValueAssigned { edit, result })
                .await?;
        }
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
            generation,
            frame_id,
        } => {
            inspect_scopes(client, messages, &frame_id, &thread_id, generation).await?;
            let registers = list_frame_registers(client, &frame_id).await?;
            messages
                .send(UiMessage::Registers {
                    generation,
                    thread_id,
                    registers,
                })
                .await?;
        }
        BackendRequest::LoadSource(_)
        | BackendRequest::ExploreSources(_)
        | BackendRequest::CancelSourceDiscovery => {
            unreachable!("source requests have their own task lanes")
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

        BackendRequest::PausePeer {
            session_id,
            epoch,
            current_epoch,
        } => {
            if current_epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
                return Ok(());
            }
            let result = async {
                let admission = client
                    .execute(v2::ExecuteRequest {
                        target: Some(crate::api::session_target(&session_id)),
                        action: v2::ExecutionAction::Interrupt as i32,
                        ..Default::default()
                    })
                    .await?;
                complete_operation(client, admission).await
            }
            .await;
            match result {
                Ok(receipt) => {
                    if receipt.state != v2::OperationState::Completed as i32 {
                        messages
                            .send(UiMessage::PeerPauseFailed {
                                session_id: session_id.clone(),
                                epoch,
                                error: format!(
                                    "interrupt operation {} did not complete successfully",
                                    receipt.operation_id
                                ),
                            })
                            .await?;
                    }
                    messages
                        .send(UiMessage::Receipt(
                            format!("all-stop pause {session_id}"),
                            receipt,
                        ))
                        .await?;
                }
                Err(error) => {
                    messages
                        .send(UiMessage::PeerPauseFailed {
                            session_id,
                            epoch,
                            error: format!("{error:#}"),
                        })
                        .await?;
                }
            }
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
            location,
            target,
            options,
        } => {
            let label = match &location {
                breakpoint_spec::Location::Source(source) => {
                    format!("{}:{}", source.source, source.line)
                }
                breakpoint_spec::Location::Function(function) => function.function_name.clone(),
                breakpoint_spec::Location::Address(address) => address.address.clone(),
            };
            let receipt = complete_operation(
                client,
                client
                    .create_breakpoint(v2::CreateBreakpointRequest {
                        context: None,
                        target: Some(target.into_api_target()),
                        breakpoint: Some(v2::BreakpointSpec {
                            location: Some(location),
                            enabled: Some(true),
                            condition: options.condition,
                            ignore_count: options.ignore_count,
                            temporary: options.temporary,
                            hardware: options.hardware,
                        }),
                        preconditions: None,
                    })
                    .await?,
            )
            .await?;
            messages
                .send(UiMessage::Receipt(format!("breakpoint {label}"), receipt))
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
        BackendRequest::ExpandScope {
            scope_id,
            thread_id,
            generation,
        } => {
            let variables = list_scope_variables(client, &scope_id).await?;
            messages
                .send(UiMessage::VariableChildren {
                    generation,
                    thread_id,
                    parent_id: scope_id,
                    variables,
                })
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
            owner_thread,
            generation,
        } => {
            let memory = client
                .read_memory(v2::ReadMemoryRequest {
                    context: None,
                    target: Some(thread_target(owner_thread)),
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
        BackendRequest::RawCommand {
            command,
            target,
            frame_id,
        } => {
            let receipt = complete_operation(
                client,
                client
                    .execute_raw_command(v2::ExecuteRawCommandRequest {
                        context: None,
                        target: Some(target),
                        dialect: v2::RawCommandDialect::BackendNative as i32,
                        command: command.clone(),
                        preconditions: None,
                        frame_id,
                    })
                    .await?,
            )
            .await?;
            messages.send(UiMessage::Receipt(command, receipt)).await?;
        }
    }
    Ok(())
}

fn ui_state_sync_options() -> StateSyncOptions {
    StateSyncOptions {
        // Execution is deliberately not part of DDB's default snapshot.
        // Both refresh and stream rehydration replace the complete UI model.
        sections: [
            v2::SnapshotSection::Topology,
            v2::SnapshotSection::Selection,
            v2::SnapshotSection::Execution,
            v2::SnapshotSection::Breakpoints,
            v2::SnapshotSection::PendingOperations,
            v2::SnapshotSection::Extensions,
            v2::SnapshotSection::Capabilities,
        ]
        .into_iter()
        .map(|section| section as i32)
        .collect(),
        ..Default::default()
    }
}

async fn evaluate_value(
    client: &ApiClient,
    owner: &str,
    frame: &str,
    expression: &str,
    context: v2::EvaluationContext,
) -> Result<v2::EvaluationResult, String> {
    async {
        let admission = client
            .evaluate(v2::EvaluateRequest {
                target: Some(thread_target(owner)),
                frame_id: Some(frame.into()),
                expression: expression.into(),
                evaluation_context: context as i32,
                ..Default::default()
            })
            .await?;
        let receipt = complete_operation(client, admission).await?;
        if receipt.state != v2::OperationState::Completed as i32 {
            anyhow::bail!(
                "{}",
                receipt.error.map_or_else(
                    || "Evaluation did not complete".into(),
                    |error| error.message
                )
            );
        }
        match receipt.result.and_then(|result| result.value) {
            Some(v2::operation_result::Value::Evaluation(value)) => Ok(value),
            _ => anyhow::bail!("DDB returned no evaluation value"),
        }
    }
    .await
    .map_err(|error: anyhow::Error| format!("{error:#}"))
}

async fn refresh(client: &ApiClient, messages: &mpsc::Sender<UiMessage>) -> anyhow::Result<()> {
    let snapshot = client
        .get_snapshot(v2::GetSnapshotRequest {
            context: None,
            sections: ui_state_sync_options().sections,
            target: None,
        })
        .await?
        .snapshot
        .context("GetSnapshot omitted snapshot")?;
    messages.send(UiMessage::RefreshSnapshot(snapshot)).await?;
    Ok(())
}

async fn inspect_thread(
    client: &ApiClient,
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
    let first_frame = result
        .frames
        .iter()
        .find_map(|distributed| distributed.frame.clone());
    messages
        .send(UiMessage::DistributedFrames {
            generation,
            thread_id: thread_id.clone(),
            result,
        })
        .await?;
    if let Some(frame) = first_frame {
        inspect_scopes(client, messages, &frame.frame_id, &thread_id, generation).await?;
        let registers = list_frame_registers(client, &frame.frame_id).await?;
        messages
            .send(UiMessage::Registers {
                generation,
                thread_id: thread_id.clone(),
                registers,
            })
            .await?;
    }
    Ok(())
}

async fn distributed_frames(
    client: &ApiClient,
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
    client: &ApiClient,
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

async fn inspect_scopes(
    client: &ApiClient,
    messages: &mpsc::Sender<UiMessage>,
    frame_id: &str,
    thread_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
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
    messages
        .send(UiMessage::Scopes {
            generation,
            thread_id: thread_id.to_string(),
            scopes: scopes.clone(),
        })
        .await?;
    for scope in scopes
        .into_iter()
        .filter(|scope| !scope.expensive && scope.variable_count != Some(0))
    {
        let variables = list_scope_variables(client, &scope.scope_id).await?;
        messages
            .send(UiMessage::VariableChildren {
                generation,
                thread_id: thread_id.to_string(),
                parent_id: scope.scope_id,
                variables,
            })
            .await?;
    }
    Ok(())
}

async fn list_scope_variables(
    client: &ApiClient,
    scope_id: &str,
) -> anyhow::Result<Vec<v2::Variable>> {
    Ok(client
        .collect_variables(
            v2::ListVariablesRequest {
                context: None,
                scope_id: scope_id.to_string(),
                page: None,
            },
            MAX_COLLECTION_ITEMS,
        )
        .await?)
}

pub(crate) async fn complete_operation(
    client: &ApiClient,
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
        | crate::model::Control::RefreshStack
        | crate::model::Control::RevealFrame => None,
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
    let mut sync = match client.projected_state_sync(ui_state_sync_options()) {
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
                if messages
                    .send(UiMessage::EventStream(EventStreamStatus::Connected))
                    .await
                    .is_err()
                    || messages.send(UiMessage::Snapshot(snapshot)).await.is_err()
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
                let server_instance_id = snapshot.server_instance_id.clone();
                let state_revision = snapshot.base_state_revision;
                let state_event_sequence = snapshot
                    .state_event_cursor
                    .as_ref()
                    .map_or(0, |cursor| cursor.sequence);
                if messages.send(UiMessage::Snapshot(snapshot)).await.is_err()
                    || messages
                        .send(UiMessage::DebuggerEvent(DebuggerEvent {
                            server_instance_id,
                            state_revision,
                            state_event_sequence,
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
            // Session/group projections are already applied in the preceding
            // snapshot. They are not a broadcast resume of every thread.
            target_thread_id(state.target.as_ref())
                .map(DebuggerActivity::Running)
                .unwrap_or(DebuggerActivity::None)
        }
        Some(resource_upsert::Resource::ExecutionState(state)) => {
            let Some(thread_id) = target_thread_id(state.target.as_ref()) else {
                return DebuggerActivity::None;
            };
            // A process-wide stop can project the same cause onto its peer
            // threads. Only the cause thread may trigger inspection or
            // automatic control; the preceding snapshot still updates peers.
            if state
                .stop_reason
                .as_ref()
                .and_then(|reason| reason.thread_id.as_deref())
                .is_some_and(|owner| owner != thread_id)
            {
                return DebuggerActivity::None;
            }
            DebuggerActivity::Stopped(Some(thread_id))
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
    let gap = event.gap.map(|gap| format!("output gap: {}", gap.reason));
    if event.content.is_none() {
        if let Some(gap) = gap.as_ref() {
            return gap.clone();
        }
    }
    let stream = v2::OutputStreamKind::try_from(event.stream)
        .map(|stream| short_enum(stream.as_str_name()))
        .unwrap_or_else(|_| "output".to_string());
    let mut text = match event.content {
        Some(output_event::Content::Text(text)) => text,
        Some(output_event::Content::Data(data)) => String::from_utf8_lossy(&data).into_owned(),
        None => String::new(),
    };
    if event.truncated {
        text.push_str(" …[truncated]");
    }
    let owner = match (event.session_id, event.thread_id) {
        (Some(session), Some(thread)) => format!(" [{session}/{thread}]"),
        (Some(session), None) => format!(" [{session}]"),
        (None, Some(thread)) => format!(" [{thread}]"),
        (None, None) => String::new(),
    };
    let output = format!("{stream}{owner} · {}", text.trim_end_matches('\n'));
    match gap {
        Some(gap) => format!("{gap}\n{output}"),
        None => output,
    }
}

fn short_enum(name: &str) -> String {
    name.rsplit('_').next().unwrap_or(name).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejected_expression_inspection_keeps_its_identity_and_error() {
        let (tx, mut rx) = mpsc::channel(2);
        reject_inspection(
            BackendRequest::InspectExpression(crate::model::ExpressionInspection {
                id: 17,
                expression: "request".into(),
                generation: 9,
                origin_thread: "origin".into(),
                owner_thread: "owner".into(),
                frame_id: "frame".into(),
            }),
            "queue full".into(),
            &tx,
        )
        .await;
        assert!(
            matches!(rx.try_recv().unwrap(), UiMessage::ExpressionInspected { request, result: Err(error) }
            if request.id == 17 && request.generation == 9 && error == "queue full")
        );
    }

    #[tokio::test]
    async fn explicit_inspection_uses_hover_and_the_captured_owner_frame() {
        use axum::{routing::post, Json, Router};
        let router = Router::new().route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Evaluate",
            post(|Json(request): Json<v2::EvaluateRequest>| async move {
                assert_eq!(
                    request.evaluation_context,
                    v2::EvaluationContext::Hover as i32
                );
                assert_eq!(request.target, Some(thread_target("thread/caller")));
                assert_eq!(request.frame_id.as_deref(), Some("frame/caller"));
                assert_eq!(request.expression, "request");
                Json(v2::OperationAdmissionResponse {
                    operation: Some(v2::Operation {
                        state: v2::OperationState::Completed as i32,
                        result: Some(v2::OperationResult {
                            value: Some(v2::operation_result::Value::Evaluation(
                                v2::EvaluationResult {
                                    value: "object".into(),
                                    variable_id: Some("value/child".into()),
                                    has_children: true,
                                    ..Default::default()
                                },
                            )),
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = ApiClient::new(crate::api::ClientConfig::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (tx, mut rx) = mpsc::channel(2);
        handle(
            &client,
            &tx,
            BackendRequest::InspectExpression(crate::model::ExpressionInspection {
                id: 3,
                expression: "request".into(),
                generation: 7,
                origin_thread: "thread/leaf".into(),
                owner_thread: "thread/caller".into(),
                frame_id: "frame/caller".into(),
            }),
        )
        .await
        .unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), UiMessage::ExpressionInspected { request, result: Ok(value) }
            if request.origin_thread == "thread/leaf" && value.has_children && value.variable_id.as_deref() == Some("value/child"))
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn memory_reads_target_the_frame_owner_but_replies_keep_origin_lifetime() {
        use axum::{routing::post, Json, Router};
        let router = Router::new().route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ReadMemory",
            post(|Json(request): Json<v2::ReadMemoryRequest>| async move {
                assert_eq!(request.target, Some(thread_target("thread/caller")));
                assert_eq!(request.address, "0x1234");
                assert_eq!(request.byte_count, 32);
                Json(v2::ReadMemoryResponse {
                    memory: Some(v2::MemoryBlock {
                        address: request.address,
                        data: vec![42; 32],
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
        let (tx, mut rx) = mpsc::channel(2);
        tokio::time::timeout(
            Duration::from_secs(5),
            handle(
                &client,
                &tx,
                BackendRequest::ReadMemory {
                    address: "0x1234".into(),
                    count: 32,
                    thread_id: "thread/origin".into(),
                    owner_thread: "thread/caller".into(),
                    generation: 9,
                },
            ),
        )
        .await
        .expect("memory worker should finish after both replies")
        .unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), UiMessage::Memory { thread_id, generation: 9, memory } if thread_id == "thread/origin" && memory.data == vec![42; 32])
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn pretty_printer_setup_uses_typed_session_configuration_and_preserves_failure() {
        use axum::{routing::post, Json, Router};
        for state in [v2::OperationState::Completed, v2::OperationState::Failed] {
            let router = Router::new().route(
                "/api/v2/rpc/ddb.api.v2.DebuggerControlService/ConfigureDebugger",
                post(
                    move |Json(request): Json<v2::ConfigureDebuggerRequest>| async move {
                        assert_eq!(
                            request.target,
                            Some(crate::api::session_target("session/gdb"))
                        );
                        assert!(matches!(
                            request.setting,
                            Some(v2::configure_debugger_request::Setting::EnablePrettyPrinting(_))
                        ));
                        Json(v2::OperationAdmissionResponse {
                            operation: Some(v2::Operation {
                                operation_id: "operation/configure".into(),
                                state: state as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    },
                ),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
            let (tx, mut rx) = mpsc::channel(1);
            let result = handle(
                &client,
                &tx,
                BackendRequest::ConfigureSession {
                    session_id: "session/gdb".into(),
                    setup: crate::debugger_setup::Settings::default()
                        .plan(
                            v2::BackendKind::Gdb as i32,
                            crate::cli::ValuesFormatting::PrettyPrinters,
                        )
                        .unwrap(),
                },
            )
            .await;
            assert_eq!(result.is_ok(), state == v2::OperationState::Completed);
            assert!(
                matches!(rx.try_recv().unwrap(), UiMessage::Receipt(label, operation)
                if label == "debugger setup for session/gdb" && operation.state == state as i32)
            );
            server.abort();
            let _ = server.await;
        }
    }

    #[test]
    fn byte_backed_output_preserves_text_and_replaces_invalid_utf8() {
        let mut bytes = "client says 你好\n".as_bytes().to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"\x1b[2J");
        let summary = output_event_summary(v2::OutputEvent {
            stream: v2::OutputStreamKind::InferiorStdout as i32,
            session_id: Some("session/client".into()),
            content: Some(output_event::Content::Data(bytes)),
            ..Default::default()
        });
        assert_eq!(
            summary,
            "stdout [session/client] · client says 你好\n�\x1b[2J"
        );
        let mut output = crate::output::OutputBuffer::default();
        output.push(&summary);
        output.fit(120, 10);
        let displayed = output.visible_lines().collect::<Vec<_>>();
        assert_eq!(
            displayed,
            ["stdout [session/client] · client says 你好", "��[2J"]
        );
    }

    #[test]
    fn output_labels_owners_and_preserves_multiline_text_and_spaces() {
        let summary = output_event_summary(v2::OutputEvent {
            session_id: Some("session/server".into()),
            thread_id: Some("thread/worker".into()),
            content: Some(output_event::Content::Text("first\nsecond  \n".into())),
            ..Default::default()
        });
        assert!(summary.contains("session/server/thread/worker"));
        assert!(summary.ends_with("first\nsecond  "));
        let summary = output_event_summary(v2::OutputEvent {
            gap: Some(v2::OutputGap {
                reason: "replay evicted".into(),
                ..Default::default()
            }),
            content: Some(output_event::Content::Text("surviving output".into())),
            ..Default::default()
        });
        assert!(summary.contains("output gap: replay evicted"));
        assert!(summary.contains("surviving output"));
    }

    #[tokio::test]
    async fn superseded_peer_pause_never_contacts_backend() {
        let client = ApiClient::new(crate::api::ClientConfig::new("http://127.0.0.1:1")).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let current_epoch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(2));
        handle(
            &client,
            &tx,
            BackendRequest::PausePeer {
                session_id: "server".into(),
                epoch: 1,
                current_epoch,
            },
        )
        .await
        .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn source_navigation_beyond_eof_loads_the_last_window_without_a_frame() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        type Calls = Arc<Mutex<Vec<u32>>>;
        async fn read(
            State(calls): State<Calls>,
            Json(request): Json<v2::ReadSourceRequest>,
        ) -> Json<v2::ReadSourceResponse> {
            calls.lock().unwrap().push(request.start_line);
            assert_eq!(request.source_reference, "src-workspace");
            let beyond = request.start_line > 3;
            Json(v2::ReadSourceResponse {
                context: Some(v2::ResponseContext {
                    server_instance_id: "test".into(),
                    ..Default::default()
                }),
                source: Some(v2::SourceContent {
                    source: Some(v2::SourceFile {
                        source_reference: request.source_reference,
                        path: Some("project/main.rs".into()),
                        ..Default::default()
                    }),
                    start_line: request.start_line,
                    content: if beyond {
                        String::new()
                    } else {
                        "one\ntwo\nthree".into()
                    },
                    line_count: if beyond { 0 } else { 3 },
                    total_lines: 3,
                    has_more: false,
                }),
            })
        }
        let calls = Calls::default();
        let router = Router::new()
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerService/ReadSource",
                post(read),
            )
            .with_state(Arc::clone(&calls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
        let mut browser = crate::source_browser::SourceBrowser::default();
        let request = browser.open_source(crate::source_browser::tests::remote_source(
            v2::SourceFile {
                source_reference: "src-workspace".into(),
                ..Default::default()
            },
        ));
        // Navigate an already resolved document beyond its actual end.
        let initial = crate::source_browser::tests::remote_content(v2::SourceContent {
            source: Some(v2::SourceFile {
                source_reference: "src-workspace".into(),
                ..Default::default()
            }),
            total_lines: 1000,
            ..Default::default()
        });
        browser.apply(request.generation, initial);
        let request = browser.go_to_line(999).unwrap();
        let generation = request.generation;
        let content = crate::source_provider::read(
            &client,
            &crate::source_workspace::Workspace::default(),
            request,
        )
        .await
        .unwrap();
        assert!(browser.apply(generation, content));
        assert_eq!(browser.document().unwrap().cursor, 3);
        assert_eq!(browser.document().unwrap().lines, ["one", "two", "three"]);
        assert_eq!(*calls.lock().unwrap(), [899, 1]);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn typed_scope_loading_defers_expensive_reads_and_pages_variable_children() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        type Calls = Arc<Mutex<Vec<String>>>;
        async fn scopes(
            Json(request): Json<v2::ListScopesRequest>,
        ) -> Json<v2::ListScopesResponse> {
            assert_eq!(request.frame_id, "frame/parent");
            Json(v2::ListScopesResponse {
                scopes: vec![
                    v2::Scope {
                        scope_id: "locals".into(),
                        name: "Locals".into(),
                        variable_count: Some(1),
                        ..Default::default()
                    },
                    v2::Scope {
                        scope_id: "globals".into(),
                        name: "Globals".into(),
                        expensive: true,
                        ..Default::default()
                    },
                    v2::Scope {
                        scope_id: "empty".into(),
                        name: "Arguments".into(),
                        variable_count: Some(0),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
        }
        async fn variables(
            State(calls): State<Calls>,
            Json(request): Json<v2::ListVariablesRequest>,
        ) -> Json<v2::ListVariablesResponse> {
            calls.lock().unwrap().push(request.scope_id.clone());
            Json(v2::ListVariablesResponse {
                variables: vec![v2::Variable {
                    variable_id: format!("{}/value", request.scope_id),
                    name: "value".into(),
                    has_children: true,
                    ..Default::default()
                }],
                ..Default::default()
            })
        }
        async fn expand(
            State(calls): State<Calls>,
            Json(request): Json<v2::ExpandVariableRequest>,
        ) -> Json<v2::ExpandVariableResponse> {
            assert_eq!(request.variable_id, "globals/value");
            let second = request
                .page
                .as_ref()
                .and_then(|page| page.page_token.as_deref())
                == Some("next");
            calls.lock().unwrap().push(format!("expand/{second}"));
            Json(v2::ExpandVariableResponse {
                variables: vec![v2::Variable {
                    variable_id: format!("child/{second}"),
                    name: format!("child/{second}"),
                    ..Default::default()
                }],
                page: if second {
                    None
                } else {
                    Some(v2::PageInfo {
                        next_page_token: Some("next".into()),
                    })
                },
                ..Default::default()
            })
        }
        let calls = Calls::default();
        let router = Router::new()
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerService/ListScopes",
                post(scopes),
            )
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerService/ListVariables",
                post(variables),
            )
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerService/ExpandVariable",
                post(expand),
            )
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
        let (messages, mut updates) = mpsc::channel(8);
        inspect_scopes(&client, &messages, "frame/parent", "thread/child", 9)
            .await
            .unwrap();
        assert!(
            matches!(updates.recv().await.unwrap(), UiMessage::Scopes { generation: 9, thread_id, scopes }
            if thread_id == "thread/child" && scopes.len() == 3)
        );
        assert!(
            matches!(updates.recv().await.unwrap(), UiMessage::VariableChildren { parent_id, .. } if parent_id == "locals")
        );
        assert_eq!(*calls.lock().unwrap(), ["locals"]);
        assert!(updates.try_recv().is_err());

        let (requests, request_rx) = mpsc::channel(4);
        let worker = tokio::spawn(run(client, Arc::default(), request_rx, messages));
        requests
            .send(BackendRequest::ExpandScope {
                scope_id: "globals".into(),
                thread_id: "thread/child".into(),
                generation: 9,
            })
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(update, UiMessage::VariableChildren { generation: 9, parent_id, variables, .. }
            if parent_id == "globals" && variables[0].variable_id == "globals/value")
        );
        requests
            .send(BackendRequest::ExpandVariable {
                variable_id: "globals/value".into(),
                thread_id: "thread/child".into(),
                generation: 9,
            })
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(update, UiMessage::VariableChildren { generation: 9, parent_id, variables, .. }
            if parent_id == "globals/value" && variables.len() == 2)
        );
        assert_eq!(
            *calls.lock().unwrap(),
            ["locals", "globals", "expand/false", "expand/true"]
        );
        drop(requests);
        worker.await.unwrap();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn expanding_a_variable_keeps_the_worker_alive() {
        let client = ApiClient::new(crate::api::ClientConfig::new("http://127.0.0.1:1")).unwrap();
        let (requests, request_rx) = mpsc::channel(4);
        let (messages, mut message_rx) = mpsc::channel(4);
        let worker = tokio::spawn(run(client, std::sync::Arc::default(), request_rx, messages));
        requests
            .send(BackendRequest::ExpandVariable {
                variable_id: "value".into(),
                thread_id: "thread".into(),
                generation: 7,
            })
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(5), message_rx.recv())
            .await
            .unwrap();
        assert!(matches!(message, Some(UiMessage::ExpansionError {
            generation: 7, thread_id, ..
        }) if thread_id == "thread"));
        assert!(!worker.is_finished());
        drop(requests);
        worker.await.unwrap();
    }

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
    fn process_wide_stop_projections_do_not_repeat_the_originating_stop() {
        let event_for = |target: v2::Target, origin: Option<&str>| v2::StateEvent {
            payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::ExecutionState(
                    v2::ExecutionState {
                        target: Some(target),
                        stop_reason: Some(v2::StopReason {
                            thread_id: origin.map(str::to_owned),
                            kind: v2::StopReasonKind::Breakpoint as i32,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
            })),
            ..Default::default()
        };
        let owner = "thread/server:rpc";
        assert_eq!(
            state_event_activity(&event_for(thread_target(owner), Some(owner))),
            DebuggerActivity::Stopped(Some(owner.into()))
        );
        assert_eq!(
            state_event_activity(&event_for(
                thread_target("thread/server:worker"),
                Some(owner)
            )),
            DebuggerActivity::None
        );
        assert_eq!(
            state_event_activity(&event_for(
                crate::api::session_target("session/server"),
                Some(owner)
            )),
            DebuggerActivity::None
        );
        // Backends may omit the cause thread. The canonical target still owns
        // this stop, so stepping and explicit interruption must keep working.
        assert_eq!(
            state_event_activity(&event_for(thread_target(owner), None)),
            DebuggerActivity::Stopped(Some(owner.into()))
        );
    }

    #[test]
    fn aggregate_running_states_do_not_impersonate_global_thread_resumes() {
        for target in [
            None,
            Some(crate::api::session_target("session/server")),
            Some(crate::api::group_target("group/server")),
        ] {
            let event = v2::StateEvent {
                payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                    resource: Some(resource_upsert::Resource::ExecutionState(
                        v2::ExecutionState {
                            target,
                            running: true,
                            ..Default::default()
                        },
                    )),
                })),
                ..Default::default()
            };
            assert_eq!(state_event_activity(&event), DebuggerActivity::None);
        }
        let event = v2::StateEvent {
            payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::ExecutionState(
                    v2::ExecutionState {
                        target: Some(thread_target("thread/server")),
                        running: true,
                        ..Default::default()
                    },
                )),
            })),
            ..Default::default()
        };
        assert_eq!(
            state_event_activity(&event),
            DebuggerActivity::Running("thread/server".into())
        );
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
