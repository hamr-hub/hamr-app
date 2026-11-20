//! Agent 运行时的 HTTP 端点。
//!
//! 全部挂在 `auth_middleware` 之后 —— 管家数据是家庭隐私，
//! Agent 能读到的数据不比直接调 API 少，所以鉴权级别必须一致。
//!
//! 鉴权成功后，Agent 子路由统一检查运行时是否可用，缺失时返回 503。

use axum::{
    extract::{Path, Query, Request, State},
    middleware::Next,
    response::Response,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::agent::proactive::{AgentReminder, ReminderStatus, ScanReport, ToldItem};
use crate::agent::{AgentError, AgentRuntime, AgentStatus, MemoryKind};
use crate::db::AppState;
use crate::errors::{AppError, AppResult};
use crate::middleware::Claims;

// ===== 请求 / 响应 =====

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub family_id: Uuid,
    pub message: String,
    /// 续用已有会话；不传则新建。
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// 当前对话属于哪位家庭成员；不传表示家庭公共会话。
    #[serde(default)]
    pub member_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: Uuid,
    pub reply: String,
    pub tool_calls: usize,
    pub redacted: bool,
    pub steps: Vec<crate::agent::StepTrace>,
}

#[derive(Debug, Deserialize)]
pub struct AddMemoryRequest {
    pub family_id: Uuid,
    pub content: String,
    /// fact / preference / rule，不填按 fact。
    #[serde(default)]
    pub kind: Option<String>,
}

/// 主动扫描请求。
#[derive(Debug, Deserialize)]
pub struct ProactiveRequest {
    pub family_id: Uuid,
}

/// 取出运行时；未启用时返回 503。
fn runtime(state: &AppState) -> AppResult<&AgentRuntime> {
    state
        .agent
        .as_deref()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))
}

pub(crate) async fn require_runtime(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> AppResult<Response> {
    runtime(&state)?;
    Ok(next.run(req).await)
}

/// 把 Agent 内部错误映射成 HTTP 语义。
fn map_err(e: AgentError) -> AppError {
    match e {
        AgentError::InvalidInput(m) => AppError::ValidationError(m),
        AgentError::Disabled => AppError::AgentUnavailable("agent runtime is disabled".into()),
        AgentError::Store(e) => AppError::Internal(anyhow::anyhow!(e.to_string())),
        AgentError::Provider(e) => AppError::AgentUnavailable(format!("model call failed: {e}")),
    }
}

// ===== 端点 =====

/// 与管家对话。
pub async fn chat(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> AppResult<Json<ChatResponse>> {
    let rt = runtime(&state)?;
    let outcome = rt
        .run(req.family_id, req.member_id, &req.message, req.session_id)
        .await
        .map_err(map_err)?;
    Ok(Json(ChatResponse {
        session_id: outcome.session_id,
        reply: outcome.reply,
        tool_calls: outcome.tool_calls,
        redacted: outcome.redacted,
        steps: outcome.steps,
    }))
}

/// 会话列表。
pub async fn list_sessions(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<crate::agent::AgentSession>>> {
    let family_id: Uuid = params
        .get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::ValidationError("family_id required".into()))?;
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let sessions = store
        .list_sessions(family_id, 50)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(sessions))
}

/// 会话消息历史。
pub async fn session_messages(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> AppResult<Json<Vec<crate::agent::provider::ChatMessage>>> {
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let msgs = store
        .history(session_id, 200)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(msgs))
}

/// 删除会话（连带消息）。
pub async fn delete_session(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let deleted = store
        .delete_session(session_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    if !deleted {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}

/// 长期记忆列表。
pub async fn list_memories(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<crate::agent::AgentMemory>>> {
    let family_id: Uuid = params
        .get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::ValidationError("family_id required".into()))?;
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let memories = store
        .memories(family_id, 100)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(memories))
}

/// 手动写入一条长期记忆。
pub async fn add_memory(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<AddMemoryRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let content = req.content.trim();
    if content.is_empty() {
        return Err(AppError::ValidationError("content is empty".into()));
    }
    let kind = match req.kind.as_deref().map(str::trim) {
        None | Some("") => MemoryKind::Fact,
        Some(raw) => MemoryKind::parse(raw).ok_or_else(|| {
            AppError::ValidationError(format!(
                "invalid kind: {raw} (expected one of fact / preference / rule)"
            ))
        })?,
    };
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    // 与模型工具走同一条写入路径：内容已存在即视为再次确认（强化），
    // 不会写出两条重复记忆。
    let m = store
        .remember(req.family_id, content, "manual", kind)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "memory_id": m.id,
        "kind": m.kind.as_str(),
        "reinforced": m.reinforced_count > 0,
        "reinforced_count": m.reinforced_count,
    })))
}

