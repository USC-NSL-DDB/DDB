use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{FromRef, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::trace::TraceLayer;
use tracing::{debug, info};

use super::read_model::{ApiQueries, GroupView};
use crate::state::BreakpointSnapshot;

use crate::{
    cmd_flow::{engine::CommandEngine, router::Target, FinishedCmd},
    notification::{self, NotificationManager},
    status::{Component, RuntimeStatus},
};

#[derive(Deserialize, Debug, Clone)]
struct SendCommand {
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    target: Option<Target>,
    cmd: String,
}

#[derive(Serialize)]
struct SendCommandResponse {
    message: String,
    success: bool,
    payload: Option<FinishedCmd>,
}

#[derive(Deserialize, Debug)]
struct GetGroupQuery {
    grp_id: Option<u64>,
    grp_hash: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SourceQuery {
    src: String,
}

#[derive(Serialize)]
struct GroupIdsResponse {
    grp_ids: Vec<u64>,
}

#[derive(Serialize)]
struct GroupsResponse {
    grps: Vec<GroupView>,
}

#[derive(Serialize)]
struct BkptsResponse {
    bkpts: Vec<BreakpointSnapshot>,
}

// Struct for JSON output
#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

#[derive(Clone)]
struct ApiState {
    notifications: Arc<NotificationManager>,
    command_engine: Arc<CommandEngine>,
    queries: Arc<ApiQueries>,
    status: Arc<RuntimeStatus>,
}

impl FromRef<ApiState> for Arc<NotificationManager> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.notifications)
    }
}

impl FromRef<ApiState> for Arc<CommandEngine> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.command_engine)
    }
}

impl FromRef<ApiState> for Arc<ApiQueries> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.queries)
    }
}

impl FromRef<ApiState> for Arc<RuntimeStatus> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.status)
    }
}

pub struct ApiServer {
    addr: String,
    state: ApiState,
}

impl ApiServer {
    pub fn new(
        addr: impl Into<String>,
        notifications: Arc<NotificationManager>,
        command_engine: Arc<CommandEngine>,
        queries: Arc<ApiQueries>,
        status: Arc<RuntimeStatus>,
    ) -> Self {
        Self {
            addr: addr.into(),
            state: ApiState {
                notifications,
                command_engine,
                queries,
                status,
            },
        }
    }

    pub async fn run(
        &self,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), std::io::Error> {
        let app = Router::new()
            .route("/", get(root_handler))
            .route("/status", get(get_status))
            .route("/sessions", get(get_sessions))
            .route("/pcommands", get(get_pending_commands))
            .route("/src_to_grp_ids", get(resolve_src_to_group_ids))
            .route("/src_to_grps", get(resolve_src_to_groups))
            .route("/send", post(send_cmd))
            .route("/groups", get(get_groups))
            .route("/group", get(get_group))
            .route("/bkpts", get(get_bkpts))
            .route(
                "/notifications/subscribe",
                get(notification::notification_subscribe_handler),
            )
            .route(
                "/notifications/status",
                get(notification::notification_status_handler),
            )
            .route(
                "/notifications/test",
                post(notification::test_notification_handler),
            )
            .with_state(self.state.clone())
            .layer(TraceLayer::new_for_http());

        let listener = tokio::net::TcpListener::bind(self.addr.clone()).await?;
        info!("[API Server]: Listening on {}", listener.local_addr()?);
        self.state.status.up(Component::Api);

        let shutdown = async move {
            let _ = shutdown_rx.changed().await;
        };

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

// Root handler
#[cfg_attr(feature = "profile", tracing::instrument)]
async fn root_handler() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome to the Axum API server!".to_string(),
    })
}

fn source_resolution_error(error: anyhow::Error) -> (StatusCode, Json<ApiResponse>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(ApiResponse {
            message: format!("Failed to resolve debugger sources: {error:#}"),
        }),
    )
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn resolve_src_to_group_ids(
    State(queries): State<Arc<ApiQueries>>,
    Query(src): Query<SourceQuery>,
) -> std::result::Result<Json<GroupIdsResponse>, (StatusCode, Json<ApiResponse>)> {
    let grp_ids = queries
        .group_ids_for_source(&src.src)
        .await
        .map_err(source_resolution_error)?;
    Ok(Json(GroupIdsResponse { grp_ids }))
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn resolve_src_to_groups(
    State(queries): State<Arc<ApiQueries>>,
    Query(src): Query<SourceQuery>,
) -> std::result::Result<Json<GroupsResponse>, (StatusCode, Json<ApiResponse>)> {
    let grps = queries
        .groups_for_source(&src.src)
        .await
        .map_err(source_resolution_error)?;
    Ok(Json(GroupsResponse { grps }))
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(engine)))]
async fn send_cmd(
    State(engine): State<Arc<CommandEngine>>,
    Json(send_cmd): Json<SendCommand>,
) -> impl IntoResponse {
    debug!("Received command: {:?}", send_cmd);

    let result: Result<Option<FinishedCmd>> = async {
        if send_cmd.wait {
            Ok(engine
                .execute_api(&send_cmd.cmd, send_cmd.target)
                .await?
                .into_response())
        } else {
            engine.submit_api(&send_cmd.cmd, send_cmd.target).await?;
            Ok(None)
        }
    }
    .await;

    match result {
        Ok(Some(finished_cmd)) => (
            StatusCode::OK,
            Json(SendCommandResponse {
                message: "success".to_string(),
                success: true,
                payload: Some(finished_cmd),
            }),
        ),
        Ok(None) => (
            StatusCode::OK,
            Json(SendCommandResponse {
                message: "success".to_string(),
                success: true,
                payload: None,
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(SendCommandResponse {
                message: format!("Failed to process command: {}", e),
                success: false,
                payload: None,
            }),
        ),
    }
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(status)))]
async fn get_status(State(status): State<Arc<RuntimeStatus>>) -> impl IntoResponse {
    let is_up = status.is_up();
    if is_up {
        (StatusCode::OK, Json(json!({"status": "up"})))
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "down"})),
        )
    }
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn get_sessions(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.sessions().await))
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn get_pending_commands(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.pending_commands()))
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn get_groups(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.groups()))
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn get_group(
    State(queries): State<Arc<ApiQueries>>,
    Query(query): Query<GetGroupQuery>,
) -> impl IntoResponse {
    if let Some(grp_id) = query.grp_id {
        if let Some(group_meta) = queries.group_by_id(grp_id) {
            (StatusCode::OK, Json(json!(group_meta)))
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Group not found"})),
            )
        }
    } else if let Some(grp_hash) = query.grp_hash {
        if let Some(group_meta) = queries.group_by_hash(&grp_hash) {
            (StatusCode::OK, Json(json!(group_meta)))
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Group not found"})),
            )
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Either grp_id or grp_hash must be provided"})),
        )
    }
}

#[cfg_attr(feature = "profile", tracing::instrument(skip(queries)))]
async fn get_bkpts(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(BkptsResponse {
            bkpts: queries.breakpoints(),
        }),
    )
}
