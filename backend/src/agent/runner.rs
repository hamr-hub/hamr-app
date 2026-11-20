//! Agent 主循环。
//!
//! 一轮对话的形状：
//!
//! ```text
//! 系统提示词（含长期记忆）
//!   + 历史消息
//!   + 用户这句话
//!        ↓
//!   ┌──────────────────────────────────────────┐
//!   │ 思考 → 要调工具？                          │
//!   │   是 → 执行工具 → 把结果塞回上下文 → 再思考 │
//!   │   否 → 得到结论                            │
//!   └──────────────────────────────────────────┘
//!        ↓
//!   写回会话历史
//! ```
//!
//! 三个必须守住的性质：
//!
//! 1. **有界**：`max_steps` 限制循环次数，模型再执着也不会无限调工具。
//! 2. **脱敏发生在出网那一刻**：库里存原文，模型看到的是脱敏后的内容。
//! 3. **工具失败不中断整轮**：失败信息回喂给模型，让它自己决定怎么办。

use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::config::AgentConfig;
use super::privacy::Redactor;
use super::proactive::{self, ScanReport, ToldItem};
use super::prompt;
use super::provider::{ChatMessage, ChatRequest, LlmProvider, ProviderError};
use super::store::{ConversationStore, StoreError};
use super::tools::{self, ToolContext};

/// Agent 运行时的错误。
#[derive(Debug)]
pub enum AgentError {
    /// 请求本身不合法（空消息等）。
    InvalidInput(String),
    /// Agent 未启用。
    Disabled,
    /// 存储层错误。
    Store(StoreError),
    /// 模型调用失败。
    Provider(ProviderError),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::InvalidInput(m) => write!(f, "invalid input: {m}"),
            AgentError::Disabled => write!(f, "agent runtime is disabled"),
            AgentError::Store(e) => write!(f, "agent store error: {e}"),
            AgentError::Provider(e) => write!(f, "agent provider error: {e}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<StoreError> for AgentError {
    fn from(e: StoreError) -> Self {
        AgentError::Store(e)
    }
}

impl From<ProviderError> for AgentError {
    fn from(e: ProviderError) -> Self {
        AgentError::Provider(e)
    }
}

/// 一次工具调用的轨迹记录。
#[derive(Clone, Debug, Serialize)]
pub struct StepTrace {
    pub step: u32,
    /// 调用的工具名；给出结论的那一步为 None。
    pub tool: Option<String>,
    pub summary: String,
}

/// 一轮对话的结果。
#[derive(Clone, Debug, Serialize)]
pub struct AgentOutcome {
    pub session_id: Uuid,
    pub reply: String,
    pub steps: Vec<StepTrace>,
    /// 本轮共发起多少次工具调用。
    pub tool_calls: usize,
    /// 本轮发给模型的内容是否做了脱敏。
    pub redacted: bool,
}

/// Agent 运行状态（供 `/api/v1/agent/status` 使用，不含任何密钥）。
#[derive(Clone, Debug, Serialize)]
pub struct AgentStatus {
    pub enabled: bool,
    pub provider: &'static str,
    pub model: String,
    pub base_url: String,
    /// 发出网内容是否会被脱敏。
    pub redacts_pii: bool,
    pub max_steps: u32,
    pub jiabu_enabled: bool,
    /// 免打扰时段（本地时区，一天中的第几分钟）。
    pub quiet_start: u16,
    pub quiet_end: u16,
    /// 每日主动提醒条数上限。
    pub daily_reminder_cap: u32,
}

/// Agent 运行时。
pub struct AgentRuntime {
    provider: Arc<dyn LlmProvider>,
    config: AgentConfig,
    store: Arc<dyn ConversationStore>,
    db: sqlx::PgPool,
    http: reqwest::Client,
}

impl AgentRuntime {
    pub fn new(
        config: AgentConfig,
        provider: Arc<dyn LlmProvider>,
        store: Arc<dyn ConversationStore>,
        db: sqlx::PgPool,
    ) -> Self {
        // HTTP client 构造失败不该让服务起不来 —— 退化成一个永远报错的 client，
        // 只影响 jiabu_score 这一个可选工具。
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            provider,
            config,
            store,
            db,
            http,
        }
    }

