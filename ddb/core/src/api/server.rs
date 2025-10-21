use std::collections::{HashMap, HashSet};

use axum::{
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info};

use crate::{
    cmd_flow::{input::ParsedInputCmd, router::Target, FinishedCmd, NullFormatter},
    state::{GroupId, GroupMeta, SessionId},
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

#[derive(Deserialize)]
struct SourceResolver {
    src: String,
}

#[derive(Serialize)]
struct GroupIdsResponse {
    grp_ids: Vec<String>,
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

    pub async fn run(&self) -> Result<(), std::io::Error> {
        let app = Router::new()
            .route("/", get(root_handler))
            .route("/status", get(get_status))
            .route("/sessions", get(get_sessions))
            .route("/pcommands", get(get_pending_commands))
            .route("/src_to_grp_ids", get(resolve_src_to_group_ids))
            .route("/src_to_grps", get(resolve_src_to_groups))
            .route("/send", post(send_cmd))
            .route("/groups", get(get_groups));

        let listener = tokio::net::TcpListener::bind(self.addr.clone())
            .await
            .unwrap();
        info!("API Server listening on {}", listener.local_addr().unwrap());
        axum::serve(listener, app).await.unwrap();
        Ok(())
    }
}

impl Default for ApiServer {
    fn default() -> Self {
        ApiServer::new("localhost:5000")
    }
}

// Root handler
async fn root_handler() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "Welcome to the Axum API server!".to_string(),
    })
}

async fn resolve_src_to_group_ids(Query(src): Query<SourceResolver>) -> impl IntoResponse {
    let src = src.src;
    #[cfg(feature = "lazy_source_map")]
    let grp_ids = crate::state::get_source_mgr()
        .resolve_src_to_group_ids(&src)
        .await;

    #[cfg(not(feature = "lazy_source_map"))]
    let grp_ids = crate::state::get_source_mgr().resolve_src_to_group_ids(&src);

    let grp_ids = grp_ids
        .unwrap_or_default()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(GroupIdsResponse { grp_ids: grp_ids }))
}

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

async fn send_cmd(Json(send_cmd): Json<SendCommand>) -> impl IntoResponse {
    debug!("Received command: {:?}", send_cmd);
    let query = send_cmd.clone();
    if let Ok(cmd) = send_cmd.cmd.try_into() as Result<ParsedInputCmd> {
        let (target, cmd) = cmd.to_command(NullFormatter);
        // if the user specifies a target, it overrides the one in the command
        let target = send_cmd.target.unwrap_or(target);
        debug!(
            "Sending command: {:?} to {:?}. query: {:?}",
            cmd, target, query
        );
        if send_cmd.wait {
            match crate::cmd_flow::get_router().send_to_ret(target, cmd).await {
                Ok(r) => {
                    return (
                        StatusCode::OK,
                        Json(SendCommandResponse {
                            message: "success".to_string(),
                            success: true,
                            payload: Some(r),
                        }),
                    );
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(SendCommandResponse {
                            message: format!("Failed to send command: {}", e),
                            success: false,
                            payload: None,
                        }),
                    );
                }
            }
        } else {
            crate::cmd_flow::get_router().send_to(target, cmd);
            return (
                StatusCode::OK,
                Json(SendCommandResponse {
                    message: "success".to_string(),
                    success: true,
                    payload: None,
                }),
            );
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(SendCommandResponse {
                message: "Invalid command".to_string(),
                success: false,
                payload: None,
            }),
        );
    }
}

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

async fn get_sessions() -> impl IntoResponse {
    // if it has performance issues, we can probably parallelize this
    // or maybe do it in parallel conditionally when the size
    // is above a certain threshold
    let mut results = vec![];
    let ss = crate::state::STATES.get_all_sessions();
    for s in ss {
        let s_meta = s.read().await;
        let service_meta = s_meta.service_meta.as_ref();
        let sid = s_meta.sid;
        let tag = s_meta.tag.clone();
        let status: &str = s_meta.status.clone().into();
        let session = json!({
            "sid": sid,
            "tag": tag,
            "alias": service_meta.map(|x| x.alias.clone()).unwrap_or("UNKNOWN".to_string()),
            "status": status,
            "groupId": crate::state::get_group_mgr().get_group_id(sid).unwrap_or("UNKNOWN".to_string()),
        });
        results.push(session);
    }

    (StatusCode::OK, Json(json!(results)))
}

async fn get_pending_commands() -> impl IntoResponse {
    let mut results = vec![];
    let cmds = crate::cmd_flow::get_cmd_tracker().get_inflight_cmds_copy();
    for c in cmds {
        let e_token = c.ext_id;
        let i_token = c.id;
        let cmd = json!({
            "token": e_token.map(|x| x.to_string()).unwrap_or("".to_string()),
            "internal_token": i_token,
            "target_sessions": c.target_num_resp,
            "finished_sessions": c.received_num_resp,
        });
        results.push(cmd);
    }
    (StatusCode::OK, Json(json!(results)))
}
async fn get_groups() -> impl IntoResponse {
    // This assumes you have a function to get the global GroupMgr instance,
    // similar to `get_source_mgr()` or `get_cmd_tracker()` in your code.
    // You might need to adjust this line to match your actual state management.
    let group_mgr = crate::state::get_group_mgr();

    // Retrieve all groups from the GroupMgr.
    let all_groups = group_mgr.get_all_groups_if(|_id, _meta| true);

    // Transform the data into the desired format: { group_id: sessions }
    let result: HashMap<GroupId, HashSet<SessionId>> = all_groups
        .into_iter()
        .map(|(id, meta)| (id, meta.get_sids().clone()))
        .collect();

    (StatusCode::OK, Json(result))
}
// async fn get_finished_commands() -> impl IntoResponse {

// }