/// 主动扫描一轮：查五维数据 → 过闸门 → 落库，返回本轮要告知的提醒。
///
/// 全程不调用模型，不出网。
pub async fn proactive_scan(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ProactiveRequest>,
) -> AppResult<Json<ScanReport>> {
    let rt = runtime(&state)?;
    let report = rt.proactive_scan(req.family_id).await.map_err(map_err)?;
    Ok(Json(report))
}

/// 放行到期提醒（免打扰结束后由调度方调用）。
pub async fn proactive_flush(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ProactiveRequest>,
) -> AppResult<Json<Vec<ToldItem>>> {
    let rt = runtime(&state)?;
    let items = rt.proactive_flush(req.family_id).await.map_err(map_err)?;
    Ok(Json(items))
}

/// 提醒列表（可按状态过滤）。
pub async fn list_reminders(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<AgentReminder>>> {
    let family_id: Uuid = params
        .get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::ValidationError("family_id required".into()))?;
    let status = match params.get("status").map(|s| s.trim()) {
        None | Some("") => None,
        Some(raw) => Some(ReminderStatus::parse(raw).ok_or_else(|| {
            AppError::ValidationError(format!(
                "invalid status: {raw} (expected one of pending / told / snoozed / dismissed)"
            ))
        })?),
    };
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let reminders = store
        .reminders(family_id, status, 100)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(reminders))
}

/// 重建家庭 Wiki（从五维数据确定性派生）。
pub async fn wiki_rebuild(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ProactiveRequest>,
) -> AppResult<Json<crate::agent::wiki::RebuildReport>> {
    let report = crate::agent::wiki::rebuild(&state.db, req.family_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(report))
}

/// 家庭 Wiki 摘要索引。
pub async fn wiki_index(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<crate::agent::wiki::WikiPage>>> {
    let family_id = require_family(&params)?;
    let pages = crate::agent::wiki::summary_index(&state.db, family_id, 200)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(pages))
}

/// 家庭 Wiki 检索。
pub async fn wiki_search(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<crate::agent::wiki::WikiHit>>> {
    let family_id = require_family(&params)?;
    let query = params.get("query").cloned().unwrap_or_default();
    let hits = crate::agent::wiki::search(&state.db, family_id, &query, 50)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(hits))
}

/// 家庭 Wiki 单页详情（含来源引用）。
pub async fn wiki_page(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<serde_json::Value>> {
    let family_id = require_family(&params)?;
    let entity_type = params
        .get("entity_type")
        .cloned()
        .ok_or_else(|| AppError::ValidationError("entity_type required".into()))?;
    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| AppError::ValidationError("key required".into()))?;
    let detail = crate::agent::wiki::page_by_ref(&state.db, family_id, &entity_type, &key)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(match detail {
        Some(d) => serde_json::to_value(d).unwrap_or(serde_json::Value::Null),
        None => serde_json::json!({ "found": false }),
    }))
}

/// 家庭 Wiki 变更日志。
pub async fn wiki_changelog(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let family_id = require_family(&params)?;
    let log = crate::agent::wiki::changelog(&state.db, family_id, 100)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(log))
}

/// 已安装的场景包。
pub async fn list_skills(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let family_id = require_family(&params)?;
    let rows = crate::agent::skills::list_installed(&state.db, family_id)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(
        rows.into_iter()
            .map(|(r, enabled)| {
                serde_json::json!({
                    "skill_id": r.skill_id,
                    "version": r.version,
                    "name": r.name,
                    "description": r.description,
                    "path": r.path,
                    "tools": r.tools,
                    "enabled": enabled,
                })
            })
            .collect(),
    ))
}

