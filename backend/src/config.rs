#[cfg(test)]
mod tests {
    use super::*;

    /// env 是进程级共享状态，而 `cargo test` 默认多线程并发跑：
    /// 一个测试的 `clear_env()` 会把邻居刚 `set_var` 的值抹掉（round-1 里
    /// parses_did_auth_mode / cors_origins_parses_and_trims 就是这么 flaky 失败的）。
    /// 所有碰 env 的测试统一先抢这把锁，串行执行。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 取 env 锁；某个测试 panic 导致锁中毒时仍要能继续（否则失败会级联）
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 清除所有相关 env 变量，避免测试间互相污染
    fn clear_env() {
        for k in [
            "HAMR_DATA_DIR",
            "HAMR_APP_AUTH_MODE",
            "HAMR_APP_SHARED_SECRET",
            "HAMR_APP_ALLOWED_ORIGINS",
            "DATABASE_URL",
            "PORT",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn default_auth_mode_is_open() {
        let _env = env_guard();
        clear_env();
        let cfg = Config::from_env().expect("default config");
        assert_eq!(cfg.auth_mode, AuthMode::Open);
        // 默认 CORS allowlist 至少包含前端默认端口
        assert!(cfg.allowed_origins.iter().any(|o| o == "http://localhost:3010"));
    }

    #[test]
    fn parses_did_auth_mode() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_APP_AUTH_MODE", "did");
        let cfg = Config::from_env().expect("did config");
        assert_eq!(cfg.auth_mode, AuthMode::Did);
    }

    #[test]
    fn parses_shared_secret_aliases() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_APP_AUTH_MODE", "shared_secret");
        std::env::set_var("HAMR_APP_SHARED_SECRET", "topsecret");
        let cfg = Config::from_env().expect("shared-secret config");
        assert_eq!(cfg.auth_mode, AuthMode::SharedSecret);
        assert_eq!(cfg.shared_secret.as_deref(), Some("topsecret"));

        clear_env();
        std::env::set_var("HAMR_APP_AUTH_MODE", "shared-secret");
        std::env::set_var("HAMR_APP_SHARED_SECRET", "topsecret");
        let cfg = Config::from_env().expect("shared-secret config");
        assert_eq!(cfg.auth_mode, AuthMode::SharedSecret);
    }

    #[test]
    fn shared_secret_requires_value() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_APP_AUTH_MODE", "shared-secret");
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("HAMR_APP_SHARED_SECRET"), "msg was: {err}");
    }

    #[test]
    fn invalid_auth_mode_bails() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_APP_AUTH_MODE", "oauth");
        let err = Config::from_env().unwrap_err().to_string();
        assert!(err.contains("invalid HAMR_APP_AUTH_MODE"), "msg was: {err}");
    }

    #[test]
    fn cors_origins_parses_and_trims() {
        let _env = env_guard();
        clear_env();
        std::env::set_var(
            "HAMR_APP_ALLOWED_ORIGINS",
            "http://a.test, http://b.test ,,http://c.test",
        );
        let cfg = Config::from_env().expect("cors config");
        assert_eq!(
            cfg.allowed_origins,
            vec![
                "http://a.test".to_string(),
                "http://b.test".to_string(),
                "http://c.test".to_string(),
            ]
        );
    }

    #[test]
    fn defaults_database_url_to_postgres() {
        let _env = env_guard();
        clear_env();
        let cfg = Config::from_env().expect("default config");
        assert!(cfg.database_url.starts_with("postgresql://"), "got: {}", cfg.database_url);
    }
}

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
    /// CORS 允许的 Origin 列表（精确匹配）
    pub allowed_origins: Vec<String>,
}

/// 手写 Debug：`shared_secret` 是凭据，绝不能因为一句 `{:?}` / `unwrap_err()`
/// 就被打进日志，这里只暴露"有没有配"。
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &self.database_url)
            .field("port", &self.port)
            .field("data_dir", &self.data_dir)
            .field("auth_mode", &self.auth_mode)
            .field(
                "shared_secret",
                &self.shared_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
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

        // CORS allowlist: 逗号分隔；为空时回退到安全的本地默认（开发端口）
        let allowed_origins: Vec<String> = std::env::var("HAMR_APP_ALLOWED_ORIGINS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "http://localhost:3010".to_string(),
                    "http://localhost:3000".to_string(),
                    "http://127.0.0.1:3010".to_string(),
                    "http://127.0.0.1:3000".to_string(),
                ]
            });

        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgresql://hamr:changeme@localhost:5432/hamr_app".to_string()),
            port: std::env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3002),
            data_dir,
            auth_mode,
            shared_secret,
            allowed_origins,
        })
    }
}
