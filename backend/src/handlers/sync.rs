/// handlers/sync.rs — P2P 入站同步 API
///
/// POST /api/v1/sync/incoming
///
/// 用途：给不方便直连 gossipsub 的对端（手机 App、Web 端、跨网段设备）一条
/// HTTP 通道，把同一批 SyncRecord 交给 `p2p::handle_incoming_sync` 走同一套
/// 幂等去重 + last-write-wins 合并逻辑 —— 两条入口共用一份判定，避免行为漂移。
///
/// 请求：
/// ```json
/// { "records": [
///     { "sync_id": "uuid", "table": "people", "primary_key": "uuid",
///       "payload": { "id": "uuid", "name": "阿妈" }, "ts_ms": 1767225600000 }
/// ] }
/// ```
///
/// 响应：
/// ```json
/// { "applied": ["uuid"],
///   "skipped": [{ "sync_id": "uuid", "reason": "duplicate_sync_id" }],
///   "counts":  { "received": 2, "applied": 1, "skipped": 1 } }
/// ```
///
/// 契约要点：LWW 是**整行替换**语义，`payload` 必须是完整行（含 `id` /
/// `family_id` / `created_at` / `updated_at` 等 NOT NULL 列）；只带部分列会被
/// Postgres 的 NOT NULL 约束挡下并返回错误 —— 宁可失败也不写半行脏数据。
///
/// 该路由挂在 `auth_middleware` 之后 —— 它是一个远程写入原语，绝不能匿名开放。
use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    db::AppState,
    errors::AppResult,
    middleware::Claims,
    p2p::{handle_incoming_sync, SyncOutcome, SyncRecord},
};

#[derive(Debug, Deserialize)]
pub struct IncomingSyncRequest {
    /// 一批待合并的记录；空数组是合法的 no-op
    #[serde(default)]
    pub records: Vec<SyncRecord>,
}

#[derive(Debug, Serialize)]
pub struct SkippedEntry {
    pub sync_id: String,
    /// duplicate_sync_id（幂等命中）或 stale_timestamp（LWW 判负）
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct IncomingSyncResponse {
    /// 真正落库的 sync_id
    pub applied: Vec<String>,
    /// 已 ack 但未写库的 sync_id + 原因
    pub skipped: Vec<SkippedEntry>,
    pub counts: serde_json::Value,
}

/// POST /api/v1/sync/incoming
///
/// 逐条处理：跳过的进 `skipped`，写成功的进 `applied`。任一条硬失败
/// （落库异常 / 非法记录 / 表名不在白名单）直接向上抛错 —— 由
/// `From<SyncError> for AppError` 映射成 500 / 422，不静默吞。
pub async fn incoming(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<IncomingSyncRequest>,
) -> AppResult<Json<IncomingSyncResponse>> {
    let received = req.records.len();
    tracing::debug!(
        "[Sync][API] {} record(s) from device={}",
        received,
        claims.device_id
    );

    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for record in &req.records {
        match handle_incoming_sync(&state.db, record).await? {
            SyncOutcome::Applied => applied.push(record.sync_id.clone()),
            other => skipped.push(SkippedEntry {
                sync_id: record.sync_id.clone(),
                reason: other.reason(),
            }),
        }
    }

    tracing::info!(
        "[Sync][API] received={} applied={} skipped={}",
        received,
        applied.len(),
        skipped.len()
    );

    Ok(Json(IncomingSyncResponse {
        counts: json!({
            "received": received,
            "applied": applied.len(),
            "skipped": skipped.len(),
        }),
        applied,
        skipped,
    }))
}
