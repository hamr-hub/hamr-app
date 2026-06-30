use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use crate::{config::AuthMode, db::AppState, errors::AppError};

/// 本地 P2P 设备身份声明
/// 替代 JWT —— 在 P2P 本地化应用中，Claims 由设备 DID 派生
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 设备 DID（did:key:...）
    pub sub: String,
    /// 设备标识符（简称）
    pub device_id: String,
    /// 过期时间（Unix timestamp，本地设备固定为远期）
    pub exp: i64,
}

impl Default for Claims {
    fn default() -> Self {
        Self {
            sub: "local-device".to_string(),
            device_id: "local".to_string(),
            // 2099-01-01 00:00:00 UTC
            exp: 4070908800,
        }
    }
}

/// 认证中间件
///
/// 支持三种模式（由 `HAMR_APP_AUTH_MODE` env 选择）：
/// - `open` (默认): P2P 局域网信任，注入默认 Claims。Phase1 设计。
/// - `shared-secret`: 要求 `Authorization: Bearer <HAMR_APP_SHARED_SECRET>`。
/// - `did`: Phase2 待实现 — 当前拒绝所有请求 (fail-closed)。
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    match state.config.auth_mode {
        AuthMode::Open => {
            // P2P 局域网默认信任
            req.extensions_mut().insert(Claims::default());
            Ok(next.run(req).await)
        }
        AuthMode::SharedSecret => {
            let expected = state.config.shared_secret.as_deref().unwrap_or_default();
            let token = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "));
            match token {
                Some(t) if t == expected => {
                    req.extensions_mut().insert(Claims::default());
                    Ok(next.run(req).await)
                }
                _ => {
                    warn!("hamr-app shared-secret auth failed (peer={:?})", req.headers().get("x-forwarded-for"));
                    Err(AppError::Unauthorized)
                }
            }
        }
        AuthMode::Did => {
            // Phase2: 用 ed25519-dalek 验签 did:key:... 头
            // 当前 fail-closed — 不允许任何请求直到实现完成
            warn!("hamr-app DID auth not yet implemented; rejecting request");
            Err(AppError::Unauthorized)
        }
    }
}