    pub fn status(&self) -> AgentStatus {
        let info = self.provider.describe();
        AgentStatus {
            enabled: self.config.enabled,
            provider: info.kind,
            model: info.model,
            base_url: info.base_url,
            redacts_pii: info.redacts_pii,
            max_steps: self.config.max_steps,
            jiabu_enabled: self.config.jiabu_url.is_some(),
            quiet_start: self.config.quiet_start,
            quiet_end: self.config.quiet_end,
            daily_reminder_cap: self.config.daily_reminder_cap,
        }
    }

    /// 取存储句柄（会话 / 记忆管理端点用）。
    pub fn store(&self) -> Arc<dyn ConversationStore> {
        self.store.clone()
    }

    /// 跑一轮对话。
    pub async fn run(
        &self,
        family_id: Uuid,
        message: &str,
        session_id: Option<Uuid>,
    ) -> Result<AgentOutcome, AgentError> {
        let message = message.trim();
        if message.is_empty() {
            return Err(AgentError::InvalidInput("message is empty".into()));
        }

        let session_id = self.store.ensure_session(family_id, session_id).await?;
        let memories = self.store.memories(family_id, 20).await?;
        let history = self
            .store
            .history(session_id, self.config.max_history)
            .await?;

        let mut messages = vec![ChatMessage::system(prompt::system_prompt(&memories))];
        messages.extend(history);
        messages.push(ChatMessage::user(message));
        self.store
            .append(session_id, messages.last().expect("just pushed"))
            .await?;

        // 脱敏器要用当前家庭的成员名单来建立「姓名 → 别名」的一致映射。
        let redactor = self.build_redactor(family_id).await;
        let needs_redaction = self.config.needs_redaction();

        // 出网副本：云端且未放开 PII 时脱敏。库里存的仍是原文。
        let mut wire = if needs_redaction {
            redactor.redact_messages(&messages)
        } else {
            messages.clone()
        };

        let specs = tools::tool_specs(self.config.jiabu_url.is_some());
        let ctx = ToolContext {
            db: self.db.clone(),
            store: self.store.clone(),
            family_id,
            allow_pii: self.config.allow_pii,
            redactor: redactor.clone(),
            jiabu_url: self.config.jiabu_url.clone(),
            http: self.http.clone(),
            provider: self.config.provider,
        };

        let mut steps: Vec<StepTrace> = Vec::new();
        let mut tool_calls = 0usize;
        let mut final_reply = String::new();

        for step in 1..=self.config.max_steps {
            let req = ChatRequest {
                messages: wire.clone(),
                tools: specs.clone(),
                temperature: Some(0.3),
            };
            let resp = self.provider.chat(req).await?;

            let assistant = ChatMessage::Assistant {
                content: resp.content.clone(),
                tool_calls: if resp.tool_calls.is_empty() {
                    None
                } else {
                    Some(resp.tool_calls.clone())
                },
            };
            self.store.append(session_id, &assistant).await?;

            if resp.tool_calls.is_empty() {
                final_reply = resp.content.trim().to_string();
                steps.push(StepTrace {
                    step,
                    tool: None,
                    summary: "给出结论".to_string(),
                });
                break;
            }

            for tc in &resp.tool_calls {
                tool_calls += 1;
                let raw_args = if tc.function.arguments.trim().is_empty() {
                    "{}"
                } else {
                    tc.function.arguments.as_str()
                };
                // 工具失败不中断整轮 —— 把失败原因回喂给模型，它自己决定换路。
                let content = match tools::dispatch(&ctx, &tc.function.name, raw_args).await {
                    Ok(v) => v,
                    Err(e) => format!("工具执行失败：{e}"),
                };
                let wire_content = if needs_redaction {
                    redactor.redact(&content)
                } else {
                    content.clone()
                };
                wire.push(tools::tool_message(&tc.id, &wire_content));
                steps.push(StepTrace {
                    step,
                    tool: Some(tc.function.name.clone()),
                    summary: summarize(&content),
                });
            }
        }

        if final_reply.is_empty() {
            // 步数用完还没出结论 —— 给一句能用的兜底，而不是空白。
            final_reply = prompt::FALLBACK_REPLY.to_string();
            steps.push(StepTrace {
                step: self.config.max_steps,
                tool: None,
                summary: "达到步数上限，使用兜底回复".to_string(),
            });
        }

        Ok(AgentOutcome {
            session_id,
            reply: final_reply,
            steps,
            tool_calls,
            redacted: needs_redaction,
        })
    }

