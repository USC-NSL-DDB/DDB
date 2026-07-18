use anyhow::Result;
use axum::{
    extract::Query,
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
    cmd_flow::{api as cmd_flow_api, router::Target, FinishedCmd},
    notification,
    state::{get_bkpt_mgr, BkptLoc, BkptMeta, GroupId, GroupMeta, SubBkptMeta, SubBkptType},
    status::{get_rt_status, Component},
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
struct SourceResolver {
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
                target_group: g.target_group(),
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

#[derive(Debug)]
pub struct ApiServer {
    addr: String,
}

impl ApiServer {
    pub fn new(addr: &str) -> Self {
        ApiServer {
            addr: addr.to_string(),
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
            .with_state(notification::get_notif_mgr())
            .layer(TraceLayer::new_for_http());

        let listener = tokio::net::TcpListener::bind(self.addr.clone()).await?;
        info!("[API Server]: Listening on {}", listener.local_addr()?);
        get_rt_status().up(Component::Api);

        let shutdown = async move {
            let _ = shutdown_rx.changed().await;
        };

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

impl Default for ApiServer {
    fn default() -> Self {
        ApiServer::new("localhost:5000")
    }
}

// Root handler
#[cfg_attr(feature = "profile", tracing::instrument)]
async fn root_handler() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome to the Axum API server!".to_string(),
    })
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn resolve_src_to_group_ids(Query(src): Query<SourceResolver>) -> impl IntoResponse {
    let src = src.src;
    #[cfg(feature = "lazy_source_map")]
    let grp_ids = crate::state::get_source_mgr()
        .resolve_src_to_group_ids(&src)
        .await;

    #[cfg(not(feature = "lazy_source_map"))]
    let grp_ids = crate::state::get_source_mgr().resolve_src_to_group_ids(&src);

    let grp_ids = grp_ids.unwrap_or_default().into_iter().collect::<Vec<_>>();
    (StatusCode::OK, Json(GroupIdsResponse { grp_ids: grp_ids }))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn resolve_src_to_groups(Query(src): Query<SourceResolver>) -> impl IntoResponse {
    let src = src.src;
    #[cfg(feature = "lazy_source_map")]
    let grps = crate::state::get_source_mgr()
        .resolve_src_to_groups(&src)
        .await;

    #[cfg(not(feature = "lazy_source_map"))]
    let grps = crate::state::get_source_mgr().resolve_src_to_groups(&src);

    let grps = grps.unwrap_or_default();
    (StatusCode::OK, Json(GroupsResponse { grps: grps }))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn send_cmd(Json(send_cmd): Json<SendCommand>) -> impl IntoResponse {
    debug!("Received command: {:?}", send_cmd);

    let result: Result<Option<FinishedCmd>> = async {
        if send_cmd.wait {
            Ok(Some(
                cmd_flow_api::command(&send_cmd.cmd)?
                    .target_or_default(send_cmd.target)
                    .execute()
                    .await?,
            ))
        } else {
            cmd_flow_api::command(&send_cmd.cmd)?
                .target_or_default(send_cmd.target)
                .submit()
                .await?;
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
async fn get_status() -> impl IntoResponse {
    let is_up = crate::status::get_rt_status().is_up();
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
async fn get_sessions() -> impl IntoResponse {
    // if it has performance issues, we can probably parallelize this
    // or maybe do it in parallel conditionally when the size
    // is above a certain threshold
    let mut results = vec![];
    let ss = crate::state::STATES.sessions();
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
        let grp_info = crate::state::get_group_mgr().group_info_by_session(sid);
        let session = json!({
            "sid": sid,
            "tag": tag,
            "alias": alias,
            "status": status,
            "group": {
                "valid": grp_info.is_some(),
                "id": grp_info.as_ref().map(|(id, _)| *id).unwrap_or(0),
                "hash": grp_info.as_ref().map(|(_, hash)| hash).unwrap_or(&"UNKNOWN".to_string()),
            }
        });
        results.push(session);
    }

    (StatusCode::OK, Json(json!(results)))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_pending_commands() -> impl IntoResponse {
    let statuses = crate::cmd_flow::get_router().runtime_statuses();
    (StatusCode::OK, Json(json!(statuses)))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_groups() -> impl IntoResponse {
    let group_mgr = crate::state::get_group_mgr();
    let result: Vec<GroupMeta> = group_mgr.groups();
    (StatusCode::OK, Json(result))
}

#[cfg_attr(feature = "profile", tracing::instrument)]
async fn get_group(Query(query): Query<GetGroupQuery>) -> impl IntoResponse {
    let group_mgr = crate::state::get_group_mgr();
    if let Some(grp_id) = query.grp_id {
        if let Some(group_meta) = group_mgr.group_by_id(grp_id) {
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
async fn get_bkpts() -> impl IntoResponse {
    let bkpts: Vec<BkptJson> = get_bkpt_mgr()
        .breakpoints()
        .iter()
        .map(|bkpt| bkpt.into())
        .collect();
    (StatusCode::OK, Json(BkptsResponse { bkpts }))
}
