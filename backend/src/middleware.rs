use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use crate::{db::AppState, errors::AppError};

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
/// P2P 本地化架构：局域网内设备默认信任。
/// 将默认 Claims 注入 request extensions，供 handler 使用。
/// Phase2 将在此实现 DID 签名验证。
pub async fn auth_middleware(
    State(_state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    // TODO Phase2: 从 Authorization 头解析并验证 DID Bearer token
    // 当前使用默认 Claims，允许局域网内所有请求通过
    req.extensions_mut().insert(Claims::default());
    Ok(next.run(req).await)
}
