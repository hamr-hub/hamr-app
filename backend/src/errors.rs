use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Forbidden")]
    Forbidden,
    #[error("Not found")]
    NotFound,
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::ValidationError(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            AppError::Database(e) => {
                tracing::error!("DB error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
            AppError::Internal(e) => {
                tracing::error!("Internal error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// P2P 同步错误 → HTTP 语义
///
/// 数据/契约问题是调用方的错（422），落库失败是我们的错（500）。
impl From<crate::p2p::SyncError> for AppError {
    fn from(e: crate::p2p::SyncError) -> Self {
        use crate::p2p::SyncError;
        let msg = e.to_string();
        match e {
            // 表名不在白名单 / 记录本身不合法 —— 可预期的坏输入
            SyncError::UnsupportedTable(_) | SyncError::InvalidRecord(_) => {
                AppError::ValidationError(msg)
            }
            // 落库失败 —— 细节已在 p2p 层 error! 过，对外只报 500
            SyncError::Store(_) => AppError::Internal(anyhow::anyhow!(msg)),
        }
    }
}
