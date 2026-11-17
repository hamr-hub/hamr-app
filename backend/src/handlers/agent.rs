//! Agent 运行时的 HTTP 端点。
//!
//! 全部挂在 `auth_middleware` 之后 —— 管家数据是家庭隐私，
//! Agent 能读到的数据不比直接调 API 少，所以鉴权级别必须一致。
//!
//! Agent 未启用（`HAMR_AGENT_ENABLED` 未开启）时统一返回 503，
//! 而不是伪装成「助手暂时不可用」—— 让运维一眼看出是没开而不是挂了。

use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::agent::{AgentError, AgentRuntime, AgentStatus};
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
}

/// 取出运行时；未启用时返回 503。
fn runtime(state: &AppState) -> AppResult<&AgentRuntime> {
    state
        .agent
        .as_deref()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))
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
        .run(req.family_id, &req.message, req.session_id)
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
    let store = state
        .agent_store()
        .ok_or_else(|| AppError::AgentUnavailable("agent runtime is not enabled".into()))?;
    let id = store
        .add_memory(req.family_id, content, "manual")
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))?;
    Ok(Json(serde_json::json!({ "ok": true, "memory_id": id })))
}

/// Agent 运行状态（不含任何密钥）。
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
