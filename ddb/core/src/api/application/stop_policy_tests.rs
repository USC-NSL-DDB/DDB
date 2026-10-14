#[derive(Default)]
struct PolicyTestPort {
    calls: std::sync::Mutex<Vec<(String, CommandTarget)>>,
    fail_pause: AtomicBool,
    block_continue: AtomicBool,
    continue_queued: tokio::sync::Notify,
    release_continue: tokio::sync::Notify,
    block_pause: AtomicBool,
    pause_queued: tokio::sync::Notify,
    release_pause: tokio::sync::Notify,
    block_frames: AtomicBool,
    frames_queued: tokio::sync::Notify,
    release_frames: tokio::sync::Notify,
    block_evaluation: AtomicBool,
    evaluation_queued: tokio::sync::Notify,
    release_evaluation: tokio::sync::Notify,
}

#[async_trait]
impl ApplicationCommandPort for PolicyTestPort {
    async fn execute(
        &self,
        command: &str,
        target: CommandTarget,
    ) -> Result<CommandOutcome, CommandPortError> {
        if command.starts_with("-stack-list-frames") && self.block_frames.load(Ordering::SeqCst) {
            self.frames_queued.notify_one();
            self.release_frames.notified().await;
        }
        self.calls
            .lock()
            .unwrap()
            .push((command.into(), target.clone()));
        let sid = match target {
            CommandTarget::Session(sid) => sid,
            CommandTarget::Thread(_) => 42,
            _ => panic!("unexpected target"),
        };
        if command == "-exec-interrupt" && self.fail_pause.load(Ordering::SeqCst) {
            return Err(CommandPortError::test("peer unavailable"));
        }
        let payload = command.starts_with("-stack-list-frames").then(|| {
            vec![(
                "stack".into(),
                Value::List(vec![Value::Dict(
                    vec![
                        ("level".into(), "0".into()),
                        ("addr".into(), "0x10".into()),
                        ("func".into(), "main".into()),
                    ]
                    .into(),
                )]),
            )]
            .into()
        });
        Ok(CommandOutcome::silent(FinishedCmd::new(
            None,
            sid,
            vec![ParsedSessionResponse::new(sid, "done".into(), payload)],
        )))
    }

    async fn execute_guarded(
        &self,
        command: &str,
        target: CommandTarget,
        _operation_id: &str,
        _kind: OperationKind,
        guard: Option<Arc<dyn crate::cmd_flow::input::DispatchGuard>>,
    ) -> Result<CommandOutcome, CommandPortError> {
        if command == "-record-time-and-continue" && self.block_continue.load(Ordering::SeqCst) {
            self.continue_queued.notify_one();
            self.release_continue.notified().await;
        }
        if command == "-exec-interrupt" && self.block_pause.load(Ordering::SeqCst) {
            self.pause_queued.notify_one();
            self.release_pause.notified().await;
        }
        if command.starts_with("-ddb-var-create") && self.block_evaluation.load(Ordering::SeqCst) {
            self.evaluation_queued.notify_one();
            self.release_evaluation.notified().await;
        }
        if let Some(guard) = guard {
            guard
                .check()
                .await
                .map_err(|_| CommandPortError::test("stale stop"))?;
            guard.dispatched();
        }
        self.execute(command, target).await
    }
}

async fn policy_fixture() -> (
    Arc<RuntimeModel>,
    Arc<DdbApplicationService>,
    Arc<PolicyTestPort>,
    u64,
) {
    let model = RuntimeModel::new();
    let mut owner = 0;
    for sid in [42, 43] {
        model.register_session(sid, "worker", None).await;
        model.complete_session_activation(sid, None).await;
        model.register_thread_group(sid, "i1").await.unwrap();
        let id = model
            .register_thread(sid, 11, "i1")
            .await
            .unwrap()
            .thread_id;
        if sid == 42 {
            owner = id.value()
        }
        model
            .update_thread_statuses(sid, &[11], ThreadStatus::RUNNING)
            .await
            .unwrap();
    }
    let port = Arc::new(PolicyTestPort::default());
    let service = service_with_port(model.clone(), port.clone());
    (model, service, port, owner)
}

async fn policy_stop(model: &RuntimeModel, owner: u64, log_message: Option<&str>) {
    policy_stop_in_session(model, 42, owner, log_message).await;
}

