use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use crate::{config::AuthMode, db::AppState, did::DeviceIdentity, errors::AppError};

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
/// - `did`: 验签三件套头 `X-DID-Public-Key` / `X-DID-Timestamp` / `X-DID-Signature`，
///   验签通过后注入 `Claims.sub = did:key:<public-key>`。
///
/// DID 模式请求格式：
/// ```text
/// canonical = "{device_did}:{timestamp}:{method}:{path_and_query}"
///             path_and_query 含 query string，例如
///             /api/v1/dashboard?family_id=... —— query 参与签名，不可篡改
/// signature = ed25519_sign(sk, canonical)
/// headers:
///   X-DID-Public-Key: <32-byte ed25519 public key, base64>
///   X-DID-Timestamp: <unix seconds>
///   X-DID-Signature: <64-byte ed25519 signature, base64>
/// ```
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
            let claims = verify_did_signature(&req)?;
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
    }
}

/// 真正的 DID 验签：抽离出来方便单元测试。
///
/// 验签流程：
/// 1. 抽取 `X-DID-Public-Key` (b64, 32B) / `X-DID-Timestamp` (str) / `X-DID-Signature` (b64, 64B)
/// 2. 构造 canonical 字符串 `device_did:timestamp:method:path`
/// 3. ed25519 验签
/// 4. 时间戳容忍窗口 ±300 秒（防止回放）
fn verify_did_signature(req: &Request) -> Result<Claims, AppError> {
    let headers = req.headers();
    let pk_b64 = headers
        .get("x-did-public-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            warn!("hamr-app DID auth: missing X-DID-Public-Key");
            AppError::Unauthorized
        })?;
    let ts_str = headers
        .get("x-did-timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            warn!("hamr-app DID auth: missing X-DID-Timestamp");
            AppError::Unauthorized
        })?;
    let sig_b64 = headers
        .get("x-did-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            warn!("hamr-app DID auth: missing X-DID-Signature");
            AppError::Unauthorized
        })?;

    use base64::Engine;
    let pk_bytes = base64::engine::general_purpose::STANDARD
        .decode(pk_b64)
        .map_err(|e| {
            warn!("hamr-app DID auth: bad public key b64: {}", e);
            AppError::Unauthorized
        })?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|e| {
            warn!("hamr-app DID auth: bad signature b64: {}", e);
            AppError::Unauthorized
        })?;
    if pk_bytes.len() != 32 {
        warn!("hamr-app DID auth: public key wrong length {}", pk_bytes.len());
        return Err(AppError::Unauthorized);
    }
    if sig_bytes.len() != 64 {
        warn!("hamr-app DID auth: signature wrong length {}", sig_bytes.len());
        return Err(AppError::Unauthorized);
    }

    let timestamp: i64 = ts_str.parse().map_err(|_| {
        warn!("hamr-app DID auth: non-numeric timestamp");
        AppError::Unauthorized
    })?;
    let now = chrono::Utc::now().timestamp();
    if (now - timestamp).abs() > 300 {
        warn!(
            "hamr-app DID auth: timestamp drift {}s (now={}, ts={})",
            now - timestamp,
            now,
            timestamp
        );
        return Err(AppError::Unauthorized);
    }

    // 派生 did:key 标识（Base58Btc pubkey 前缀 z）
    let did = {
        use multibase::Base;
        let encoded = multibase::encode(Base::Base58Btc, &pk_bytes);
        format!("did:key:{}", encoded)
    };

    let method = req.method().as_str();
    // 连 query 一起签：family_id 这类授权相关参数就在 query 里，只签 path
    // 等于允许攻击者在签名依然有效的情况下改 query（换一家人的 family_id）。
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| req.uri().path());
    let canonical = format!("{}:{}:{}:{}", did, timestamp, method, path);

    let ok = DeviceIdentity::verify(&pk_bytes, canonical.as_bytes(), &sig_bytes)
        .map_err(|e| {
            warn!("hamr-app DID auth: verify error: {}", e);
            AppError::Unauthorized
        })?;
    if !ok {
        warn!("hamr-app DID auth: signature mismatch");
        return Err(AppError::Unauthorized);
    }

    Ok(Claims {
        sub: did.clone(),
        device_id: did,
        exp: timestamp + 3600,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // STANDARD.encode / .decode 是 Engine trait 上的方法，需入作用域
    use base64::Engine as _;

    fn make_request(pk_b64: &str, ts: &str, sig_b64: &str) -> Request {
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/dashboard?family_id=00000000-0000-0000-0000-000000000000")
            .header("x-did-public-key", pk_b64)
            .header("x-did-timestamp", ts)
            .header("x-did-signature", sig_b64)
            .body(axum::body::Body::empty())
            .unwrap();
        req
    }

    #[test]
    fn did_verify_roundtrip_succeeds() {
        let identity = DeviceIdentity::generate().unwrap();
        let pk_b64 = identity.public_key_base64();
        let ts = chrono::Utc::now().timestamp().to_string();
        let did = identity.did.clone();
        let path = "/api/v1/dashboard?family_id=00000000-0000-0000-0000-000000000000";
        let canonical = format!("{}:{}:GET:{}", did, ts, path);
        let sig = identity.sign(canonical.as_bytes()).unwrap();
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&sig);

        let req = make_request(&pk_b64, &ts, &sig_b64);
        let claims = verify_did_signature(&req).expect("did verify should succeed");
        assert_eq!(claims.sub, did);
        assert!(claims.exp > chrono::Utc::now().timestamp());
    }

    #[test]
    fn did_verify_rejects_tampered_signature() {
        let identity = DeviceIdentity::generate().unwrap();
        let pk_b64 = identity.public_key_base64();
        let ts = chrono::Utc::now().timestamp().to_string();
        let bad_sig = vec![0u8; 64];
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&bad_sig);

        let req = make_request(&pk_b64, &ts, &sig_b64);
        let err = verify_did_signature(&req).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn did_verify_rejects_missing_header() {
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/dashboard")
            .body(axum::body::Body::empty())
            .unwrap();
        let err = verify_did_signature(&req).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn did_verify_rejects_stale_timestamp() {
        let identity = DeviceIdentity::generate().unwrap();
        let pk_b64 = identity.public_key_base64();
        // 1 小时前
        let ts = (chrono::Utc::now().timestamp() - 3600).to_string();
        let did = identity.did.clone();
        let path = "/api/v1/dashboard";
        let canonical = format!("{}:{}:GET:{}", did, ts, path);
        let sig = identity.sign(canonical.as_bytes()).unwrap();
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&sig);

        let req = make_request(&pk_b64, &ts, &sig_b64);
        let err = verify_did_signature(&req).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[test]
    fn did_verify_rejects_wrong_length_key() {
        let bad_pk = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 16]);
        let ts = chrono::Utc::now().timestamp().to_string();
        let sig = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 64]);
        let req = make_request(&bad_pk, &ts, &sig);
        let err = verify_did_signature(&req).unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }
}
