use super::*;
use axum::{routing::post, Json, Router};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn termination_revalidates_before_sending_a_guarded_session_signal() {
    for scenario in ["current", "changed", "removed", "replacement"] {
        let session = v2::Session {
            session_id: "session/captured".into(),
            revision: 8,
            status: v2::SessionStatus::Stopped as i32,
            ..Default::default()
        };
        let request = crate::model::SessionTermination {
            instance: "original".into(),
            session: session.clone(),
            scroll: 0,
        };
        let mut snapshot = v2::Snapshot {
            server_instance_id: "original".into(),
            base_state_revision: 42,
            sessions: vec![session],
            ..Default::default()
        };
        match scenario {
            "changed" => snapshot.sessions[0].revision += 1,
            "removed" => snapshot.sessions.clear(),
            "replacement" => snapshot.server_instance_id = "replacement".into(),
            _ => {}
        }
        let executed = Arc::new(AtomicUsize::new(0));
        let calls = executed.clone();
        let router = Router::new()
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerService/GetSnapshot",
                post(move |Json(request): Json<v2::GetSnapshotRequest>| {
                    let snapshot = snapshot.clone();
                    async move {
                        assert_eq!(request.sections, vec![v2::SnapshotSection::Topology as i32]);
                        Json(v2::GetSnapshotResponse {
                            snapshot: Some(snapshot),
                            ..Default::default()
                        })
                    }
                }),
            )
            .route(
                "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
                post(move |Json(request): Json<v2::ExecuteRequest>| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            request.target,
                            Some(crate::api::session_target("session/captured"))
                        );
                        assert_eq!(request.action, v2::ExecutionAction::Signal as i32);
                        assert_eq!(request.signal_name.as_deref(), Some("SIGKILL"));
                        assert_eq!(request.preconditions.unwrap().state_revision, Some(42));
                        Json(v2::OperationAdmissionResponse {
                            operation: Some(v2::Operation {
                                operation_id: "operation/terminate".into(),
                                state: v2::OperationState::Completed as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    }
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
        let result = handle(&client, &tx, BackendRequest::TerminateSession(request)).await;
        assert_eq!(
            result.is_ok(),
            scenario == "current",
            "{scenario}: {result:?}"
        );
        assert_eq!(
            executed.load(Ordering::SeqCst),
            usize::from(scenario == "current")
        );
        if scenario == "current" {
            assert!(
                matches!(rx.try_recv().unwrap(), UiMessage::Receipt(label, _) if label == "terminate session")
            );
        } else {
            assert!(rx.try_recv().is_err());
        }
        server.abort();
        let _ = server.await;
    }
}