async fn policy_stop_in_session(
    model: &RuntimeModel,
    session: u64,
    owner: u64,
    log_message: Option<&str>,
) {
    model
        .update_thread_statuses_with_details(
            session,
            &[11],
            ThreadStatus::STOPPED,
            None,
            Some(crate::state::ThreadStopReason {
                kind: crate::state::ThreadStopKind::Breakpoint,
                signal_name: None,
                breakpoint_id: Some(1),
                thread_id: Some(owner),
                log_message: log_message.map(str::to_owned),
            }),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_ordinary_stop_and_logpoint_in_one_snapshot_are_both_handled() {
    for logpoint_session in [42, 43] {
        let (model, service, port, owner) = policy_fixture().await;
        let peer = model.thread_snapshots_for_sessions(&[43]).await[0]
            .global_id
            .value();
        let (logpoint, ordinary) = if logpoint_session == 42 {
            (owner, peer)
        } else {
            (peer, owner)
        };
        // All fixture operations complete without yielding on this current-thread
        // runtime, so the worker's first snapshot contains both native stops.
        policy_stop_in_session(&model, logpoint_session, logpoint, Some("batched logpoint")).await;
        policy_stop_in_session(
            &model,
            if logpoint_session == 42 { 43 } else { 42 },
            ordinary,
            None,
        )
        .await;
        service
            .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
            .unwrap();
        let policy = wait_policy(&service, |policy| policy.initiating_stop.is_some()).await;
        assert_eq!(
            policy.initiating_stop.unwrap().target,
            Some(service_thread_target(&service, ordinary))
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !port
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(command, _)| command == "-record-time-and-continue")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordinary cause must not invalidate the concurrent logpoint");
    }
}

fn service_thread_target(service: &DdbApplicationService, thread: u64) -> PublicTarget {
    PublicTarget {
        selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
            thread_id: service.ids.encode(ResourceIdKind::Thread, thread).unwrap(),
        })),
    }
}

#[tokio::test]
async fn queued_peer_pause_cannot_interrupt_a_newer_running_generation() {
    let (model, service, port, owner) = policy_fixture().await;
    port.block_pause.store(true, Ordering::SeqCst);
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    policy_stop(&model, owner, None).await;
    tokio::time::timeout(Duration::from_secs(1), port.pause_queued.notified())
        .await
        .unwrap();
    let first_revision = service.stop_policy_snapshot().unwrap().revision;
    model
        .update_thread_statuses(43, &[11], ThreadStatus::STOPPED)
        .await
        .unwrap();
    model
        .update_thread_statuses(43, &[11], ThreadStatus::RUNNING)
        .await
        .unwrap();
    port.block_pause.store(false, Ordering::SeqCst);
    port.release_pause.notify_one();
    let settled = wait_policy(&service, |policy| {
        policy.revision > first_revision
            && policy.pending_targets.is_empty()
            && policy
                .target_outcomes
                .first()
                .is_some_and(|outcome| outcome.succeeded)
    })
    .await;
    assert!(settled.target_outcomes[0].succeeded);
    assert_eq!(
        port.calls.lock().unwrap().as_slice(),
        [("-exec-interrupt".into(), CommandTarget::Session(43))],
        "only the new running generation may reach native dispatch"
    );
    assert!(
        service
            .operations
            .list_with_context()
            .iter()
            .any(
                |entry| entry.operation.kind == OperationKind::Execute as i32
                    && entry.operation.state == OperationState::Failed as i32
            ),
        "the old pause must fail instead of targeting new execution"
    );
}

