/// handlers/p2p.rs — P2P 状态查询 API
///
/// GET /api/v1/p2p/peers  — 查看已发现的设备列表
/// GET /api/v1/p2p/status — 查看本节点状态
use axum::{extract::State, Json};
use serde_json::json;

use crate::{db::AppState, errors::AppResult};

/// GET /api/v1/p2p/peers
///
/// 返回局域网内通过 mDNS 发现的所有对端设备。
pub async fn list_peers(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    match &state.p2p_handle {
        Some(handle) => {
            let peers = handle.get_peers().await;
            Ok(Json(json!({
                "peers": peers,
                "count": peers.len(),
            })))
        }
        None => Ok(Json(json!({
            "peers": [],
            "count": 0,
            "message": "P2P node not running"
        }))),
    }
}

/// GET /api/v1/p2p/status
///
/// 返回本节点的运行状态：peer_id、监听地址、连接数等。
pub async fn get_status(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    match &state.p2p_handle {
        Some(handle) => match handle.get_status().await {
            Some(status) => Ok(Json(json!(status))),
            None => Ok(Json(json!({ "error": "Failed to get status" }))),
        },
        None => Ok(Json(json!({
            "peer_id": null,
            "running": false,
            "message": "P2P node not running"
        }))),
    }
}