/// 磁盘上可安装的场景包。
pub async fn scan_skills(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let root = runtime(&state)?.skills_dir();
    Ok(Json(
        crate::agent::skills::scan_disk(root)
            .into_iter()
            .map(|(skill_id, version, path)| {
                serde_json::json!({ "skill_id": skill_id, "version": version, "path": path })
            })
            .collect(),
    ))
}

/// 安装（登记）一个场景包。
#[derive(Debug, Deserialize)]
pub struct InstallSkillRequest {
    pub family_id: Uuid,
    pub skill_id: String,
    pub version: String,
    #[serde(default)]
    pub enabled: bool,
}

pub async fn install_skill(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<InstallSkillRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let root = runtime(&state)?.skills_dir();
    let row = crate::agent::skills::install(
        &state.db,
        req.family_id,
        root,
        &req.skill_id,
        &req.version,
        req.enabled,
    )
    .await
    .map_err(AppError::ValidationError)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "skill_id": row.skill_id,
        "version": row.version,
        "name": row.name,
    })))
}

/// 启用 / 停用场景包。
#[derive(Debug, Deserialize)]
pub struct ToggleSkillRequest {
    pub family_id: Uuid,
    pub skill_id: String,
    pub version: String,
    pub enabled: bool,
}

pub async fn toggle_skill(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ToggleSkillRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let ok = crate::agent::skills::set_enabled(
        &state.db,
        req.family_id,
        &req.skill_id,
        &req.version,
        req.enabled,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    if !ok {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 写操作列表（待确认的提议等）。
pub async fn list_write_ops(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<crate::agent::writeops::WriteOp>>> {
    let family_id = require_family(&params)?;
    let status = params.get("status").map(|s| s.trim().to_string());
    let ops = crate::agent::writeops::list_ops(&state.db, family_id, status.as_deref(), 100)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(ops))
}

/// 确认一次写操作 —— 真正落库。幂等。
#[derive(Debug, Deserialize)]
pub struct WriteOpRequest {
    pub family_id: Uuid,
}

pub async fn confirm_write_op(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
    Json(req): Json<WriteOpRequest>,
) -> AppResult<Json<crate::agent::writeops::WriteOp>> {
    // 当前服务端开关也约束已排队的提议，在读取或修改审批记录前检查。
    if !state.config.agent.allow_write {
        return Err(AppError::Forbidden);
    }
    let op = crate::agent::writeops::confirm(&state.db, req.family_id, operation_id)
        .await
        .map_err(AppError::ValidationError)?;
    Ok(Json(op))
}

/// 否决一次写操作。
pub async fn reject_write_op(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
    Json(req): Json<WriteOpRequest>,
) -> AppResult<Json<crate::agent::writeops::WriteOp>> {
    let op = crate::agent::writeops::reject(&state.db, req.family_id, operation_id)
        .await
        .map_err(AppError::ValidationError)?;
    Ok(Json(op))
}

/// 模型体检：判断当前模型会不会调工具。
pub async fn probe(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
) -> AppResult<Json<crate::agent::probe::ProbeResult>> {
    let rt = runtime(&state)?;
    let result = rt.probe().await.map_err(map_err)?;
    Ok(Json(result))
}

/// 取出 family_id 查询参数。
fn require_family(params: &HashMap<String, String>) -> AppResult<Uuid> {
    params
        .get("family_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::ValidationError("family_id required".into()))
}

/// 供 Agent 就绪检查使用。运行时可用时返回 200 和运行信息，缺失时返回 503。
/// 响应不含任何密钥。
pub async fn status(
    Extension(_claims): Extension<Claims>,
    State(state): State<AppState>,
) -> AppResult<Json<AgentStatus>> {
    let rt = runtime(&state)?;
    Ok(Json(rt.status()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_input_maps_to_422() {
        let e = map_err(AgentError::InvalidInput("message is empty".into()));
        assert!(matches!(e, AppError::ValidationError(_)));
    }

    #[test]
    fn disabled_maps_to_503() {
        let e = map_err(AgentError::Disabled);
        assert!(matches!(e, AppError::AgentUnavailable(_)));
    }

    #[test]
    fn provider_error_maps_to_503() {
        let e = map_err(AgentError::Provider(
            crate::agent::provider::ProviderError::Transport("conn refused".into()),
        ));
        assert!(matches!(e, AppError::AgentUnavailable(_)));
    }
}
