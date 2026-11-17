//! Agent 运行时的环境变量配置。
//!
//! 全部通过 `HAMR_AGENT_*` 前缀的环境变量设置，**默认关闭** —— 与本仓库
//! 其他组件（hamr-browser 鉴权 fail-closed、Grafana 弱口令 fail-closed）
//! 保持同一个安全取向：没配置好就当作没这个功能，而不是「默认开启再祈祷」。

use std::time::Duration;

/// 模型来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// 本机模型（Ollama / LM Studio / 任意 OpenAI 兼容本地端点）。
    /// 数据不出设备，是默认路径。
    Local,
    /// 云端模型（任意 OpenAI 兼容 API）。需要显式开启 + API Key，
    /// 且默认对家庭 PII 做脱敏。
    Cloud,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Local => "local",
            ProviderKind::Cloud => "cloud",
        }
    }

    fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "local" | "ollama" | "" => Ok(ProviderKind::Local),
            "cloud" | "remote" => Ok(ProviderKind::Cloud),
            other => anyhow::bail!("invalid HAMR_AGENT_PROVIDER: {}", other),
        }
    }
}

#[derive(Clone)]
pub struct AgentConfig {
    /// 总开关。false 时所有 Agent 端点返回 503。
    pub enabled: bool,
    /// 模型来源。
    pub provider: ProviderKind,
    /// OpenAI 兼容 API 的 base URL（不带尾斜杠）。
    pub base_url: String,
    /// 模型名。
    pub model: String,
    /// API Key。仅云端需要；本地端点一般不需要鉴权。
    pub api_key: Option<String>,
    /// 是否允许把原始 PII 发给模型。
    ///
    /// 默认 `false`。**注意**：本地 provider 下这个开关无意义 ——
    /// 数据根本没出设备，所以本地路径永远不做脱敏（脱敏会破坏 Agent 的判断力，
    /// 比如「张伟的生日快到了」变成「成员B的生日快到了」就没法给出建议了）。
    pub allow_pii: bool,
    /// 单轮对话内最多允许的「思考 → 调工具」循环次数，防止跑飞。
    pub max_steps: u32,
    /// 单次模型调用超时。
    pub timeout: Duration,
    /// 回灌给模型的历史消息条数上限。
    pub max_history: i64,
    /// JiaBu 决策引擎地址。配置后才会向 Agent 暴露 `jiabu_score` 工具。
    pub jiabu_url: Option<String>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // api_key 是凭据：绝不能因为一句 `{:?}` / `unwrap_err()` 就被打进日志。
        f.debug_struct("AgentConfig")
            .field("enabled", &self.enabled)
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("allow_pii", &self.allow_pii)
            .field("max_steps", &self.max_steps)
            .field("timeout", &self.timeout)
            .field("max_history", &self.max_history)
            .field("jiabu_url", &self.jiabu_url)
            .finish()
    }
}

impl AgentConfig {
    /// 一个「明确关闭」的配置 —— 配置解析失败时的安全兜底。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            provider: ProviderKind::Local,
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            allow_pii: false,
            max_steps: 8,
            timeout: Duration::from_secs(60),
            max_history: 20,
            jiabu_url: None,
        }
    }

    /// 从环境变量读取配置。
    ///
    /// 校验失败一律 `bail!` —— 让 `main` 降级为「Agent 不可用」，
    /// 而不是带着半截配置跑起来。
    pub fn from_env() -> anyhow::Result<Self> {
        let enabled = env_bool("HAMR_AGENT_ENABLED", false)?;
        let provider = ProviderKind::parse(&env_str("HAMR_AGENT_PROVIDER", "local"))?;

        // 本地默认指向 Ollama 的 OpenAI 兼容端点；云端默认留空由用户显式指定。
        let default_base_url = match provider {
            ProviderKind::Local => "http://127.0.0.1:11434/v1",
            ProviderKind::Cloud => "",
        };
        let base_url = env_str("HAMR_AGENT_BASE_URL", default_base_url)
            .trim()
            .to_string();
        let base_url = base_url.trim_end_matches('/').to_string();

        let default_model = match provider {
            ProviderKind::Local => "qwen2.5:7b",
            ProviderKind::Cloud => "gpt-4o-mini",
        };
        let model = env_str("HAMR_AGENT_MODEL", default_model);

        let api_key = std::env::var("HAMR_AGENT_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());

        // 云端缺 API Key 直接拒绝配置 —— 空 Key 请求云端只会得到 401，
        // 不如启动时就报清楚。
        if provider == ProviderKind::Cloud && api_key.is_none() {
            anyhow::bail!("HAMR_AGENT_PROVIDER=cloud requires HAMR_AGENT_API_KEY");
        }
        if provider == ProviderKind::Cloud && base_url.is_empty() {
            anyhow::bail!("HAMR_AGENT_PROVIDER=cloud requires HAMR_AGENT_BASE_URL");
        }
        if provider == ProviderKind::Local && base_url.is_empty() {
            anyhow::bail!("HAMR_AGENT_PROVIDER=local requires HAMR_AGENT_BASE_URL");
        }

        let allow_pii = env_bool("HAMR_AGENT_ALLOW_PII", false)?;
        let max_steps = env_int("HAMR_AGENT_MAX_STEPS", 8)?.clamp(1, 32) as u32;
        let timeout_secs = env_int("HAMR_AGENT_TIMEOUT_SECS", 60)?.clamp(5, 600) as u64;
        let max_history = env_int("HAMR_AGENT_MAX_HISTORY", 20)?.clamp(0, 200);

        let jiabu_url = std::env::var("HAMR_JIABU_URL")
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());

        Ok(Self {
            enabled,
            provider,
            base_url,
            model,
            api_key,
            allow_pii,
            max_steps,
            timeout: Duration::from_secs(timeout_secs),
            max_history,
            jiabu_url,
        })
    }

    /// 当前配置下是否需要对外发数据 —— 决定要不要走脱敏。
    pub fn needs_redaction(&self) -> bool {
        self.provider == ProviderKind::Cloud && !self.allow_pii
    }
}

