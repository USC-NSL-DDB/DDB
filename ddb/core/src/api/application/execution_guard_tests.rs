async fn guarded_fixture() -> (
    Arc<RuntimeModel>,
    Arc<DdbApplicationService>,
    Arc<RecordingCommandPort>,
    PublicTarget,
) {
    let model = RuntimeModel::new();
    model.register_session(42, "worker", None).await;
    model.complete_session_activation(42, None).await;
    model.register_thread_group(42, "i1").await.unwrap();
    let thread = model.register_thread(42, 11, "i1").await.unwrap().thread_id;
    model
        .update_thread_statuses(42, &[11], ThreadStatus::STOPPED)
        .await
        .unwrap();
    let port = Arc::new(RecordingCommandPort::default());
    let service = service_with_port(model.clone(), port.clone());
    let target = PublicTarget {
        selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
            thread_id: service.ids.encode(ResourceIdKind::Thread, thread).unwrap(),
        })),
    };
    (model, service, port, target)
}

async fn guarded_request(
    service: &DdbApplicationService,
    target: PublicTarget,
    key: &str,
) -> ExecuteRequest {
    let state = service
        .get_execution_state(GetExecutionStateRequest {
            target: Some(target.clone()),
            ..Default::default()
        })
        .await
        .unwrap()
        .execution_state
        .unwrap();
    ExecuteRequest {
        execution_guard: Some(ddb_api_types::v2::ExecutionGuard {
            server_instance_id: service.server_instance_id().into(),
            execution_state_id: state.execution_state_id,
            revision: state.revision,
        }),
        ..execute_request(key, target, ExecutionAction::Continue)
    }
}