#[tokio::test]
async fn an_undispatched_peer_pause_does_not_hide_a_manual_pause() {
    let (model, service, port, owner) = policy_fixture().await;
    port.block_pause.store(true, Ordering::SeqCst);
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    policy_stop(&model, owner, None).await;
    tokio::time::timeout(Duration::from_secs(1), port.pause_queued.notified())
        .await
        .unwrap();
    service.retire_stop_automation();
    let peer = model.thread_snapshots_for_sessions(&[43]).await[0]
        .global_id
        .value();
    model
        .update_thread_statuses_with_details(
            43,
            &[11],
            ThreadStatus::STOPPED,
            None,
            Some(crate::state::ThreadStopReason {
                kind: crate::state::ThreadStopKind::Pause,
                signal_name: None,
                breakpoint_id: None,
                thread_id: Some(peer),
                log_message: None,
            }),
        )
        .await
        .unwrap();
    let policy = wait_policy(&service, |policy| policy.initiating_stop.is_some()).await;
    assert_eq!(
        policy.initiating_stop.unwrap().target,
        Some(service_thread_target(&service, peer))
    );
    port.release_pause.notify_one();
    assert!(port.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn logpoint_evaluation_cannot_execute_after_an_intervening_stop() {
    for (queued_evaluation, explicit_pause) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let (model, service, port, owner) = policy_fixture().await;
        if queued_evaluation {
            port.block_evaluation.store(true, Ordering::SeqCst);
        } else {
            port.block_frames.store(true, Ordering::SeqCst);
        }
        policy_stop(&model, owner, Some("side effect={counter++}")).await;
        let queued = if queued_evaluation {
            &port.evaluation_queued
        } else {
            &port.frames_queued
        };
        tokio::time::timeout(Duration::from_secs(1), queued.notified())
            .await
            .unwrap();
        if explicit_pause {
            let request = execute_request(
                "pause-evaluation",
                session_target(service.ids.encode(ResourceIdKind::Session, 42).unwrap()),
                ExecutionAction::Interrupt,
            );
            let operation = service
                .execute(&PrincipalContext::new("second-client").unwrap(), request)
                .await
                .unwrap()
                .operation
                .unwrap();
            terminal_operation(&service, &operation.operation_id).await;
        } else {
            policy_stop(&model, owner, None).await;
        }
        if queued_evaluation {
            port.release_evaluation.notify_one();
        } else {
            port.release_frames.notify_one();
        }
        // The guard failure is surfaced through the operation when evaluation was
        // admitted, or through the stopped frame query before admission.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let done = if queued_evaluation {
                    service.operations.list_with_context().iter().any(|entry| {
                        entry.operation.kind == OperationKind::Evaluate as i32
                            && entry.operation.state == OperationState::Failed as i32
                    })
                } else {
                    port.calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(command, _)| command.starts_with("-stack-list-frames"))
                };
                if done {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !port
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(command, _)| command.starts_with("-ddb-var-create")
                    || command == "-record-time-and-continue"),
            "an old logpoint must not evaluate or resume the newer stop"
        );
    }
}

