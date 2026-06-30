use anyhow::Context;

#[derive(Clone, Debug, PartialEq)]
pub enum AuthMode {
    /// 默认 P2P 局域网模式 — 信任所有请求 (Phase1 设计)
    Open,
    /// 共享密钥模式 — 要求 Authorization: Bearer <HAMR_APP_SHARED_SECRET>
    SharedSecret,
    /// DID 签名模式 — Phase2 实现 (ed25519-dalek 已就位)
    Did,
}

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub data_dir: String,
    pub auth_mode: AuthMode,
    pub shared_secret: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        // data_dir 默认 ~/.hamr
        let data_dir = std::env::var("HAMR_DATA_DIR").unwrap_or_else(|_| {
            dirs_next::home_dir()
                .map(|h| h.join(".hamr").to_string_lossy().to_string())
                .unwrap_or_else(|| "/tmp/hamr".to_string())
        });

        // AuthMode 默认 Open (P2P LAN 信任)，可通过 env 切到 SharedSecret / Did
        let auth_mode = match std::env::var("HAMR_APP_AUTH_MODE")
            .unwrap_or_else(|_| "open".to_string())
            .to_lowercase()
            .as_str()
        {
            "open" | "" => AuthMode::Open,
            "shared-secret" | "shared_secret" => AuthMode::SharedSecret,
            "did" => AuthMode::Did,
            other => anyhow::bail!("invalid HAMR_APP_AUTH_MODE: {}", other),
        };

        let shared_secret = std::env::var("HAMR_APP_SHARED_SECRET").ok();

        if auth_mode == AuthMode::SharedSecret && shared_secret.is_none() {
            anyhow::bail!("HAMR_APP_AUTH_MODE=shared-secret requires HAMR_APP_SHARED_SECRET");
        }

        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| format!("sqlite://{}/hamr.db", data_dir)),
            port: std::env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3002),
            data_dir,
            auth_mode,
            shared_secret,
        })
    }
}
