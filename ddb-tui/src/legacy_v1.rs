//! Explicit migration adapter for the frozen DDB API v1 contract.
//!
//! Numeric v1 IDs are wrapped in visibly namespaced strings only for the TUI
//! view model and decoded exclusively here. No opaque v2 ID is ever inferred.

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ddb_api_client::v2::{
    self, breakpoint_spec, extension_payload, operation_result, target, BreakpointFeature,
    ExecutionAction, OperationKind, ResourceKind,
};
use futures_util::{SinkExt, StreamExt};
use reqwest::{header::CONTENT_LENGTH, Method, Url};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use crate::{
    api::BreakpointTarget,
    model::{BackendRequest, DebuggerActivity, DebuggerEvent, EventStreamStatus, UiMessage},
};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_LINES: usize = 2_000;
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(100);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);
static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    websocket: Url,
    bearer_token: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyV1Client")
            .field("base", &self.base)
            .field("authenticated", &self.bearer_token.is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(endpoint: &str, bearer_token: Option<&str>) -> Result<Self> {
        let mut endpoint = Url::parse(endpoint).context("invalid DDB v1 endpoint")?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            bail!("DDB v1 endpoint scheme must be http or https");
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            bail!("supply credentials with --token, not in the DDB endpoint URL");
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        endpoint.set_path(&format!(
            "{}/api/v1/",
            endpoint.path().trim_end_matches('/')
        ));

        let mut websocket = endpoint.clone();
        websocket
            .set_scheme(if endpoint.scheme() == "https" {
                "wss"
            } else {
                "ws"
            })
            .map_err(|_| anyhow!("failed to derive DDB v1 WebSocket endpoint"))?;
        websocket = websocket.join("events")?;

        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(30))
                .build()?,
            base: endpoint,
            websocket,
            bearer_token: bearer_token.map(str::to_string),
        })
    }

    pub async fn handshake(&self) -> Result<()> {
        let service = self.get("").await?;
        if service.get("name").and_then(Value::as_str) != Some("ddb") {
            bail!("v1 service discovery did not identify DDB");
        }
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, self.base.join(path)?, None).await
    }

    async fn send(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        self.request(method, self.base.join(path)?, Some(body))
            .await
    }

    async fn request(&self, method: Method, url: Url, body: Option<Value>) -> Result<Value> {
        let mut request = self.http.request(method, url.clone());
        if let Some(token) = self.bearer_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("DDB v1 request to {} failed", safe_url(&url)))?;
        let status = response.status();
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > MAX_RESPONSE_BYTES)
        {
            bail!("DDB v1 response exceeded the {MAX_RESPONSE_BYTES}-byte client limit");
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read DDB v1 response")?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                bail!("DDB v1 response exceeded the {MAX_RESPONSE_BYTES}-byte client limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("DDB v1 returned invalid JSON with HTTP status {status}"))?;
        if !status.is_success() {
            let code = value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error");
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("DDB v1 request failed");
            bail!("DDB v1 {code}: {message} (HTTP {status})");
        }
        if value.get("api_version").and_then(Value::as_str) != Some("v1") {
            bail!("DDB v1 response omitted the v1 envelope marker");
        }
        value
            .get("data")
            .cloned()
            .context("DDB v1 response omitted data")
    }

    async fn source(&self, path: &str, line: usize) -> Result<v2::SourceContent> {
        let start_line = line.saturating_sub(100).max(1);
        let end_line = start_line.saturating_add(239);
        let mut url = self.base.join("sources/content")?;
        url.query_pairs_mut()
            .append_pair("path", path)
            .append_pair("start_line", &start_line.to_string())
            .append_pair("end_line", &end_line.to_string());
        let data = self.request(Method::GET, url, None).await?;
        let returned_path = required_str(&data, "path")?.to_string();
        let first = required_u64(&data, "start_line")?;
        let last = required_u64(&data, "end_line")?;
        let total = required_u64(&data, "total_lines")?;
        let lines = required_array(&data, "lines")?
            .iter()
            .map(|line| {
                line.as_str()
                    .map(str::to_string)
                    .context("DDB v1 source line is not a string")
            })
            .collect::<Result<Vec<_>>>()?;
        if lines.len() > MAX_SOURCE_LINES {
            bail!("DDB v1 source response exceeded the client line limit");
        }
        let first = u32::try_from(first).context("DDB v1 source start line exceeds u32")?;
        Ok(v2::SourceContent {
            source: Some(v2::SourceFile {
                source_reference: format!("v1:source:{returned_path}"),
                path: Some(returned_path.clone()),
                name: returned_path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&returned_path)
                    .to_string(),
                media_type: "text/plain".to_string(),
                content_hash: None,
            }),
            start_line: first,
            content: lines.join("\n"),
            line_count: u32::try_from(lines.len())?,
            has_more: last < total,
        })
    }

    fn websocket_request(&self) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
        let mut request = self.websocket.as_str().into_client_request()?;
        if let Some(token) = self.bearer_token.as_deref() {
            request.headers_mut().insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        Ok(request)
    }
}

