use super::manager::{NotificationManager, SubscribeError, MAX_SUBSCRIBERS};
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
    let mut subscription = match manager.subscribe() {
        Ok(subscription) => subscription,
        Err(SubscribeError::MaxSubscribersReached) => {
            error!("Cannot accept new subscriber: max limit reached");
            return;
        }
    };
    let subscriber_id = subscription.id();

    info!(
        "WebSocket connection established for subscriber {}",
        subscriber_id
    );
    let welcome = json!({
        "type": "welcome",
        "subscriber_id": subscriber_id.to_string(),
        "max_subscribers": MAX_SUBSCRIBERS,
    });
    if sender
        .send(Message::Text(welcome.to_string()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            outgoing = subscription.recv() => {
                let Some(message) = outgoing else {
                    break;
                };
                if sender.send(message).await.is_err() {
                    break;
                }
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Pong(_))) => {
                        debug!("Received pong from subscriber {}", subscriber_id);
                        manager.record_pong(subscriber_id);
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Client {} requested close", subscriber_id);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        debug!(subscriber_id = %subscriber_id, ?error, "WebSocket receive failed");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

/// Status endpoint
pub async fn notification_status_handler(
    State(manager): State<Arc<NotificationManager>>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "subscriber_count": manager.subscriber_count(),
            "max_subscribers": MAX_SUBSCRIBERS,
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