    /// 用家庭成员姓名建立脱敏映射。
    async fn build_redactor(&self, family_id: Uuid) -> Redactor {
        let names = sqlx::query_scalar::<_, String>(
            "SELECT name FROM people WHERE family_id = $1 ORDER BY created_at ASC",
        )
        .bind(family_id)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        Redactor::new(&names)
    }

    /// 主动扫描一轮：从五维数据取候选信号，过闸门后落库。
    ///
    /// 全程不调用模型 —— 规则判定，不出网、不涉脱敏。
    pub async fn proactive_scan(&self, family_id: Uuid) -> Result<ScanReport, AgentError> {
        let now = Utc::now();
        let report =
            proactive::scan(&self.db, self.store.as_ref(), family_id, now, &self.config).await?;
        Ok(report)
    }

    /// 放行到期提醒（免打扰结束 / 新的一天由调度方调用）。
    pub async fn proactive_flush(&self, family_id: Uuid) -> Result<Vec<ToldItem>, AgentError> {
        let now = Utc::now();
        let items = proactive::flush_due(self.store.as_ref(), family_id, now, &self.config).await?;
        Ok(items)
    }
}

/// 工具结果摘要（写进轨迹，便于人工回看这一轮 Agent 干了什么）。
fn summarize(content: &str) -> String {
    let s = content.trim().replace('\n', " ");
    if s.chars().count() <= 80 {
        s
    } else {
        let head: String = s.chars().take(80).collect();
        format!("{}…", head)
    }
}

use std::time::Duration;