fn env_str(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}

fn env_bool(key: &str, default: bool) -> anyhow::Result<bool> {
    match std::env::var(key) {
        Ok(v) => match v.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" | "" => Ok(false),
            other => anyhow::bail!("invalid boolean for {}: {}", key, other),
        },
        Err(_) => Ok(default),
    }
}

fn env_int(key: &str, default: i64) -> anyhow::Result<i64> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid integer for {}: {}", key, v)),
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env 是进程级共享状态，而 `cargo test` 默认多线程并发跑：
    /// 一个测试的 `remove_var` 会把邻居刚 `set_var` 的值抹掉。
    /// 所有碰 env 的测试统一先抢这把锁，串行执行。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn clear_env() {
        for k in [
            "HAMR_AGENT_ENABLED",
            "HAMR_AGENT_PROVIDER",
            "HAMR_AGENT_BASE_URL",
            "HAMR_AGENT_MODEL",
            "HAMR_AGENT_API_KEY",
            "HAMR_AGENT_ALLOW_PII",
            "HAMR_AGENT_MAX_STEPS",
            "HAMR_AGENT_TIMEOUT_SECS",
            "HAMR_AGENT_MAX_HISTORY",
            "HAMR_JIABU_URL",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn disabled_by_default() {
        let _env = env_guard();
        clear_env();
        let cfg = AgentConfig::from_env().expect("default agent config");
        assert!(!cfg.enabled, "Agent 必须默认关闭");
        assert_eq!(cfg.provider, ProviderKind::Local);
        assert_eq!(cfg.base_url, "http://127.0.0.1:11434/v1");
        assert!(!cfg.allow_pii);
        assert!(!cfg.needs_redaction(), "本地 provider 不需要脱敏");
    }

    #[test]
    fn parses_enabled_and_local_provider() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_ENABLED", "true");
        std::env::set_var("HAMR_AGENT_PROVIDER", "local");
        std::env::set_var("HAMR_AGENT_BASE_URL", "http://127.0.0.1:1234/v1/");
        let cfg = AgentConfig::from_env().expect("local config");
        assert!(cfg.enabled);
        assert_eq!(cfg.base_url, "http://127.0.0.1:1234/v1", "尾斜杠应被去掉");
    }

    #[test]
    fn cloud_requires_api_key() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_PROVIDER", "cloud");
        std::env::set_var("HAMR_AGENT_BASE_URL", "https://api.example.com/v1");
        let err = AgentConfig::from_env().unwrap_err().to_string();
        assert!(err.contains("HAMR_AGENT_API_KEY"), "msg was: {err}");
    }

    #[test]
    fn cloud_needs_redaction_unless_pii_allowed() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_PROVIDER", "cloud");
        std::env::set_var("HAMR_AGENT_BASE_URL", "https://api.example.com/v1");
        std::env::set_var("HAMR_AGENT_API_KEY", "sk-test");
        let cfg = AgentConfig::from_env().expect("cloud config");
        assert!(cfg.needs_redaction(), "云端默认必须脱敏");

        std::env::set_var("HAMR_AGENT_ALLOW_PII", "true");
        let cfg = AgentConfig::from_env().expect("cloud config with pii");
        assert!(!cfg.needs_redaction(), "显式放开后才不脱敏");
    }

    #[test]
    fn rejects_invalid_provider() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_PROVIDER", "gpt");
        let err = AgentConfig::from_env().unwrap_err().to_string();
        assert!(
            err.contains("invalid HAMR_AGENT_PROVIDER"),
            "msg was: {err}"
        );
    }

    #[test]
    fn rejects_invalid_boolean() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_ENABLED", "maybe");
        let err = AgentConfig::from_env().unwrap_err().to_string();
        assert!(err.contains("invalid boolean"), "msg was: {err}");
    }

    #[test]
    fn clamps_step_and_timeout_bounds() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_MAX_STEPS", "9999");
        std::env::set_var("HAMR_AGENT_TIMEOUT_SECS", "1");
        let cfg = AgentConfig::from_env().expect("clamped config");
        assert_eq!(cfg.max_steps, 32, "步数上限应被夹住");
        assert_eq!(cfg.timeout.as_secs(), 5, "超时下限应被夹住");
    }

    #[test]
    fn debug_never_leaks_api_key() {
        let _env = env_guard();
        clear_env();
        std::env::set_var("HAMR_AGENT_PROVIDER", "cloud");
        std::env::set_var("HAMR_AGENT_BASE_URL", "https://api.example.com/v1");
        std::env::set_var("HAMR_AGENT_API_KEY", "sk-super-secret-value");
        let cfg = AgentConfig::from_env().expect("cloud config");
        let dumped = format!("{:?}", cfg);
        assert!(
            !dumped.contains("sk-super-secret-value"),
            "API Key 绝不能出现在 Debug 输出里: {dumped}"
        );
        assert!(dumped.contains("<redacted>"));
    }

    #[test]
    fn jiabu_url_is_optional() {
        let _env = env_guard();
        clear_env();
        let cfg = AgentConfig::from_env().expect("config");
        assert!(cfg.jiabu_url.is_none());

        std::env::set_var("HAMR_JIABU_URL", "http://jiabu:3003/");
        let cfg = AgentConfig::from_env().expect("config with jiabu");
        assert_eq!(cfg.jiabu_url.as_deref(), Some("http://jiabu:3003"));
    }
}
