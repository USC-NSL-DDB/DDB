use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use gdbmi::raw::{Dict, Value};
use tracing::{debug, error};

use crate::{
    common::Config,
    feature::proclet_restore::ProcletRestorationMgr,
    state::{GlobalThreadId, LocalThreadId, RuntimeModel, ThreadContext},
};

use super::{
    api::CommandExecutor,
    framework_adapter::FrameworkCommandAdapter,
    input::ParsedInputCmd,
    router::Target,
    transaction::{SessionTransaction, TransactionCoordinator},
    CommandOutcome, FinishedCmd, Presentation,
};

struct BacktraceData {
    completion: FinishedCmd,
    parent: Option<ParentMetadata>,
}

struct ParentCapture {
    session_id: u64,
    thread_id: GlobalThreadId,
    data: BacktraceData,
}

#[derive(Debug)]
struct ParentMetadata {
    message: String,
    caller_context: Dict,
    id: String,
    proclet_id: String,
}

pub(crate) struct DistributedBacktraceService {
    adapter: Arc<dyn FrameworkCommandAdapter>,
    model: Arc<RuntimeModel>,
    config: Arc<Config>,
    executor: CommandExecutor,
    transactions: TransactionCoordinator,
    proclet_restoration: Arc<ProcletRestorationMgr>,
}

impl DistributedBacktraceService {
    pub(crate) fn new(
        adapter: Arc<dyn FrameworkCommandAdapter>,
        model: Arc<RuntimeModel>,
        config: Arc<Config>,
        executor: CommandExecutor,
        transactions: TransactionCoordinator,
        proclet_restoration: Arc<ProcletRestorationMgr>,
    ) -> Self {
        Self {
            adapter,
            model,
            config,
            executor,
            transactions,
            proclet_restoration,
        }
    }

    pub(crate) async fn execute(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let Target::Thread(initial_thread_id) = command.target else {
            bail!("bt-remote requires a thread target");
        };

        let BacktraceData {
            mut completion,
            mut parent,
        } = self
            .capture_thread(initial_thread_id)
            .await
            .with_context(|| {
                format!(
                    "Failed to get backtrace for thread {}, break the call chain",
                    initial_thread_id
                )
            })?;
        if let Some(external_token) = command.external_token {
            completion.set_external_token(external_token);
        }

        while parent
            .as_ref()
            .is_some_and(|metadata| metadata.message == "success")
        {
            let Some(metadata) = parent.take() else {
                break;
            };
            let capture = match self.capture_parent(&metadata).await {
                Ok(capture) => capture,
                Err(error) => {
                    error!(?error, "failed to capture distributed parent");
                    break;
                }
            };
            parent = capture.data.parent;
            if let Err(error) = append_parent_frames(
                &mut completion,
                capture.session_id,
                capture.thread_id,
                capture.data.completion,
            ) {
                error!(?error, "failed to append distributed parent frames");
                break;
            }
        }

        add_reordered_frame_levels(&mut completion)?;
        Ok(CommandOutcome::response(completion, Presentation::Plain))
    }

    async fn capture_thread(&self, global_thread_id: GlobalThreadId) -> Result<BacktraceData> {
        let LocalThreadId(session_id, _) = self
            .model
            .local_thread_id(global_thread_id)
            .ok_or_else(|| anyhow!("Unknown global thread {}", global_thread_id))?;
        let transaction = self
            .transactions
            .begin(session_id)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        self.capture_thread_locked(&transaction, session_id, global_thread_id)
            .await
    }

