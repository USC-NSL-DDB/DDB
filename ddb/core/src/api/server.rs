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

use crate::{
    cmd_flow::{
        engine::CommandEngine,
        router::{Router as CommandRouter, Target},
        FinishedCmd,
    },
    notification::{self, NotificationManager},
    runtime_model::RuntimeModel,
    source::resolver::SourceResolver,
    state::{BkptLoc, BkptMeta, GroupId, GroupMeta, SubBkptMeta, SubBkptType},
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
    grp_ids: Vec<GroupId>,
}

#[derive(Serialize)]
struct GroupsResponse {
    grps: Vec<GroupMeta>,
}

// Struct for JSON output
#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

// Breakpoint API response types
#[derive(Serialize, Debug, Clone)]
pub struct BkptLocJson {
    src: String,
    line: u64,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum SubBkptJson {
    #[serde(rename = "session")]
    Session {
        id: u64,
        target_session: u64,
        local_breakpoint_id: u64,
    },
    #[serde(rename = "group")]
    Group {
        id: u64,
        target_group: u64,
        active_sessions: usize,
    },
}

#[derive(Serialize, Debug, Clone)]
pub struct BkptJson {
    id: u64,
    location: BkptLocJson,
    enabled: bool,
    times: u64,
    subbkpts: Vec<SubBkptJson>,
}

#[derive(Serialize, Debug)]
struct BkptsResponse {
    bkpts: Vec<BkptJson>,
}

impl From<&BkptLoc> for BkptLocJson {
    fn from(loc: &BkptLoc) -> Self {
        BkptLocJson {
            src: loc.path().to_string(),
            line: loc.line(),
        }
    }
}

impl From<&SubBkptMeta> for SubBkptJson {
    fn from(subbkpt: &SubBkptMeta) -> Self {
        match subbkpt.kind() {
            SubBkptType::Session(s) => SubBkptJson::Session {
                id: subbkpt.id(),
                target_session: s.target_session(),
                local_breakpoint_id: s.local_id(),
            },
            SubBkptType::Group(g) => SubBkptJson::Group {
                id: subbkpt.id(),
                target_group: g.target_group().value(),
                active_sessions: g.local_ids().len(),
            },
        }
    }
}

impl From<&BkptMeta> for BkptJson {
    fn from(bkpt: &BkptMeta) -> Self {
        BkptJson {
            id: bkpt.id(),
            location: bkpt.location().into(),
            enabled: bkpt.is_enabled(),
            times: bkpt.times(),
            subbkpts: bkpt.sub_breakpoints().iter().map(|s| s.into()).collect(),
        }
    }
}

#[derive(Clone)]
struct ApiState {
    notifications: Arc<NotificationManager>,
    source_resolver: Arc<SourceResolver>,
    command_engine: Arc<CommandEngine>,
    command_router: Arc<CommandRouter>,
    model: Arc<RuntimeModel>,
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

impl FromRef<ApiState> for Arc<CommandRouter> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.command_router)
    }
}

impl FromRef<ApiState> for Arc<RuntimeModel> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.model)
    }
}

impl FromRef<ApiState> for Arc<RuntimeStatus> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.status)
    }
}

impl FromRef<ApiState> for Arc<SourceResolver> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.source_resolver)
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
        source_resolver: Arc<SourceResolver>,
        command_engine: Arc<CommandEngine>,
        command_router: Arc<CommandRouter>,
        model: Arc<RuntimeModel>,
        status: Arc<RuntimeStatus>,
    ) -> Self {
        Self {
            addr: addr.into(),
            state: ApiState {
                notifications,
                source_resolver,
                command_engine,
                command_router,
                model,
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

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn resolve_src_to_group_ids(
    State(resolver): State<Arc<SourceResolver>>,
    Query(src): Query<SourceQuery>,
) -> std::result::Result<Json<GroupIdsResponse>, (StatusCode, Json<ApiResponse>)> {
    let mut grp_ids = resolver
        .group_ids_for(&src.src)
        .await
        .map_err(source_resolution_error)?
        .into_iter()
        .collect::<Vec<_>>();
    grp_ids.sort_unstable();
    Ok(Json(GroupIdsResponse { grp_ids }))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn resolve_src_to_groups(
    State(resolver): State<Arc<SourceResolver>>,
    Query(src): Query<SourceQuery>,
) -> std::result::Result<Json<GroupsResponse>, (StatusCode, Json<ApiResponse>)> {
    let grps = resolver
        .groups_for(&src.src)
        .await
        .map_err(source_resolution_error)?;
    Ok(Json(GroupsResponse { grps }))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
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

#[cfg_attr(feature = "profile", tracing::instrument)]
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

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_sessions(State(model): State<Arc<RuntimeModel>>) -> impl IntoResponse {
    // if it has performance issues, we can probably parallelize this
    // or maybe do it in parallel conditionally when the size
    // is above a certain threshold
    let mut results = vec![];
    let ss = model.state().sessions();
    for s in ss {
        let (sid, tag, alias, status) = s
            .read_with(|s_meta| {
                (
                    s_meta.sid(),
                    s_meta.tag().to_string(),
                    s_meta
                        .service_meta()
                        .map(|x| x.alias.clone())
                        .unwrap_or("UNKNOWN".to_string()),
                    s_meta.status().to_string(),
                )
            })
            .await;
        let grp_info = model.groups().group_info_by_session(sid);
        let session = json!({
            "sid": sid,
            "tag": tag,
            "alias": alias,
            "status": status,
            "group": {
                "valid": grp_info.is_some(),
                "id": grp_info.as_ref().map(|(id, _)| *id).map(crate::state::GroupId::value).unwrap_or(0),
                "hash": grp_info.as_ref().map(|(_, hash)| hash).unwrap_or(&"UNKNOWN".to_string()),
            }
        });
        results.push(session);
    }

    (StatusCode::OK, Json(json!(results)))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_pending_commands(State(router): State<Arc<CommandRouter>>) -> impl IntoResponse {
    let statuses = router.runtime_statuses();
    (StatusCode::OK, Json(json!(statuses)))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_groups(State(model): State<Arc<RuntimeModel>>) -> impl IntoResponse {
    let group_mgr = model.groups();
    let result: Vec<GroupMeta> = group_mgr.groups();
    (StatusCode::OK, Json(result))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_group(
    State(model): State<Arc<RuntimeModel>>,
    Query(query): Query<GetGroupQuery>,
) -> impl IntoResponse {
    let group_mgr = model.groups();
    if let Some(grp_id) = query.grp_id {
        if let Some(group_meta) = group_mgr.group_by_id(GroupId::new(grp_id)) {
            (StatusCode::OK, Json(json!(group_meta)))
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Group not found"})),
            )
        }
    } else if let Some(grp_hash) = query.grp_hash {
        if let Some(group_meta) = group_mgr.group_by_hash(&grp_hash) {
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

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_bkpts(State(model): State<Arc<RuntimeModel>>) -> impl IntoResponse {
    let bkpts: Vec<BkptJson> = model
        .breakpoints()
        .breakpoints()
        .iter()
        .map(|bkpt| bkpt.into())
        .collect();
    (StatusCode::OK, Json(BkptsResponse { bkpts }))
}