async fn wait_policy(
    service: &DdbApplicationService,
    condition: impl Fn(&ddb_api_types::v2::StopPolicy) -> bool,
) -> ddb_api_types::v2::StopPolicy {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let policy = service.stop_policy_snapshot().unwrap();
            if condition(&policy) {
                return policy;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("policy did not converge")
}

#[tokio::test]
async fn backend_stop_policy_pauses_peers_once_without_a_frontend_and_replays_its_state() {
    let (model, service, port, owner) = policy_fixture().await;
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    let checkpoint = service.journal.checkpoint().0;
    policy_stop(&model, owner, None).await;
    let policy = wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
    assert!(policy.pending_targets.is_empty());
    assert!(policy.target_outcomes[0].succeeded);
    assert!(policy.initiating_stop.is_some());
    assert_eq!(port.calls.lock().unwrap().len(), 1);
    let generation = policy.generation;
    // A second client applying the same policy cannot create another owner.
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    model.select_local_thread(42, 11).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        service.stop_policy_snapshot().unwrap().generation,
        generation
    );
    assert_eq!(port.calls.lock().unwrap().len(), 1);
    let mut replay = service
        .subscribe_state_events(SubscribeStateEventsRequest {
            after_cursor: Some(checkpoint),
            filter: Some(StateEventFilter {
                resource_kinds: vec![ResourceKind::StopPolicy as i32],
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), replay.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.resource_kind, ResourceKind::StopPolicy as i32);
    let snapshot = service
        .get_snapshot(GetSnapshotRequest {
            sections: vec![SnapshotSection::StopPolicy as i32],
            ..Default::default()
        })
        .await
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(snapshot.stop_policy.unwrap(), policy);
}

#[tokio::test]
async fn failed_peer_pause_remains_visible_and_a_new_cause_retries_it() {
    let (model, service, port, owner) = policy_fixture().await;
    port.fail_pause.store(true, Ordering::SeqCst);
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    policy_stop(&model, owner, None).await;
    let failed = wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
    assert!(!failed.target_outcomes[0].succeeded);
    assert!(failed.target_outcomes[0].error.is_some());
    assert_eq!(
        model.thread_snapshots_for_sessions(&[43]).await[0].status,
        ThreadStatus::RUNNING
    );
    for _ in 0..3 {
        model.select_local_thread(42, 11).await.unwrap();
    }
    tokio::task::yield_now().await;
    assert_eq!(port.calls.lock().unwrap().len(), 1);
    port.fail_pause.store(false, Ordering::SeqCst);
    policy_stop(&model, owner, None).await;
    let recovered = wait_policy(&service, |policy| {
        policy.generation > failed.generation
            && policy
                .target_outcomes
                .first()
                .is_some_and(|outcome| outcome.succeeded)
    })
    .await;
    assert!(recovered.pending_targets.is_empty());
    assert_eq!(port.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn backend_logpoint_outputs_once_to_two_clients_and_continues_once() {
    let (model, service, port, owner) = policy_fixture().await;
    let first = service.output.subscribe(None).unwrap();
    let second = service.output.subscribe(None).unwrap();
    policy_stop(&model, owner, Some("literal {{value}}; $pc = 0")).await;
    let read = |mut subscription: crate::cmd_flow::output_hub::OutputSubscription| async move {
        match tokio::time::timeout(Duration::from_secs(2), subscription.recv())
            .await
            .unwrap()
            .unwrap()
        {
            crate::cmd_flow::output_hub::OutputDelivery::Record(record) => record,
            _ => panic!("unexpected gap"),
        }
    };
    let (a, b) = tokio::join!(read(first), read(second));
    assert_eq!(a.text, "literal {value}; $pc = 0\n");
    assert_eq!(a, b);
    assert_eq!(a.session_id, Some(42));
    tokio::time::timeout(Duration::from_secs(1), async {
        while port.calls.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        port.calls.lock().unwrap().as_slice(),
        [(
            "-record-time-and-continue".into(),
            CommandTarget::Thread(crate::state::GlobalThreadId::new(owner))
        )]
    );
}

#[tokio::test]
async fn pending_backend_logpoint_cannot_resume_an_intervening_stop_or_user_pause() {
    for explicit_pause in [false, true] {
        let (model, service, port, owner) = policy_fixture().await;
        port.block_continue.store(true, Ordering::SeqCst);
        let mut output = service.output.subscribe(None).unwrap();
        policy_stop(&model, owner, Some("old logpoint")).await;
        tokio::time::timeout(Duration::from_secs(2), port.continue_queued.notified())
            .await
            .unwrap();
        if explicit_pause {
            let request = execute_request(
                "client-pause",
                session_target(service.ids.encode(ResourceIdKind::Session, 42).unwrap()),
                ExecutionAction::Interrupt,
            );
            let operation = service
                .execute(&PrincipalContext::new("second-client").unwrap(), request)
                .await
                .unwrap()
                .operation
                .unwrap();
            terminal_operation(&service, &operation.operation_id).await;
        } else {
            policy_stop(&model, owner, None).await;
        }
        port.release_continue.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service.operations.list_with_context().iter().any(|entry| {
                    entry.operation.kind == OperationKind::Execute as i32
                        && entry.operation.state == OperationState::Failed as i32
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(command, _)| command == "-record-time-and-continue"));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), output.recv())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn invalid_or_oversize_backend_logpoints_leave_the_thread_stopped_and_pause_peers() {
    for message in ["missing={unknown}".to_string(), "x".repeat(64 * 1024)] {
        let (model, service, port, owner) = policy_fixture().await;
        service
            .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
            .unwrap();
        policy_stop(&model, owner, Some(&message)).await;
        wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
        assert_eq!(
            model.thread_snapshots_for_sessions(&[42]).await[0].status,
            ThreadStatus::STOPPED
        );
        assert!(!port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(command, _)| command == "-record-time-and-continue"));
        assert!(port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(command, _)| command == "-exec-interrupt"));
    }
}

#[tokio::test]
async fn group_stop_coordinates_a_late_member_and_leaves_other_groups_independent() {
    let (model, service, port, owner) = policy_fixture().await;
    let identity = crate::state::ServiceIdentity::new("shared", "worker");
    drop(model.register_service_group(42, &identity).await);
    drop(
        model
            .register_service_group(43, &crate::state::ServiceIdentity::new("other", "other"))
            .await,
    );
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::GroupStop)
        .unwrap();
    policy_stop(&model, owner, None).await;
    wait_policy(&service, |policy| policy.initiating_stop.is_some()).await;
    assert!(port.calls.lock().unwrap().is_empty());
    model.register_session(44, "late-peer", None).await;
    drop(model.register_service_group(44, &identity).await);
    model.complete_session_activation(44, None).await;
    model.register_thread_group(44, "i1").await.unwrap();
    model.register_thread(44, 11, "i1").await.unwrap();
    model
        .update_thread_statuses(44, &[11], ThreadStatus::RUNNING)
        .await
        .unwrap();
    wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
    assert_eq!(
        port.calls.lock().unwrap().as_slice(),
        [("-exec-interrupt".into(), CommandTarget::Session(44))]
    );
}

#[tokio::test]
async fn two_policy_writers_using_the_same_version_cannot_overwrite_each_other() {
    let (_, service, _, _) = policy_fixture().await;
    let policy = service.stop_policy_snapshot().unwrap();
    let expected = ddb_api_types::v2::ResourceVersion {
        resource_id: policy.stop_policy_id,
        revision: policy.revision,
    };
    service
        .configure_stop_policy_checked(ddb_api_types::v2::StopMode::GroupStop, Some(&expected))
        .unwrap();
    let error = service
        .configure_stop_policy_checked(ddb_api_types::v2::StopMode::AllStop, Some(&expected))
        .unwrap_err();
    assert_eq!(error.code(), DdbErrorCode::FailedPrecondition);
    assert_eq!(
        service.stop_policy_snapshot().unwrap().mode,
        ddb_api_types::v2::StopMode::GroupStop as i32
    );
}

#[tokio::test]
async fn a_peer_that_runs_again_is_paused_again_for_the_same_cause() {
    let (model, service, port, owner) = policy_fixture().await;
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    policy_stop(&model, owner, None).await;
    let first = wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
    model
        .update_thread_statuses(43, &[11], ThreadStatus::RUNNING)
        .await
        .unwrap();
    let second = wait_policy(&service, |policy| {
        policy.revision > first.revision
            && policy.pending_targets.is_empty()
            && !policy.target_outcomes.is_empty()
    })
    .await;
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.initiating_stop, first.initiating_stop);
    assert_eq!(
        port.calls.lock().unwrap().len(),
        2,
        "the new pause must not replay the previous operation"
    );
}

#[tokio::test]
async fn explicit_pause_is_a_cause_but_a_late_automatic_pause_is_not() {
    let (model, service, port, owner) = policy_fixture().await;
    service
        .configure_stop_policy(ddb_api_types::v2::StopMode::AllStop)
        .unwrap();
    model
        .update_thread_statuses_with_details(
            42,
            &[11],
            ThreadStatus::STOPPED,
            None,
            Some(crate::state::ThreadStopReason {
                kind: crate::state::ThreadStopKind::Signal,
                signal_name: Some("SIGINT".into()),
                breakpoint_id: None,
                thread_id: Some(owner),
                log_message: None,
            }),
        )
        .await
        .unwrap();
    wait_policy(&service, |policy| !policy.target_outcomes.is_empty()).await;
    assert_eq!(port.calls.lock().unwrap().len(), 1);
    model.retire_stop_automation();
    wait_policy(&service, |policy| policy.initiating_stop.is_none()).await;
    let peer = model.thread_snapshots_for_sessions(&[43]).await[0]
        .global_id
        .value();
    model
        .update_thread_statuses_with_details(
            43,
            &[11],
            ThreadStatus::STOPPED,
            None,
            Some(crate::state::ThreadStopReason {
                kind: crate::state::ThreadStopKind::Signal,
                signal_name: Some("SIGINT".into()),
                breakpoint_id: None,
                thread_id: Some(peer),
                log_message: None,
            }),
        )
        .await
        .unwrap();
    let revision = model.thread_snapshots_for_sessions(&[43]).await[0].stop_revision;
    tokio::time::timeout(Duration::from_secs(1), async {
        while !service.stop_policies.classified_stop(peer, revision) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(service
        .stop_policy_snapshot()
        .unwrap()
        .initiating_stop
        .is_none());
    assert_eq!(port.calls.lock().unwrap().len(), 1);
}