/// 构造一个不连数据库的 ToolContext（供工具层单测使用）。
///
/// `connect_lazy` 不会立刻建立连接，只在真正执行查询时才尝试 —— 正好符合
/// 「测试只想验证参数校验和分发，不碰真实数据」的需求。
#[cfg(test)]
pub(crate) fn test_ctx() -> ToolContext {
    let db = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgresql://hamr:hamr@127.0.0.1:1/hamr_test")
        .expect("lazy pool");
    ToolContext {
        db,
        store: Arc::new(super::store::MemStore::new()),
        family_id: Uuid::new_v4(),
        allow_pii: false,
        redactor: Redactor::empty(),
        jiabu_url: None,
        http: reqwest::Client::new(),
        provider: super::config::ProviderKind::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::{ChatResponse, FunctionCall, StubProvider, ToolCall};
    use crate::agent::store::MemStore;

    fn config() -> AgentConfig {
        AgentConfig {
            enabled: true,
            provider: super::super::config::ProviderKind::Local,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "test".into(),
            api_key: None,
            allow_pii: false,
            max_steps: 4,
            timeout: Duration::from_secs(5),
            max_history: 20,
            jiabu_url: None,
            quiet_start: 22 * 60,
            quiet_end: 7 * 60 + 30,
            daily_reminder_cap: 5,
        }
    }

    fn runtime(provider: Arc<dyn LlmProvider>, cfg: AgentConfig) -> AgentRuntime {
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgresql://hamr:hamr@127.0.0.1:1/hamr_test")
            .unwrap();
        AgentRuntime::new(cfg, provider, Arc::new(MemStore::new()), db)
    }

    fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[tokio::test]
    async fn rejects_empty_message() {
        let rt = runtime(Arc::new(StubProvider::new(vec![])), config());
        let err = rt.run(Uuid::new_v4(), "   ", None).await.unwrap_err();
        assert!(matches!(err, AgentError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn returns_final_answer_without_tools() {
        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text(
            "明天下午三点有家长会。",
        )]));
        let rt = runtime(provider, config());
        let out = rt
            .run(Uuid::new_v4(), "明天下午有事吗", None)
            .await
            .unwrap();
        assert_eq!(out.reply, "明天下午三点有家长会。");
        assert_eq!(out.tool_calls, 0);
        assert!(!out.redacted);
        assert_eq!(out.steps.len(), 1);
        assert!(out.steps[0].tool.is_none());
    }

    #[tokio::test]
    async fn runs_tool_then_answers() {
        let provider = Arc::new(StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "list_tasks", "{}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("还有 3 件待办没完成。"),
        ]));
        let rt = runtime(provider, config());
        let out = rt
            .run(Uuid::new_v4(), "我还有什么事没做", None)
            .await
            .unwrap();
        assert_eq!(out.reply, "还有 3 件待办没完成。");
        assert_eq!(out.tool_calls, 1);
        assert_eq!(out.steps[0].tool.as_deref(), Some("list_tasks"));
        assert!(out.steps[1].tool.is_none());
    }

    #[tokio::test]
    async fn tool_failure_does_not_break_the_turn() {
        let provider = Arc::new(StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "not_a_tool", "{}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("工具那边出了点问题，我先按现有信息回答。"),
        ]));
        let rt = runtime(provider, config());
        let out = rt.run(Uuid::new_v4(), "随便问问", None).await.unwrap();
        assert!(out.reply.contains("出了点问题"));
        assert_eq!(out.tool_calls, 1);
    }

    #[tokio::test]
    async fn stops_at_max_steps_with_fallback() {
        // 模型永远要调工具 —— 循环必须被 max_steps 掐断。
        let responses: Vec<ChatResponse> = (0..10)
            .map(|i| ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call(&format!("c{i}"), "list_people", "{}")],
                finish_reason: Some("tool_calls".into()),
            })
            .collect();
        let provider = Arc::new(StubProvider::new(responses));
        let mut cfg = config();
        cfg.max_steps = 3;
        let rt = runtime(provider, cfg);
        let out = rt.run(Uuid::new_v4(), "无限循环", None).await.unwrap();
        assert_eq!(out.tool_calls, 3, "工具调用次数应受 max_steps 限制");
        assert_eq!(out.reply, prompt::FALLBACK_REPLY);
    }

    #[tokio::test]
    async fn persists_user_and_assistant_messages() {
        // 两个 runtime 必须共享同一个 store —— 否则第二轮读到的是全新的库，
        // 会话根本接不上。共享 store 才能验证「续用会话」这条链路。
        let store = Arc::new(MemStore::new());
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgresql://hamr:hamr@127.0.0.1:1/hamr_test")
            .unwrap();

        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text("好的")]));
        let rt = AgentRuntime::new(config(), provider, store.clone(), db.clone());
        let fam = Uuid::new_v4();
        let out = rt.run(fam, "记住我喜欢喝茶", None).await.unwrap();

        let provider2 = Arc::new(StubProvider::new(vec![ChatResponse::final_text("好的")]));
        let rt2 = AgentRuntime::new(config(), provider2, store.clone(), db);
        let out2 = rt2
            .run(fam, "我刚才说了什么", Some(out.session_id))
            .await
            .unwrap();
        assert_eq!(out2.session_id, out.session_id, "应续用同一个会话");

        // 第二轮的历史里应能看到第一轮的用户消息
        let hist = store.history(out.session_id, 50).await.unwrap();
        let texts: Vec<&str> = hist.iter().map(|m| m.text()).collect();
        assert!(
            texts.iter().any(|t| t.contains("记住我喜欢喝茶")),
            "历史应包含上一轮内容，实际: {texts:?}"
        );
    }

    #[tokio::test]
    async fn cloud_provider_redacts_wire_payload() {
        let provider = Arc::new(
            StubProvider::new(vec![ChatResponse::final_text("好的")])
                .with_kind(super::super::config::ProviderKind::Cloud, true),
        );
        let mut cfg = config();
        cfg.provider = super::super::config::ProviderKind::Cloud;
        cfg.allow_pii = false;
        let rt = runtime(provider, cfg);
        let out = rt
            .run(Uuid::new_v4(), "我的邮箱是 a@b.cn", None)
            .await
            .unwrap();
        assert!(out.redacted, "云端 + 未放开 PII 时应标记为脱敏");
    }

    #[tokio::test]
    async fn local_provider_does_not_redact() {
        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text("好的")]));
        let rt = runtime(provider, config());
        let out = rt.run(Uuid::new_v4(), "a@b.cn", None).await.unwrap();
        assert!(!out.redacted, "本地模型不需要脱敏");
    }

    #[tokio::test]
    async fn status_reports_config_without_secrets() {
        let provider = Arc::new(StubProvider::new(vec![]));
        let rt = runtime(provider, config());
        let s = rt.status();
        assert!(s.enabled);
        assert_eq!(s.provider, "local");
        assert!(!s.redacts_pii);
        assert!(!s.jiabu_enabled);
    }

    #[test]
    fn summarize_truncates_long_content() {
        let long = "x".repeat(200);
        let s = summarize(&long);
        assert!(
            s.chars().count() <= 82,
            "摘要应被截断，实际 {}",
            s.chars().count()
        );
        assert!(s.ends_with('…'));
    }
}