async fn terminal_operation(service: &DdbApplicationService, id: &str) -> Operation {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let operation = service.operations.get(id).unwrap();
            if matches!(
                OperationState::try_from(operation.state).unwrap(),
                OperationState::Completed | OperationState::Failed
            ) {
                return operation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("operation did not finish")
}

#[tokio::test]
async fn invalidating_commands_cancel_queued_logpoints_before_admission_returns() {
    for (kind, command) in [
        (OperationKind::Evaluate, "-data-evaluate-expression counter=7"),
        (OperationKind::SetVariable, "-var-assign watched-counter 7"),
        (OperationKind::Execute, "-exec-next"),
        (OperationKind::RawCommand, "-interpreter-exec console continue"),
        (OperationKind::UpdateBreakpoint, "-break-disable 1"),
        (OperationKind::DeleteBreakpoint, "-break-delete 1"),
    ] {
        let (model, service, port, owner) = policy_fixture().await;
        port.block_continue.store(true, Ordering::SeqCst);
        policy_stop(&model, owner, Some("queued automatic output")).await;
        tokio::time::timeout(Duration::from_secs(1), port.continue_queued.notified())
            .await.unwrap();
        let epoch = model.execution_control_epoch().current();
        let context = RequestContext {
            idempotency_key: Some("explicit-intent".into()),
            ..Default::default()
        };
        let scope = RequestScope::begin(Some(&context)).unwrap();
        let principal = PrincipalContext::new("explicit-client").unwrap();
        let admit = || service.admit_command(
            &principal, &scope, Some(&context), command.as_bytes(), kind,
            ResolvedTarget {
                public: service_thread_target(&service, owner),
                command: CommandTarget::Thread(crate::state::GlobalThreadId::new(owner)),
                session_ids: vec![42],
                resolved_target_count: 1,
            },
            command.into(),
            super::super::mutations::CompletionProjection::NoContent,
        ).unwrap().operation.unwrap();
        let first = admit();
        assert!(model.execution_control_epoch().current() > epoch,
            "{kind:?} must retire automation before the accepted operation is visible");
        let retired = model.execution_control_epoch().current();
        assert_eq!(admit().operation_id, first.operation_id);
        assert_eq!(model.execution_control_epoch().current(), retired,
            "idempotent replay must not cancel again");
        port.release_continue.notify_one();
        terminal_operation(&service, &first.operation_id).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !service.operations.list_with_context().iter().any(|entry| {
                entry.operation.kind == OperationKind::Execute as i32
                    && entry.operation.state == OperationState::Failed as i32
            }) {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(!port.calls.lock().unwrap().iter()
            .any(|(call, _)| call == "-record-time-and-continue"));
    }
}

#[tokio::test]
async fn public_repl_evaluation_retires_automation_at_admission() {
    let (model, service, port, owner) = policy_fixture().await;
    port.block_continue.store(true, Ordering::SeqCst);
    policy_stop(&model, owner, Some("queued automatic output")).await;
    tokio::time::timeout(Duration::from_secs(1), port.continue_queued.notified())
        .await.unwrap();
    let epoch = model.execution_control_epoch().current();
    service.evaluate(
        &PrincipalContext::new("repl-client").unwrap(),
        ddb_api_types::v2::EvaluateRequest {
            context: Some(RequestContext { idempotency_key: Some("repl-mutation".into()), ..Default::default() }),
            target: Some(service_thread_target(&service, owner)),
            expression: "counter=7".into(),
            evaluation_context: ddb_api_types::v2::EvaluationContext::Repl as i32,
            ..Default::default()
        },
    ).await.unwrap();
    assert!(model.execution_control_epoch().current() > epoch,
        "accepted REPL work must not leave queued logpoint continuation admissible");
    port.release_continue.notify_one();
}

#[tokio::test]
async fn execution_guards_allow_only_one_client_to_consume_a_stop() {
    let (_, service, port, target) = guarded_fixture().await;
    let a = PrincipalContext::new("client-a").unwrap();
    let b = PrincipalContext::new("client-b").unwrap();
    let request = guarded_request(&service, target, "same-stop").await;
    let (first, second) = tokio::join!(
        service.execute(&a, request.clone()),
        service.execute(&b, request.clone())
    );
    let first = terminal_operation(&service, &first.unwrap().operation.unwrap().operation_id).await;
    let second =
        terminal_operation(&service, &second.unwrap().operation.unwrap().operation_id).await;
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert!(first.state != second.state);
    let failed = if first.state == OperationState::Failed as i32 {
        first
    } else {
        second
    };
    assert_eq!(
        failed.error.unwrap().code,
        DdbErrorCode::FailedPrecondition as i32
    );
    let retry = service
        .execute(&a, request)
        .await
        .unwrap()
        .operation
        .unwrap();
    assert_eq!(
        retry.state,
        service.operations.get(&retry.operation_id).unwrap().state
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sibling_thread_guards_cannot_resume_the_same_native_process_twice() {
    let (model, service, port, first_target) = guarded_fixture().await;
    let sibling = model.register_thread(42, 12, "i1").await.unwrap().thread_id;
    model
        .update_thread_statuses(42, &[12], ThreadStatus::STOPPED)
        .await
        .unwrap();
    let second_target = PublicTarget {
        selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
            thread_id: service.ids.encode(ResourceIdKind::Thread, sibling).unwrap(),
        })),
    };
    let a = guarded_request(&service, first_target, "first-thread").await;
    let b = guarded_request(&service, second_target.clone(), "second-thread").await;
    let first = service
        .execute(&PrincipalContext::new("client-a").unwrap(), a)
        .await
        .unwrap()
        .operation
        .unwrap();
    assert_eq!(
        terminal_operation(&service, &first.operation_id)
            .await
            .state,
        OperationState::Completed as i32
    );
    // No native Running event has arrived yet, including for the sibling.
    let second = service
        .execute(&PrincipalContext::new("client-b").unwrap(), b)
        .await
        .unwrap()
        .operation
        .unwrap();
    let failed = terminal_operation(&service, &second.operation_id).await;
    assert_eq!(
        failed.error.unwrap().code,
        DdbErrorCode::FailedPrecondition as i32
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    model
        .update_thread_statuses(42, &[11, 12], ThreadStatus::STOPPED)
        .await
        .unwrap();
    let next = guarded_request(&service, second_target, "new-process-stop").await;
    let next = service
        .execute(&PrincipalContext::new("client-b").unwrap(), next)
        .await
        .unwrap()
        .operation
        .unwrap();
    assert_eq!(
        terminal_operation(&service, &next.operation_id).await.state,
        OperationState::Completed as i32
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn execution_guards_reject_intervening_stops_and_backend_restarts() {
    let (model, service, port, target) = guarded_fixture().await;
    let principal = PrincipalContext::new("controller").unwrap();
    let request = guarded_request(&service, target.clone(), "old-stop").await;
    // Another stop is distinct even without an intervening running event.
    model
        .update_thread_statuses(42, &[11], ThreadStatus::STOPPED)
        .await
        .unwrap();
    assert_eq!(
        service
            .execute(&principal, request)
            .await
            .unwrap_err()
            .code(),
        DdbErrorCode::FailedPrecondition
    );
    let mut request = guarded_request(&service, target, "old-instance").await;
    request.execution_guard.as_mut().unwrap().server_instance_id = "previous-backend".into();
    assert_eq!(
        service
            .execute(&principal, request)
            .await
            .unwrap_err()
            .code(),
        DdbErrorCode::FailedPrecondition
    );
    let replacement = service_with_port(model, port.clone());
    let mut request = execute_request(
        "restart",
        session_target(replacement.ids.encode(ResourceIdKind::Session, 42).unwrap()),
        ExecutionAction::Continue,
    );
    request.preconditions = Some(ddb_api_types::v2::Preconditions {
        server_instance_id: Some(service.server_instance_id().into()),
        ..Default::default()
    });
    assert_eq!(
        replacement
            .execute(&principal, request)
            .await
            .unwrap_err()
            .code(),
        DdbErrorCode::FailedPrecondition
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn execution_guard_is_rechecked_after_admission_before_dispatch() {
    let (model, service, port, target) = guarded_fixture().await;
    let principal = PrincipalContext::new("controller").unwrap();
    let request = guarded_request(&service, target, "queued-stop").await;
    // The spawned operation cannot run until this task yields.
    let admitted = service
        .execute(&principal, request)
        .await
        .unwrap()
        .operation
        .unwrap();
    model
        .update_thread_statuses(42, &[11], ThreadStatus::STOPPED)
        .await
        .unwrap();
    let failed = terminal_operation(&service, &admitted.operation_id).await;
    assert_eq!(failed.state, OperationState::Failed as i32);
    assert_eq!(
        failed.error.unwrap().code,
        DdbErrorCode::FailedPrecondition as i32
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_stop_guard_survives_backend_context_restoration_but_not_a_new_stop() {
    let (model, service, _, target) = guarded_fixture().await;
    let thread = model.thread_snapshots_for_sessions(&[42]).await[0].global_id;
    model
        .enter_custom_context(
            42,
            crate::state::ThreadContext {
                tid: thread,
                ctx: std::collections::HashMap::from([("pc".into(), 42)]),
            },
        )
        .await;
    let request = guarded_request(&service, target.clone(), "restore").await;
    let resolved = service
        .target_resolver()
        .resolve(Some(&target), super::super::TargetPurpose::Command)
        .await
        .unwrap();
    let guard = service
        .execution_guard(request.execution_guard.as_ref(), &resolved)
        .await
        .unwrap()
        .unwrap();
    model.finish_context_restore(42, true).await;
    guard.check().await.unwrap();
    assert!(guard.check().await.is_err());
    model
        .update_thread_statuses(42, &[11], ThreadStatus::STOPPED)
        .await
        .unwrap();
    assert!(guard.check().await.is_err());
}

#[tokio::test]
async fn native_stop_cannot_change_between_validation_and_writer_admission() {
    let (model, service, _, target) = guarded_fixture().await;
    let request = guarded_request(&service, target.clone(), "held-native-stop").await;
    let resolved = service
        .target_resolver()
        .resolve(Some(&target), super::super::TargetPurpose::Command)
        .await
        .unwrap();
    let guard = service
        .execution_guard(request.execution_guard.as_ref(), &resolved)
        .await
        .unwrap()
        .unwrap();
    let mut permit = guard.prepare().await.unwrap();
    let mut new_stop = Box::pin(model.update_thread_statuses(42, &[11], ThreadStatus::STOPPED));
    assert!(futures::poll!(new_stop.as_mut()).is_pending());
    let mut admitted = false;
    permit
        .dispatch(&mut || Ok(()), &mut || {
            admitted = true;
            Ok(())
        })
        .unwrap();
    assert!(admitted);
    guard.dispatched();
    drop(permit);
    new_stop.await.unwrap();
    assert!(guard.prepare().await.is_err());
}