    async fn capture_thread_locked(
        &self,
        transaction: &SessionTransaction,
        session_id: u64,
        global_thread_id: GlobalThreadId,
    ) -> Result<BacktraceData> {
        let mut completion = self
            .executor
            .execute_exclusive(
                &format!("-stack-list-frames --thread {}", global_thread_id),
                Target::Thread(global_thread_id),
                transaction.lease(),
            )
            .await?;

        for frame in stack_mut(&mut completion)? {
            let frame = frame
                .expect_dict_ref_mut()
                .map_err(|_| anyhow!("stack frame must be a dictionary"))?;
            frame.insert("session".to_string(), session_id.to_string().into());
            frame.insert("thread".to_string(), global_thread_id.to_string().into());
        }

        let metadata_command = self.adapter.get_bt_command_name();
        let parent = match self
            .executor
            .execute_exclusive(
                &metadata_command,
                Target::Thread(global_thread_id),
                transaction.lease(),
            )
            .await
        {
            Ok(response) => match first_payload(&response)
                .and_then(|payload| extract_remote_metadata(self.adapter.as_ref(), payload))
            {
                Ok(metadata) => Some(metadata),
                Err(error) => {
                    debug!(?error, "distributed parent metadata is unavailable");
                    None
                }
            },
            Err(error) => {
                debug!(?error, "distributed parent metadata command failed");
                None
            }
        };

        Ok(BacktraceData { completion, parent })
    }

    async fn capture_parent(&self, parent: &ParentMetadata) -> Result<ParentCapture> {
        let session_id = self
            .model
            .session_id_by_tag(&parent.id)
            .await
            .ok_or_else(|| anyhow!("No session matches distributed parent {}", parent.id))?;
        let in_custom_context = self
            .model
            .session_snapshot(session_id)
            .await
            .ok_or_else(|| anyhow!("Session {} disappeared", session_id))?
            .in_custom_context;
        let global_thread_id = self
            .model
            .global_thread_ids_for_session(session_id)
            .first()
            .copied()
            .ok_or_else(|| anyhow!("Session {} has no threads", session_id))?;

        let data = if in_custom_context {
            self.capture_thread(global_thread_id).await?
        } else {
            self.capture_parent_with_context(session_id, global_thread_id, parent)
                .await?
        };

        Ok(ParentCapture {
            session_id,
            thread_id: global_thread_id,
            data,
        })
    }

    async fn capture_parent_with_context(
        &self,
        session_id: u64,
        global_thread_id: GlobalThreadId,
        parent: &ParentMetadata,
    ) -> Result<BacktraceData> {
        debug!(session_id, "switching to distributed parent context");
        let related_session = if self.config.handle_migration()
            && !parent.proclet_id.is_empty()
            && parent.proclet_id != "0"
        {
            self.proclet_restoration
                .related_session(session_id, &parent.proclet_id)
                .await?
        } else {
            None
        };
        let transaction = self
            .transactions
            .begin_with_related(session_id, related_session)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;

        self.executor
            .execute_exclusive(
                "-exec-interrupt",
                Target::Session(session_id),
                transaction.lease(),
            )
            .await
            .with_context(|| format!("Failed to interrupt session {}", session_id))?;
        wait_for_all_threads_stopped(&transaction).await?;

        let switch = self
            .executor
            .execute_exclusive(
                &format!(
                    "-switch-context-custom {}",
                    prepare_context_switch_args(&parent.caller_context)
                ),
                Target::Thread(global_thread_id),
                transaction.lease(),
            )
            .await?;
        let switch_payload = first_payload(&switch)?;
        if required_string(switch_payload, "message")? != "success" {
            bail!(
                "Failed to switch context for session {}; the call stack may be incomplete",
                session_id
            );
        }
        let context = extract_context(switch_payload, global_thread_id)?;
        if !transaction.enter_custom_context(context).await {
            bail!("Session {} disappeared during context switch", session_id);
        }

        self.handle_migration(global_thread_id, parent, Some(&transaction))
            .await;
        self.capture_thread_locked(&transaction, session_id, global_thread_id)
            .await
    }

    async fn handle_migration(
        &self,
        global_thread_id: GlobalThreadId,
        parent: &ParentMetadata,
        transaction: Option<&SessionTransaction>,
    ) {
        if !self.config.handle_migration() {
            return;
        }
        let Some(LocalThreadId(session_id, _)) = self.model.local_thread_id(global_thread_id)
        else {
            error!(
                global_thread_id = %global_thread_id,
                "unable to resolve session for proclet restoration"
            );
            return;
        };
        match self
            .proclet_restoration
            .handle_proclet_restoration(session_id, &parent.proclet_id, transaction)
            .await
        {
            Ok(_) => debug!(session_id, "proclet heap restoration completed"),
            Err(error) => error!(?error, session_id, "proclet heap restoration failed"),
        }
    }
}

