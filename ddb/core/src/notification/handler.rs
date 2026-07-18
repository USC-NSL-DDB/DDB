use super::manager::{NotificationManager, SubscribeError, SUBSCRIBER_QUEUE_CAPACITY};
use super::message::{CustomEvent, Notification, NotificationPayload};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// WebSocket subscription endpoint
pub async fn notification_subscribe_handler(
    ws: WebSocketUpgrade,
    State(manager): State<Arc<NotificationManager>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, manager))
}

async fn handle_socket(socket: WebSocket, manager: Arc<NotificationManager>) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);

    // Subscribe to notifications
    let subscriber_id = match manager.subscribe(tx) {
        Ok(id) => id,
        Err(SubscribeError::MaxSubscribersReached) => {
            error!("Cannot accept new subscriber: max limit reached");
            return;
        }
    };

    info!(
        "WebSocket connection established for subscriber {}",
        subscriber_id
    );

    // Send welcome message with subscriber ID
    let welcome = json!({
        "type": "welcome",
        "subscriber_id": subscriber_id.to_string(),
        "max_subscribers": 20,
    });

    let welcome_msg = Message::Text(welcome.to_string());
    if sender.send(welcome_msg).await.is_err() {
        manager.unsubscribe(subscriber_id);
        return;
    }

    // Forward messages from notification channel to WebSocket
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming WebSocket messages (mostly pongs)
    let subscriber_id_clone = subscriber_id;
    let manager_clone = Arc::clone(&manager);
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Pong(_) => {
                    debug!("Received pong from subscriber {}", subscriber_id_clone);
                    manager_clone.record_pong(subscriber_id_clone).await;
                }
                Message::Close(_) => {
                    info!("Client {} requested close", subscriber_id_clone);
                    break;
                }
                _ => {
                    // Ignore other message types (we only send, don't receive commands)
                }
            }
        }
        manager_clone.unsubscribe(subscriber_id_clone);
    });

    // Wait for either task to complete
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }

    manager.unsubscribe(subscriber_id);
}

/// Status endpoint
pub async fn notification_status_handler(
    State(manager): State<Arc<NotificationManager>>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "subscriber_count": manager.subscriber_count(),
            "max_subscribers": 20,
        })),
    )
}

/// Test notification endpoint
#[derive(Deserialize)]
pub struct TestNotificationRequest {
    message: String,
}

#[derive(Serialize)]
pub struct TestNotificationResponse {
    success: bool,
    message: String,
}

pub async fn test_notification_handler(
    State(manager): State<Arc<NotificationManager>>,
    Json(req): Json<TestNotificationRequest>,
) -> impl IntoResponse {
    let notification = Notification::new(NotificationPayload::Custom(CustomEvent {
        event_type: "test".to_string(),
        data: json!({ "message": req.message }),
    }));

    manager.broadcast(notification).await;

    (
        StatusCode::OK,
        Json(TestNotificationResponse {
            success: true,
            message: "Test notification sent".to_string(),
        }),
    )
}