pub async fn handle(
    client: &Client,
    messages: &mpsc::Sender<UiMessage>,
    request: BackendRequest,
) -> Result<()> {
    match request {
        BackendRequest::Bootstrap => {
            client.handshake().await?;
            messages
                .send(UiMessage::Capabilities(legacy_capabilities(
                    client.get("capabilities").await?,
                )?))
                .await?;
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
            messages
                .send(UiMessage::Variables {
                    generation,
                    thread_id: thread_id.clone(),
                    variables: variables(client, &owner_thread_id, &frame_id).await?,
                })
                .await?;
            if let (Some(source), Some(line)) = (source, line) {
                messages
                    .send(UiMessage::Source {
                        generation,
                        thread_id,
                        source: client.source(&source, line as usize).await?,
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
            messages
                .send(UiMessage::Source {
                    generation,
                    thread_id,
                    source: client.source(&source, line).await?,
                    line,
                })
                .await?;
        }
        BackendRequest::ListSignals { .. } => {
            messages
                .send(UiMessage::Notice(
                    "typed signal discovery requires DDB API v2; enter a signal name manually"
                        .to_string(),
                ))
                .await?;
        }

        BackendRequest::Control(control, target) => {
            let action = control
                .action_name()
                .context("only execution controls reach the v1 adapter")?;
            client
                .send(
                    Method::POST,
                    "execution",
                    json!({"action": action, "target": target_json(&target)?}),
                )
                .await?;
            send_receipt(messages, action, OperationKind::Execute, None).await?;
        }
        BackendRequest::Jump { location, target } => {
            client
                .send(
                    Method::POST,
                    "execution",
                    json!({
                        "action": "jump",
                        "target": target_json(&target)?,
                        "location": location,
                    }),
                )
                .await?;
            send_receipt(
                messages,
                &format!("jump to {location}"),
                OperationKind::Execute,
                None,
            )
            .await?;
        }
        BackendRequest::SendSignal { signal, target } => {
            client
                .send(
                    Method::POST,
                    "execution",
                    json!({
                        "action": "send_signal",
                        "target": target_json(&target)?,
                        "signal": signal,
                    }),
                )
                .await?;
            send_receipt(
                messages,
                &format!("signal {signal}"),
                OperationKind::Execute,
                None,
            )
            .await?;
        }
        BackendRequest::CreateBreakpoint {
            source,
            line,
            target,
            options,
        } => {
            client
                .send(
                    Method::POST,
                    "breakpoints",
                    json!({
                        "source": source,
                        "line": line,
                        "target": breakpoint_target_json(target)?,
                        "condition": options.condition,
                        "temporary": options.temporary,
                        "hardware": options.hardware,
                    }),
                )
                .await?;
            send_receipt(
                messages,
                &format!("breakpoint {source}:{line}"),
                OperationKind::CreateBreakpoint,
                None,
            )
            .await?;
        }
        BackendRequest::DeleteBreakpoint { id, .. } => {
            let id = decode_id(&id, "breakpoint")?;
            client
                .send(Method::DELETE, &format!("breakpoints/{id}"), json!({}))
                .await?;
            send_receipt(
                messages,
                &format!("delete breakpoint {id}"),
                OperationKind::DeleteBreakpoint,
                None,
            )
            .await?;
        }
        BackendRequest::SetBreakpointEnabled { id, enabled, .. } => {
            let id = decode_id(&id, "breakpoint")?;
            client
                .send(
                    Method::PATCH,
                    &format!("breakpoints/{id}"),
                    json!({"enabled": enabled}),
                )
                .await?;
            send_receipt(
                messages,
                &format!(
                    "{} breakpoint {id}",
                    if enabled { "enable" } else { "disable" }
                ),
                OperationKind::UpdateBreakpoint,
                None,
            )
            .await?;
        }
        BackendRequest::Evaluate {
            expression,
            thread_id,
            frame_id,
        } => {
            let frame = frame_id
                .as_deref()
                .map(decode_frame_id)
                .transpose()?
                .map(|(_, level)| level);
            let data = client
                .send(
                    Method::POST,
                    "evaluate",
                    json!({
                        "expression": expression,
                        "target": thread_target_json(&thread_id)?,
                        "frame": frame,
                    }),
                )
                .await?;
            let value = command_payloads(&data)
                .into_iter()
                .find_map(|payload| payload.get("value").and_then(Value::as_str))
                .unwrap_or("<no value>")
                .to_string();
            send_receipt(
                messages,
                &format!("evaluate {expression}"),
                OperationKind::Evaluate,
                Some(v2::OperationResult {
                    value: Some(operation_result::Value::Evaluation(v2::EvaluationResult {
                        expression,
                        value,
                        ..Default::default()
                    })),
                }),
            )
            .await?;
        }
        BackendRequest::ExpandVariable { .. } => {
            messages
                .send(UiMessage::Notice(
                    "variable expansion requires the DDB v2 API".to_string(),
                ))
                .await?;
        }
        BackendRequest::ReadMemory {
            address,
            count,
            thread_id,
            generation,
        } => {
            let data = client
                .send(
                    Method::POST,
                    "memory/read",
                    json!({
                        "address": address,
                        "count": count,
                        "target": thread_target_json(&thread_id)?,
                    }),
                )
                .await?;
            messages
                .send(UiMessage::Memory {
                    generation,
                    thread_id,
                    memory: memory_block(&data)?,
                })
                .await?;
            messages
                .send(UiMessage::Notice(format!(
                    "memory {address} ({count} bytes)"
                )))
                .await?;
        }
        BackendRequest::InvokeExtensionAction { .. } => {
            messages
                .send(UiMessage::Notice(
                    "extension actions require the DDB v2 API".to_string(),
                ))
                .await?;
        }
        BackendRequest::RawCommand { command, target } => {
            let data = client
                .send(
                    Method::POST,
                    "commands",
                    json!({"command": command, "target": target_json(&target)?, "wait": true}),
                )
                .await?;
            let text = serde_json::to_string(&data)
                .ok()
                .map(|text| text.chars().take(1_024).collect());
            send_receipt(
                messages,
                &command,
                OperationKind::RawCommand,
                Some(v2::OperationResult {
                    value: Some(operation_result::Value::RawCommand(v2::RawCommandResult {
                        value: None,
                        text,
                        truncated: false,
                    })),
                }),
            )
            .await?;
        }
    }
    Ok(())
}

async fn send_receipt(
    messages: &mpsc::Sender<UiMessage>,
    label: &str,
    kind: OperationKind,
    result: Option<v2::OperationResult>,
) -> Result<()> {
    messages
        .send(UiMessage::Receipt(
            label.to_string(),
            completed_operation(kind, result),
        ))
        .await?;
    Ok(())
}

async fn refresh(client: &Client, messages: &mpsc::Sender<UiMessage>) -> Result<()> {
    let snapshot = legacy_snapshot(client.get("state").await?)?;
    let threads = snapshot.threads.clone();
    messages.send(UiMessage::Snapshot(snapshot)).await?;
    messages.send(UiMessage::Threads(threads)).await?;
    Ok(())
}

async fn inspect_thread(
    client: &Client,
    messages: &mpsc::Sender<UiMessage>,
    thread_id: String,
    generation: u64,
) -> Result<()> {
    let numeric_thread = decode_id(&thread_id, "thread")?;
    client
        .send(
            Method::POST,
            "threads/select",
            json!({"thread_id": numeric_thread}),
        )
        .await?;
    let frames = frames(client, &thread_id).await?;
    let first = frames.iter().min_by_key(|frame| frame.level).cloned();
    messages
        .send(UiMessage::Frames {
            generation,
            thread_id: thread_id.clone(),
            frames,
        })
        .await?;
    if let Some(frame) = first {
        messages
            .send(UiMessage::Variables {
                generation,
                thread_id: thread_id.clone(),
                variables: variables(client, &thread_id, &frame.frame_id).await?,
            })
            .await?;
        if let Some(location) = frame.location {
            if let Some(path) = location.path {
                messages
                    .send(UiMessage::Source {
                        generation,
                        thread_id,
                        source: client.source(&path, location.line as usize).await?,
                        line: location.line as usize,
                    })
                    .await?;
            }
        }
    }
    Ok(())
}

async fn frames(client: &Client, thread_id: &str) -> Result<Vec<v2::Frame>> {
    let numeric_thread = decode_id(thread_id, "thread")?;
    let data = client
        .send(
            Method::POST,
            "stack/frames",
            json!({"thread_id": numeric_thread, "low": 0, "high": 1024}),
        )
        .await?;
    let mut frames = Vec::new();
    for payload in command_payloads(&data) {
        for frame in payload
            .get("stack")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let level = string_u64(frame, "level")?;
            frames.push(v2::Frame {
                frame_id: encode_frame_id(numeric_thread, level),
                thread_id: thread_id.to_string(),
                level: u32::try_from(level)?,
                function_name: optional_string(frame, &["func"]),
                location: Some(v2::SourceLocation {
                    path: optional_string(frame, &["fullname", "file"]),
                    line: optional_string(frame, &["line"])
                        .and_then(|line| line.parse().ok())
                        .unwrap_or_default(),
                    address: optional_string(frame, &["addr"]),
                    function_name: optional_string(frame, &["func"]),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
    }
    Ok(frames)
}

async fn variables(client: &Client, thread_id: &str, frame_id: &str) -> Result<Vec<v2::Variable>> {
    let numeric_thread = decode_id(thread_id, "thread")?;
    let (frame_thread, level) = decode_frame_id(frame_id)?;
    if frame_thread != numeric_thread {
        bail!("v1 frame does not belong to the selected thread");
    }
    let data = client
        .send(
            Method::POST,
            "stack/variables",
            json!({"thread_id": numeric_thread, "frame": level, "values": "simple"}),
        )
        .await?;
    let mut variables = Vec::new();
    for payload in command_payloads(&data) {
        for (index, variable) in payload
            .get("variables")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let name = required_str(variable, "name")?.to_string();
            let child_count = optional_string(variable, &["numchild"])
                .and_then(|value| value.parse::<u64>().ok());
            variables.push(v2::Variable {
                variable_id: format!("v1:variable:{numeric_thread}:{level}:{index}"),
                name: name.clone(),
                value: optional_string(variable, &["value"]).unwrap_or_default(),
                type_name: optional_string(variable, &["type"]),
                has_children: child_count.is_some_and(|count| count > 0),
                child_count,
                evaluate_name: Some(name),
                ..Default::default()
            });
        }
    }
    Ok(variables)
}

pub async fn watch_events(client: Client, messages: mpsc::Sender<UiMessage>) {
    let mut delay = INITIAL_RECONNECT_DELAY;
    let mut was_connected = false;
    loop {
        let request = match client.websocket_request() {
            Ok(request) => request,
            Err(error) => {
                let _ = messages.send(UiMessage::Error(error.to_string())).await;
                return;
            }
        };
        if let Ok((mut socket, _)) = connect_async(request).await {
            if was_connected
                && messages
                    .send(UiMessage::Output(
                        "v1 output replay is unavailable; output may have been lost".to_string(),
                    ))
                    .await
                    .is_err()
            {
                return;
            }
            was_connected = true;
            delay = INITIAL_RECONNECT_DELAY;
            if messages
                .send(UiMessage::EventStream(EventStreamStatus::Connected))
                .await
                .is_err()
            {
                return;
            }
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let value = match serde_json::from_str::<Value>(&text) {
                            Ok(value) => value,
                            Err(error) => {
                                if messages
                                    .send(UiMessage::Error(format!(
                                        "invalid DDB v1 event: {error}"
                                    )))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                continue;
                            }
                        };
                        if dispatch_notification(&messages, &value).await.is_err() {
                            return;
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
        if messages
            .send(UiMessage::EventStream(EventStreamStatus::Reconnecting(
                "v1 WebSocket disconnected".to_string(),
            )))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
    }
}

async fn dispatch_notification(messages: &mpsc::Sender<UiMessage>, value: &Value) -> Result<()> {
    if value.get("type").and_then(Value::as_str) == Some("welcome") {
        return Ok(());
    }
    let event_type = value
        .pointer("/payload/type")
        .and_then(Value::as_str)
        .context("DDB v1 notification omitted payload.type")?;
    match event_type {
        "DebuggerOutput" => {
            for record in value
                .pointer("/payload/data/records")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let stream = record
                    .get("stream")
                    .and_then(Value::as_str)
                    .unwrap_or("output");
                let event = record
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let payload = record.get("payload");
                if event == "output" {
                    if let Some(message) = payload
                        .and_then(|payload| payload.get("message"))
                        .and_then(Value::as_str)
                    {
                        messages
                            .send(UiMessage::Output(format!(
                                "{stream}: {}",
                                message.trim_end_matches(['\r', '\n'])
                            )))
                            .await?;
                    }
                    continue;
                }
                let thread_id = payload
                    .and_then(|payload| payload.get("thread-id"))
                    .and_then(json_u64)
                    .map(|id| encode_id("thread", id));
                let activity = match event {
                    "running" => DebuggerActivity::Running(thread_id),
                    "stopped" => DebuggerActivity::Stopped(thread_id),
                    _ => DebuggerActivity::None,
                };
                let refresh = !matches!(activity, DebuggerActivity::None)
                    || event.starts_with("thread-")
                    || event.starts_with("breakpoint-");
                messages
                    .send(UiMessage::DebuggerEvent(DebuggerEvent {
                        summary: format!("{stream} · {event}"),
                        refresh,
                        activity,
                    }))
                    .await?;
            }
        }
        "BreakpointChanged" | "SessionStatusChanged" | "SessionListChanged" | "Custom" => {
            messages
                .send(UiMessage::DebuggerEvent(DebuggerEvent {
                    summary: format!("v1 · {event_type}"),
                    refresh: true,
                    activity: DebuggerActivity::None,
                }))
                .await?;
        }
        _ => {}
    }
    Ok(())
}

fn legacy_capabilities(data: Value) -> Result<v2::Capabilities> {
    let execution_actions = string_array(&data, "execution_actions")?
        .into_iter()
        .filter_map(|action| {
            Some(match action.as_str() {
                "continue" => ExecutionAction::Continue,
                "interrupt" => ExecutionAction::Interrupt,
                "next" => ExecutionAction::Next,
                "step_in" => ExecutionAction::StepIn,
                "step_out" => ExecutionAction::StepOut,
                "jump" => ExecutionAction::Jump,
                "send_signal" => ExecutionAction::Signal,
                _ => return None,
            } as i32)
        })
        .collect::<Vec<_>>();

    let breakpoint_actions = string_array(&data, "breakpoint_actions")?;
    let mut breakpoint_features = Vec::new();
    for action in &breakpoint_actions {
        let feature = match action.as_str() {
            "create" | "delete" => BreakpointFeature::Source,
            "enable" | "disable" => BreakpointFeature::EnableDisable,
            "conditional" => BreakpointFeature::Condition,
            "temporary" => BreakpointFeature::Temporary,
            "hardware" => BreakpointFeature::Hardware,
            _ => continue,
        } as i32;
        if !breakpoint_features.contains(&feature) {
            breakpoint_features.push(feature);
        }
    }
    if string_array(&data, "ddb_features")?
        .iter()
        .any(|feature| feature == "group_breakpoints")
    {
        breakpoint_features.push(BreakpointFeature::Distributed as i32);
        breakpoint_features.push(BreakpointFeature::GroupInheritance as i32);
    }

    let resources = string_array(&data, "resources")?
        .into_iter()
        .filter_map(|resource| {
            Some(match resource.as_str() {
                "sessions" => ResourceKind::Session,
                "groups" => ResourceKind::Group,
                "threads" => ResourceKind::Thread,
                "breakpoints" => ResourceKind::Breakpoint,
                "pending_commands" => ResourceKind::PendingCommand,
                _ => return None,
            } as i32)
        })
        .collect::<Vec<_>>();
    let inspection = string_array(&data, "inspection")?;
    let generic_commands = data
        .pointer("/protocol/generic_command_passthrough")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ddb_features = string_array(&data, "ddb_features")?;

    let mut supported_operations = Vec::new();
    if !execution_actions.is_empty() {
        supported_operations.push(OperationKind::Execute as i32);
    }
    if string_array(&data, "target_kinds")?
        .iter()
        .any(|kind| kind == "thread")
    {
        supported_operations.push(OperationKind::SelectThread as i32);
    }
    if inspection.iter().any(|item| item == "evaluate") {
        supported_operations.push(OperationKind::Evaluate as i32);
    }
    if breakpoint_actions.iter().any(|item| item == "create") {
        supported_operations.push(OperationKind::CreateBreakpoint as i32);
    }
    if breakpoint_actions
        .iter()
        .any(|item| matches!(item.as_str(), "enable" | "disable"))
    {
        supported_operations.push(OperationKind::UpdateBreakpoint as i32);
    }
    if breakpoint_actions.iter().any(|item| item == "delete") {
        supported_operations.push(OperationKind::DeleteBreakpoint as i32);
    }
    if generic_commands {
        supported_operations.push(OperationKind::RawCommand as i32);
    }
    if ddb_features
        .iter()
        .any(|feature| feature == "distributed_backtrace")
    {
        supported_operations.push(OperationKind::DistributedBacktrace as i32);
    }

    let backend_name = data
        .pointer("/runtime/backend")
        .and_then(Value::as_str)
        .unwrap_or("other");
    let backend_kind = match backend_name.to_ascii_lowercase().as_str() {
        "mock" => v2::BackendKind::Mock,
        "gdb" => v2::BackendKind::Gdb,
        "lldb" => v2::BackendKind::Lldb,
        _ => v2::BackendKind::Other,
    };
    let framework_name = data
        .pointer("/runtime/framework")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let frameworks = (!framework_name.eq_ignore_ascii_case("none"))
        .then(|| v2::FrameworkDescriptor {
            framework_id: format!("v1:framework:{}", framework_name.to_ascii_lowercase()),
            display_name: framework_name.to_string(),
            version: None,
        })
        .into_iter()
        .collect();

    Ok(v2::Capabilities {
        capabilities_id: "v1:capabilities".to_string(),
        api_version: "v1".to_string(),
        schema_version: "1".to_string(),
        server_instance_id: "v1:untracked-server-instance".to_string(),
        transports: vec![v2::TransportEndpoint {
            transport: v2::TransportKind::Http as i32,
            uri: "/api/v1".to_string(),
            encodings: vec![v2::WireEncoding::Json as i32],
            tls_required: false,
        }],
        backends: vec![v2::BackendDescriptor {
            kind: backend_kind as i32,
            version: None,
            capability_namespace: Some(format!(
                "ddb.debugger.{}",
                backend_name.to_ascii_lowercase()
            )),
        }],
        frameworks,
        supported_resources: resources,
        supported_operations,
        execution_actions,
        breakpoint_features,
        state_event_kinds: Vec::new(),
        output_stream_kinds: vec![
            v2::OutputStreamKind::Console as i32,
            v2::OutputStreamKind::Log as i32,
            v2::OutputStreamKind::Target as i32,
        ],
        cancellable_operation_kinds: Vec::new(),
        ddb_features,
        limits: Some(v2::ApiLimits {
            max_response_bytes: MAX_RESPONSE_BYTES as u64,
            max_memory_read_bytes: 1024 * 1024,
            max_source_lines: MAX_SOURCE_LINES as u32,
            ..Default::default()
        }),
        authentication_mode: "v1-deployment-policy".to_string(),
        extensions: legacy_extension_descriptors(data.get("extensions"))?,
        ..Default::default()
    })
}

fn legacy_snapshot(data: Value) -> Result<v2::Snapshot> {
    let selected_session = data.get("selected_session_id").and_then(json_u64);
    let selected_thread = data.get("selected_thread_id").and_then(json_u64);

    let sessions = required_array(&data, "sessions")?
        .iter()
        .map(legacy_session)
        .collect::<Result<Vec<_>>>()?;
    let groups = required_array(&data, "groups")?
        .iter()
        .map(|group| legacy_group(group, selected_session))
        .collect::<Result<Vec<_>>>()?;
    let processes = data
        .get("processes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(legacy_process)
        .collect::<Result<Vec<_>>>()?;
    let threads = required_array(&data, "threads")?
        .iter()
        .map(legacy_thread)
        .collect::<Result<Vec<_>>>()?;
    let breakpoints = required_array(&data, "breakpoints")?
        .iter()
        .map(legacy_breakpoint)
        .collect::<Result<Vec<_>>>()?;
    let pending_commands = data
        .get("pending_command_details")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(legacy_pending_command)
        .collect::<Result<Vec<_>>>()?;

    let selected_group = selected_session.and_then(|session_id| {
        groups
            .iter()
            .find(|group| {
                group
                    .session_ids
                    .contains(&encode_id("session", session_id))
            })
            .map(|group| group.group_id.clone())
    });
    let selection =
        (selected_session.is_some() || selected_thread.is_some()).then(|| v2::Selection {
            selection_id: "v1:selection".to_string(),
            session_id: selected_session.map(|id| encode_id("session", id)),
            group_id: selected_group,
            thread_id: selected_thread.map(|id| encode_id("thread", id)),
            ..Default::default()
        });
    let execution_states = threads
        .iter()
        .filter(|thread| {
            !matches!(
                v2::ThreadState::try_from(thread.state),
                Ok(v2::ThreadState::Unavailable | v2::ThreadState::Exited)
            )
        })
        .map(|thread| v2::ExecutionState {
            execution_state_id: format!("v1:execution-state:{}", thread.thread_id),
            target: Some(v2::Target {
                selector: Some(target::Selector::Thread(v2::ThreadTarget {
                    thread_id: thread.thread_id.clone(),
                })),
            }),
            running: thread.state == v2::ThreadState::Running as i32,
            stop_reason: None,
            location: thread.location.clone(),
            revision: thread.revision,
        })
        .collect();

    Ok(v2::Snapshot {
        server_instance_id: "v1:untracked-server-instance".to_string(),
        included_sections: vec![
            v2::SnapshotSection::Topology as i32,
            v2::SnapshotSection::Selection as i32,
            v2::SnapshotSection::Execution as i32,
            v2::SnapshotSection::Breakpoints as i32,
            v2::SnapshotSection::PendingOperations as i32,
            v2::SnapshotSection::Extensions as i32,
        ],
        sessions,
        groups,
        processes,
        threads,
        selection,
        execution_states,
        breakpoints,
        pending_commands,
        extension_states: legacy_extension_states(data.get("extensions"))?,
        ..Default::default()
    })
}

fn legacy_session(value: &Value) -> Result<v2::Session> {
    let sid = required_u64(value, "sid")?;
    let tag = required_str(value, "tag")?;
    let alias = required_str(value, "alias")?;
    let status_text = required_str(value, "status")?;
    let all_threads_stopped = value
        .get("all_threads_stopped")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = match status_text {
        "OFF" => v2::SessionStatus::Exited,
        "ON" if all_threads_stopped => v2::SessionStatus::Stopped,
        "ON" => v2::SessionStatus::Running,
        _ => v2::SessionStatus::Failed,
    };
    let group_id = value
        .get("group")
        .filter(|group| group.get("valid").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|group| group.get("id"))
        .and_then(json_u64)
        .map(|id| encode_id("group", id));
    let selected_thread_id = value
        .get("selected_thread_id")
        .and_then(json_u64)
        .map(|id| encode_id("thread", id));
    let status_detail = value
        .get("in_custom_context")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| "ddb custom distributed context is active".to_string());

    Ok(v2::Session {
        session_id: encode_id("session", sid),
        display_name: if alias == "UNKNOWN" {
            tag.to_string()
        } else {
            format!("{alias} ({tag})")
        },
        backend: Some(v2::BackendDescriptor {
            kind: v2::BackendKind::Other as i32,
            version: None,
            capability_namespace: Some("ddb.api.v1".to_string()),
        }),
        status: status as i32,
        status_detail,
        process_id: None,
        group_id,
        selected_thread_id,
        ..Default::default()
    })
}

fn legacy_group(value: &Value, selected_session: Option<u64>) -> Result<v2::Group> {
    let id = required_u64(value, "id")?;
    let session_ids = required_array(value, "sids")?
        .iter()
        .map(|sid| {
            json_u64(sid)
                .map(|sid| encode_id("session", sid))
                .context("DDB v1 group contains a non-numeric session ID")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(v2::Group {
        group_id: encode_id("group", id),
        display_name: optional_string(value, &["alias", "hash"])
            .unwrap_or_else(|| format!("group {id}")),
        selected: selected_session
            .is_some_and(|selected| session_ids.contains(&encode_id("session", selected))),
        session_ids,
        ..Default::default()
    })
}

fn legacy_process(value: &Value) -> Result<v2::Process> {
    let id = required_u64(value, "global_id")?;
    Ok(v2::Process {
        process_id: encode_id("process", id),
        session_id: encode_id("session", required_u64(value, "session_id")?),
        group_id: value
            .get("group_id")
            .and_then(json_u64)
            .map(|id| encode_id("group", id)),
        name: None,
        system_process_id: value
            .get("system_process_id")
            .and_then(json_u64)
            .map(|id| id.to_string()),
        executable: None,
        ..Default::default()
    })
}

fn legacy_thread(value: &Value) -> Result<v2::Thread> {
    let id = required_u64(value, "global_id")?;
    let state = match required_str(value, "status")? {
        "running" => v2::ThreadState::Running,
        "stopped" => v2::ThreadState::Stopped,
        "exited" => v2::ThreadState::Exited,
        _ => v2::ThreadState::Unavailable,
    };
    Ok(v2::Thread {
        thread_id: encode_id("thread", id),
        session_id: encode_id("session", required_u64(value, "session_id")?),
        process_id: value
            .get("process_id")
            .and_then(json_u64)
            .map(|id| encode_id("process", id)),
        group_id: value
            .get("group_id")
            .and_then(json_u64)
            .map(|id| encode_id("group", id)),
        name: None,
        backend_thread_id: optional_string(value, &["backend_thread_id"]),
        state: state as i32,
        selected: value
            .get("selected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        location: value.get("location").and_then(legacy_location),
        revision: value
            .get("execution_revision")
            .and_then(json_u64)
            .unwrap_or_default(),
    })
}

fn legacy_breakpoint(value: &Value) -> Result<v2::Breakpoint> {
    let id = required_u64(value, "id")?;
    let location = value
        .get("location")
        .context("DDB v1 breakpoint omitted location")?;
    let source = required_str(location, "src")?.to_string();
    let line = u32::try_from(required_u64(location, "line")?)
        .context("DDB v1 breakpoint line exceeds u32")?;
    let sub_values = required_array(value, "subbkpts")?;
    let active = sub_values
        .iter()
        .any(|sub| match sub.get("type").and_then(Value::as_str) {
            Some("session") => true,
            Some("group") => sub
                .get("active_sessions")
                .and_then(json_u64)
                .is_some_and(|count| count > 0),
            _ => false,
        });
    let sub_breakpoints = sub_values
        .iter()
        .filter(|sub| sub.get("type").and_then(Value::as_str) == Some("session"))
        .map(|sub| {
            let sub_id = required_u64(sub, "id")?;
            Ok(v2::SubBreakpoint {
                sub_breakpoint_id: encode_id("sub-breakpoint", sub_id),
                session_id: encode_id("session", required_u64(sub, "target_session")?),
                inherited_from_group_id: None,
                location: Some(v2::SourceLocation {
                    path: Some(source.clone()),
                    line,
                    ..Default::default()
                }),
                verified: true,
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(v2::Breakpoint {
        breakpoint_id: encode_id("breakpoint", id),
        target: legacy_breakpoint_target(sub_values)?,
        spec: Some(v2::BreakpointSpec {
            location: Some(breakpoint_spec::Location::Source(
                v2::SourceBreakpointLocation {
                    source,
                    line,
                    ..Default::default()
                },
            )),
            enabled: Some(
                value
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
            condition: optional_string(value, &["condition"]),
            temporary: value
                .get("temporary")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            hardware: value
                .get("hardware")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Default::default()
        }),
        verified: active,
        pending: !active,
        hit_count: value.get("times").and_then(json_u64).unwrap_or_default(),
        message: (!active).then(|| "waiting for a matching v1 session".to_string()),
        sub_breakpoints,
        ..Default::default()
    })
}

fn legacy_breakpoint_target(sub_breakpoints: &[Value]) -> Result<Option<v2::Target>> {
    let Some(first) = sub_breakpoints.first() else {
        return Ok(None);
    };
    let selector = match first.get("type").and_then(Value::as_str) {
        Some("session") => target::Selector::Session(v2::SessionTarget {
            session_id: encode_id("session", required_u64(first, "target_session")?),
        }),
        Some("group") => target::Selector::Group(v2::GroupTarget {
            group_id: encode_id("group", required_u64(first, "target_group")?),
        }),
        Some(kind) => bail!("unsupported DDB v1 sub-breakpoint kind {kind}"),
        None => bail!("DDB v1 sub-breakpoint omitted type"),
    };
    Ok(Some(v2::Target {
        selector: Some(selector),
    }))
}

fn legacy_pending_command(value: &Value) -> Result<v2::PendingCommand> {
    let sid = required_u64(value, "sid")?;
    let token = required_u64(value, "token")?;
    let kind = value
        .get("operation_kind")
        .and_then(json_u64)
        .and_then(|kind| i32::try_from(kind).ok())
        .filter(|kind| OperationKind::try_from(*kind).is_ok())
        .unwrap_or(OperationKind::Unspecified as i32);
    let enqueued_at = value
        .get("enqueued_at")
        .and_then(Value::as_object)
        .and_then(|time| {
            let seconds = time.get("secs_since_epoch").and_then(json_u64)?;
            let nanos = time
                .get("nanos_since_epoch")
                .and_then(json_u64)
                .unwrap_or_default();
            Some(ddb_api_client::wkt::Timestamp {
                seconds: i64::try_from(seconds).ok()?,
                nanos: i32::try_from(nanos).ok()?,
            })
        });
    Ok(v2::PendingCommand {
        pending_command_id: format!("v1:pending-command:{sid}:{token}"),
        session_id: encode_id("session", sid),
        operation_id: optional_string(value, &["operation_id"]),
        kind,
        enqueued_at,
        running: value
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    })
}

fn legacy_extension_descriptors(value: Option<&Value>) -> Result<Vec<v2::ExtensionDescriptor>> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|extension| {
            let id = required_str(extension, "id")?.to_string();
            let presentations = required_array(extension, "panels")?
                .iter()
                .map(|panel| {
                    let panel_id = required_str(panel, "id")?.to_string();
                    let columns = required_array(panel, "columns")?
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            Ok(v2::ExtensionColumnDescriptor {
                                id: format!("column-{index}"),
                                title: column
                                    .as_str()
                                    .context("DDB v1 extension column is not a string")?
                                    .to_string(),
                                value_type: None,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(v2::ExtensionPresentationDescriptor {
                        id: panel_id,
                        title: required_str(panel, "title")?.to_string(),
                        description: None,
                        kind: v2::ExtensionPresentationKind::Table as i32,
                        columns,
                        action_id: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(v2::ExtensionDescriptor {
                extension_id: id.clone(),
                owner: "ddb-v1-framework".to_string(),
                version: "1".to_string(),
                title: required_str(extension, "title")?.to_string(),
                description: required_str(extension, "description")?.to_string(),
                schema_uri: format!("urn:ddb:v1:extension:{id}"),
                schema_hash: "unavailable-v1".to_string(),
                presentations,
                minimum_api_version: Some("v1".to_string()),
                maximum_api_version: Some("v1".to_string()),
                ..Default::default()
            })
        })
        .collect()
}

fn legacy_extension_states(value: Option<&Value>) -> Result<Vec<v2::ExtensionState>> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|extension| {
            let id = required_str(extension, "id")?.to_string();
            Ok(v2::ExtensionState {
                extension_state_id: format!("v1:extension-state:{id}"),
                extension_id: id.clone(),
                revision: 0,
                payloads: vec![v2::ExtensionPayload {
                    extension_id: id.clone(),
                    schema_version: "1".to_string(),
                    schema_uri: format!("urn:ddb:v1:extension:{id}"),
                    media_type: "application/json".to_string(),
                    payload: Some(extension_payload::Payload::PayloadJson(
                        serde_json::to_string(extension)?,
                    )),
                }],
            })
        })
        .collect()
}

fn legacy_location(value: &Value) -> Option<v2::SourceLocation> {
    let object = value.as_object()?;
    let line = object
        .get("line")
        .and_then(json_u64)
        .and_then(|line| u32::try_from(line).ok())
        .unwrap_or_default();
    let column = object
        .get("column")
        .and_then(json_u64)
        .and_then(|column| u32::try_from(column).ok())
        .unwrap_or_default();
    let location = v2::SourceLocation {
        path: optional_string(value, &["path"]),
        line,
        column,
        address: optional_string(value, &["address"]),
        function_name: optional_string(value, &["function_name"]),
        ..Default::default()
    };
    (location.path.is_some()
        || location.line != 0
        || location.address.is_some()
        || location.function_name.is_some())
    .then_some(location)
}

fn memory_block(data: &Value) -> Result<v2::MemoryBlock> {
    let blocks = command_payloads(data)
        .into_iter()
        .flat_map(|payload| {
            payload
                .get("memory")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect::<Vec<_>>();
    let first = blocks
        .first()
        .context("DDB v1 memory response contained no memory blocks")?;
    let address = optional_string(first, &["begin", "address"])
        .context("DDB v1 memory block omitted its address")?;
    let mut bytes = Vec::new();
    for block in blocks {
        let contents = required_str(block, "contents")?;
        if contents.len() % 2 != 0 {
            bail!("DDB v1 memory block contained an odd-length hexadecimal value");
        }
        for pair in contents.as_bytes().chunks_exact(2) {
            let pair = std::str::from_utf8(pair)
                .context("DDB v1 memory block was not valid hexadecimal text")?;
            bytes.push(
                u8::from_str_radix(pair, 16)
                    .context("DDB v1 memory block contained invalid hexadecimal data")?,
            );
        }
    }
    Ok(v2::MemoryBlock {
        address,
        data: bytes,
        unreadable_bytes: 0,
    })
}

fn command_payloads(data: &Value) -> Vec<&Value> {
    data.pointer("/result/responses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|response| response.get("payload"))
        .collect()
}

fn completed_operation(kind: OperationKind, result: Option<v2::OperationResult>) -> v2::Operation {
    let id = NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
    v2::Operation {
        operation_id: format!("v1:operation:{id}"),
        request_id: format!("v1:request:{id}"),
        kind: kind as i32,
        state: v2::OperationState::Completed as i32,
        result: Some(result.unwrap_or(v2::OperationResult {
            value: Some(operation_result::Value::NoContent(v2::Empty {})),
        })),
        cancellable: false,
        ..Default::default()
    }
}

fn target_json(target: &v2::Target) -> Result<Value> {
    let selector = target
        .selector
        .as_ref()
        .context("debugger target omitted its selector")?;
    Ok(match selector {
        target::Selector::Session(target) => {
            json!({"kind": "session", "session_id": decode_id(&target.session_id, "session")?})
        }
        target::Selector::Thread(target) => {
            json!({"kind": "thread", "thread_id": decode_id(&target.thread_id, "thread")?})
        }
        target::Selector::Group(target) => {
            json!({"kind": "group", "group_id": decode_id(&target.group_id, "group")?})
        }
        target::Selector::CurrentThread(_) => json!({"kind": "current_thread"}),
        target::Selector::CurrentSession(_) => json!({"kind": "current_session"}),
        target::Selector::SessionSet(target) => {
            let session_ids = target
                .session_ids
                .iter()
                .map(|id| decode_id(id, "session"))
                .collect::<Result<Vec<_>>>()?;
            if session_ids.is_empty() {
                bail!("v1 session-set target cannot be empty");
            }
            json!({"kind": "session_set", "session_ids": session_ids})
        }
        target::Selector::Broadcast(_) => json!({"kind": "broadcast"}),
        target::Selector::First(_) => json!({"kind": "first"}),
        target::Selector::Multiple(target) => {
            let targets = target
                .targets
                .iter()
                .map(target_json)
                .collect::<Result<Vec<_>>>()?;
            if targets.is_empty() {
                bail!("v1 multiple target cannot be empty");
            }
            json!({"kind": "multiple", "targets": targets})
        }
        target::Selector::Operation(_) => {
            bail!("DDB API v1 cannot address a v2 operation target")
        }
    })
}

fn breakpoint_target_json(target: BreakpointTarget) -> Result<Value> {
    Ok(match target {
        BreakpointTarget::Session(id) => {
            json!({"kind": "session", "session_id": decode_id(&id, "session")?})
        }
        BreakpointTarget::Group(id) => {
            json!({"kind": "group", "group_id": decode_id(&id, "group")?})
        }
        BreakpointTarget::Broadcast => json!({"kind": "broadcast"}),
        BreakpointTarget::Multiple(targets) => {
            let targets = targets
                .into_iter()
                .map(breakpoint_target_json)
                .collect::<Result<Vec<_>>>()?;
            if targets.is_empty() {
                bail!("v1 multiple breakpoint target cannot be empty");
            }
            json!({"kind": "multiple", "targets": targets})
        }
    })
}

fn thread_target_json(thread_id: &str) -> Result<Value> {
    Ok(json!({
        "kind": "thread",
        "thread_id": decode_id(thread_id, "thread")?,
    }))
}

fn encode_id(kind: &str, id: u64) -> String {
    format!("v1:{kind}:{id}")
}

fn decode_id(value: &str, kind: &str) -> Result<u64> {
    let prefix = format!("v1:{kind}:");
    let encoded = value
        .strip_prefix(&prefix)
        .with_context(|| format!("expected a namespaced v1 {kind} ID, got {value:?}"))?;
    if encoded.is_empty() || encoded.contains(':') {
        bail!("invalid namespaced v1 {kind} ID {value:?}");
    }
    encoded
        .parse()
        .with_context(|| format!("invalid numeric component in v1 {kind} ID {value:?}"))
}

fn encode_frame_id(thread_id: u64, level: u64) -> String {
    format!("v1:frame:{thread_id}:{level}")
}

fn decode_frame_id(value: &str) -> Result<(u64, u64)> {
    let encoded = value
        .strip_prefix("v1:frame:")
        .with_context(|| format!("expected a namespaced v1 frame ID, got {value:?}"))?;
    let (thread_id, level) = encoded
        .split_once(':')
        .with_context(|| format!("invalid namespaced v1 frame ID {value:?}"))?;
    if thread_id.is_empty() || level.is_empty() || level.contains(':') {
        bail!("invalid namespaced v1 frame ID {value:?}");
    }
    Ok((
        thread_id
            .parse()
            .with_context(|| format!("invalid thread component in v1 frame ID {value:?}"))?,
        level
            .parse()
            .with_context(|| format!("invalid level component in v1 frame ID {value:?}"))?,
    ))
}

fn required_array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("DDB v1 response omitted array field {field}"))
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("DDB v1 response omitted string field {field}"))
}

fn required_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(json_u64)
        .with_context(|| format!("DDB v1 response omitted numeric field {field}"))
}

fn string_u64(value: &Value, field: &str) -> Result<u64> {
    required_u64(value, field)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn optional_string(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        let value = value.get(*field)?;
        match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    })
}

fn string_array(value: &Value, field: &str) -> Result<Vec<String>> {
    required_array(value, field)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .with_context(|| format!("DDB v1 {field} entry is not a string"))
        })
        .collect()
}

fn safe_url(url: &Url) -> String {
    let mut safe = url.clone();
    safe.set_query(None);
    safe.set_fragment(None);
    let _ = safe.set_username("");
    let _ = safe.set_password(None);
    safe.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_ids_are_namespaced_and_never_inferred_from_v2_ids() {
        assert_eq!(decode_id(&encode_id("thread", 42), "thread").unwrap(), 42);
        assert!(decode_id("thr_opaque", "thread").is_err());
        assert!(decode_id("v1:session:42", "thread").is_err());
        assert!(decode_id("v1:thread:42:7", "thread").is_err());

        let frame = encode_frame_id(42, 3);
        assert_eq!(decode_frame_id(&frame).unwrap(), (42, 3));
        assert!(decode_frame_id("frm_opaque").is_err());
        assert!(decode_frame_id("v1:frame:42:3:extra").is_err());
    }

    #[test]
    fn memory_hex_decoder_preserves_zero_bytes() {
        let data = json!({
            "result": {
                "responses": [{
                    "payload": {
                        "memory": [{
                            "begin": "0x10",
                            "contents": "2a000000ff"
                        }]
                    }
                }]
            }
        });
        let block = memory_block(&data).unwrap();
        assert_eq!(block.address, "0x10");
        assert_eq!(block.data, vec![0x2a, 0, 0, 0, 0xff]);
    }

    #[test]
    fn legacy_snapshot_projects_namespaced_topology_and_location() {
        let snapshot = legacy_snapshot(json!({
            "selected_session_id": 7,
            "selected_thread_id": 41,
            "sessions": [{
                "sid": 7,
                "tag": "worker",
                "alias": "service-a",
                "status": "ON",
                "group": {"valid": true, "id": 9, "hash": "worker"},
                "selected_thread_id": 41,
                "in_custom_context": false,
                "all_threads_stopped": true
            }],
            "groups": [{
                "id": 9,
                "hash": "worker",
                "alias": "service-a",
                "sids": [7]
            }],
            "processes": [{
                "global_id": 70,
                "session_id": 7,
                "group_id": 9,
                "system_process_id": 1234
            }],
            "threads": [{
                "global_id": 41,
                "process_id": 70,
                "session_id": 7,
                "group_id": 9,
                "backend_thread_id": "1",
                "status": "stopped",
                "selected": true,
                "execution_revision": 5,
                "location": {
                    "path": "src/main.rs",
                    "line": 12,
                    "address": "0x1234",
                    "function_name": "main"
                }
            }],
            "breakpoints": [],
            "pending_commands": [],
            "pending_command_details": [],
            "extensions": []
        }))
        .unwrap();

        assert_eq!(snapshot.sessions[0].session_id, "v1:session:7");
        assert_eq!(snapshot.groups[0].group_id, "v1:group:9");
        assert!(snapshot.groups[0].selected);
        assert_eq!(snapshot.processes[0].process_id, "v1:process:70");
        assert_eq!(snapshot.threads[0].thread_id, "v1:thread:41");
        assert_eq!(
            snapshot.threads[0]
                .location
                .as_ref()
                .and_then(|location| location.path.as_deref()),
            Some("src/main.rs")
        );
        assert_eq!(
            snapshot
                .selection
                .as_ref()
                .and_then(|selection| selection.group_id.as_deref()),
            Some("v1:group:9")
        );
    }
}