impl fmt::Debug for DistributedBacktraceService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DistributedBacktraceService")
            .finish()
    }
}

async fn wait_for_all_threads_stopped(transaction: &SessionTransaction) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if transaction.all_threads_stopped().await == Some(true) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for interrupt before context switch");
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn extract_remote_metadata(
    adapter: &dyn FrameworkCommandAdapter,
    payload: &Dict,
) -> Result<ParentMetadata> {
    let metadata = required_dict(payload, "metadata")?;
    let caller_metadata_value = metadata
        .get("caller_meta")
        .ok_or_else(|| anyhow!("missing 'caller_meta' field"))?;
    let caller_metadata = caller_metadata_value
        .expect_dict_ref()
        .map_err(|_| anyhow!("'caller_meta' must be a dictionary"))?;
    let caller_context = required_dict(metadata, "caller_ctx")?.clone();
    let message = required_string(payload, "message")?.to_string();
    let id = adapter.extract_id_from_metadata(caller_metadata_value)?;
    let proclet_id = caller_metadata
        .get("proclet_id")
        .and_then(|value| value.expect_string_ref().ok())
        .unwrap_or_default()
        .to_string();

    Ok(ParentMetadata {
        message,
        caller_context,
        id,
        proclet_id,
    })
}

fn extract_context(payload: &Dict, global_thread_id: GlobalThreadId) -> Result<ThreadContext> {
    let context = required_dict(payload, "old_ctx")?
        .as_map()
        .iter()
        .map(|(register, value)| {
            value
                .expect_string_repr::<u64>()
                .map(|value| (register.clone(), value))
                .map_err(|_| anyhow!("register {} must contain an integer", register))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(ThreadContext {
        ctx: context,
        tid: global_thread_id,
    })
}

fn prepare_context_switch_args(registers: &Dict) -> String {
    registers
        .as_map()
        .iter()
        .filter_map(|(register, value)| {
            value
                .expect_string_ref()
                .ok()
                .map(|value| format!("{}={}", register, value))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn append_parent_frames(
    completion: &mut FinishedCmd,
    session_id: u64,
    global_thread_id: GlobalThreadId,
    parent_completion: FinishedCmd,
) -> Result<()> {
    let frames = take_stack(parent_completion)?;
    let boundary_frame: Value = HashMap::from([
        ("line".to_string(), "0".into()),
        ("level".to_string(), "-1".into()),
        ("func".to_string(), "<boundary>".into()),
        ("addr".to_string(), "0xDEADBEEF".into()),
        ("file".to_string(), "???".into()),
        ("arch".to_string(), "???".into()),
        ("session".to_string(), session_id.to_string().into()),
        ("thread".to_string(), global_thread_id.to_string().into()),
        ("boundary_frame".to_string(), "1".into()),
    ])
    .into();
    let stack = stack_mut(completion)?;
    stack.push(boundary_frame);
    stack.extend(frames);
    Ok(())
}

fn add_reordered_frame_levels(completion: &mut FinishedCmd) -> Result<()> {
    for (index, frame) in stack_mut(completion)?.iter_mut().enumerate() {
        let frame = frame
            .expect_dict_ref_mut()
            .map_err(|_| anyhow!("stack frame must be a dictionary"))?;
        frame.insert("level_reordered".to_string(), index.to_string().into());
    }
    Ok(())
}

fn stack_mut(completion: &mut FinishedCmd) -> Result<&mut Vec<Value>> {
    completion
        .get_responses_mut()
        .first_mut()
        .ok_or_else(|| anyhow!("backtrace response is missing"))?
        .get_payload_mut()
        .ok_or_else(|| anyhow!("backtrace response payload is missing"))?
        .get_mut("stack")
        .ok_or_else(|| anyhow!("backtrace response has no stack"))?
        .expect_list_ref_mut()
        .map_err(|_| anyhow!("backtrace stack must be a list"))
}

fn take_stack(mut completion: FinishedCmd) -> Result<Vec<Value>> {
    completion
        .get_responses_mut()
        .first_mut()
        .ok_or_else(|| anyhow!("backtrace response is missing"))?
        .get_payload_mut()
        .ok_or_else(|| anyhow!("backtrace response payload is missing"))?
        .remove("stack")
        .ok_or_else(|| anyhow!("backtrace response has no stack"))?
        .expect_list()
        .map_err(|_| anyhow!("backtrace stack must be a list"))
}

fn first_payload(completion: &FinishedCmd) -> Result<&Dict> {
    completion
        .get_responses()
        .first()
        .ok_or_else(|| anyhow!("debugger response is missing"))?
        .get_payload()
        .ok_or_else(|| anyhow!("debugger response payload is missing"))
}

fn required_dict<'a>(payload: &'a Dict, field: &str) -> Result<&'a Dict> {
    payload
        .get(field)
        .ok_or_else(|| anyhow!("missing '{}' field", field))?
        .expect_dict_ref()
        .map_err(|_| anyhow!("'{}' must be a dictionary", field))
}

fn required_string<'a>(payload: &'a Dict, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .ok_or_else(|| anyhow!("missing '{}' field", field))?
        .expect_string_ref()
        .map_err(|_| anyhow!("'{}' must be a string", field))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::cmd_flow::framework_adapter::GrpcAdapter;

    #[test]
    fn extracts_typed_grpc_parent_metadata() {
        let payload: Dict = HashMap::from([
            ("message".to_string(), Value::from("success")),
            (
                "metadata".to_string(),
                Value::from(Dict::from(HashMap::from([
                    (
                        "caller_meta".to_string(),
                        Value::from(Dict::from(HashMap::from([
                            (
                                "ip".to_string(),
                                Value::from(u32::from(Ipv4Addr::new(127, 0, 0, 1)).to_string()),
                            ),
                            ("pid".to_string(), Value::from("42")),
                            ("tid".to_string(), Value::from("7")),
                            ("proclet_id".to_string(), Value::from("0")),
                        ]))),
                    ),
                    (
                        "caller_ctx".to_string(),
                        Value::from(Dict::from(HashMap::from([
                            ("pc".to_string(), Value::from("4096")),
                            ("sp".to_string(), Value::from("8192")),
                            ("fp".to_string(), Value::from("12288")),
                        ]))),
                    ),
                ]))),
            ),
        ])
        .into();

        let metadata = extract_remote_metadata(&GrpcAdapter, &payload).unwrap();

        assert_eq!(metadata.message, "success");
        assert_eq!(metadata.id, "127.0.0.1:-42");
        assert_eq!(metadata.proclet_id, "0");
        assert_eq!(
            metadata.caller_context["pc"].expect_string_ref().unwrap(),
            "4096"
        );
    }

    #[test]
    fn malformed_context_is_reported_instead_of_panicking() {
        let payload: Dict = HashMap::from([(
            "old_ctx".to_string(),
            Value::from(Dict::from(HashMap::from([(
                "pc".to_string(),
                Value::from("not-an-integer"),
            )]))),
        )])
        .into();

        assert!(extract_context(&payload, GlobalThreadId::new(7)).is_err());
    }

    #[test]
    fn context_switch_arguments_only_include_scalar_registers() {
        let registers: Dict = HashMap::from([
            ("pc".to_string(), Value::from("4096")),
            ("sp".to_string(), Value::from("8192")),
            (
                "ignored".to_string(),
                Value::from(Dict::from(HashMap::<String, Value>::new())),
            ),
        ])
        .into();

        let arguments = prepare_context_switch_args(&registers);

        assert!(arguments.contains("pc=4096"));
        assert!(arguments.contains("sp=8192"));
        assert!(!arguments.contains("ignored"));
    }
}
