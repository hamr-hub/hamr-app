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

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::config::AgentConfig;
use super::privacy::Redactor;
use super::proactive::{self, ScanReport, ToldItem};
use super::prompt;
use super::provider::{ChatMessage, ChatRequest, LlmProvider, ProviderError, ToolSpec};
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

    pub fn skills_dir(&self) -> &std::path::Path { &self.config.skills_dir }

    /// 取存储句柄（会话 / 记忆管理端点用）。
    pub fn store(&self) -> Arc<dyn ConversationStore> {
        self.store.clone()
    }

    /// 跑一轮对话。
    ///
    /// `member_id` 标识这次对话属于哪位家庭成员 —— 只影响提示词视角与会话归属，
    /// **不构成数据访问边界**（数据边界始终是 family_id）。
    pub async fn run(
        &self,
        family_id: Uuid,
        member_id: Option<Uuid>,
        message: &str,
        session_id: Option<Uuid>,
    ) -> Result<AgentOutcome, AgentError> {
        let message = message.trim();
        if message.is_empty() {
            return Err(AgentError::InvalidInput("message is empty".into()));
        }

        let session_id = self.store.ensure_session(family_id, member_id, session_id).await?;
        let memories = self.store.memories(family_id, 20).await?;
        let history = self
            .store
            .history(session_id, self.config.max_history)
            .await?;

        // 知识层：Wiki 摘要索引 + 场景包目录（正文按需加载，不进提示词）
        let wiki_pages = super::wiki::summary_index(&self.db, family_id, 40)
            .await
            .unwrap_or_default();
        let wiki_index = super::wiki::render_index(&wiki_pages);
        let skill_items = super::skills::load_enabled(&self.db, family_id, &self.config.skills_dir)
            .await
            .unwrap_or_default();
        let skill_catalog = super::skills::render_catalog(&skill_items);
        let member_name = match member_id {
            Some(id) => sqlx::query_scalar::<_, String>(
                "SELECT name FROM people WHERE family_id = $1 AND id = $2",
            )
            .bind(family_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .unwrap_or(None),
            None => None,
        };

        let mut messages = vec![ChatMessage::system(prompt::system_prompt(
            prompt::PromptInput {
                memories: &memories,
                wiki_index: Some(&wiki_index),
                member_name: member_name.as_deref(),
                skill_catalog: Some(&skill_catalog),
            },
        ))];
        messages.extend(history.iter().cloned());
        messages.push(ChatMessage::user(message));
        self.store
            .append(session_id, messages.last().expect("just pushed"))
            .await?;

        // 脱敏器要用当前家庭的成员名单来建立「姓名 → 别名」的一致映射。
        let redactor = self.build_redactor(family_id).await;
        let needs_redaction = self.config.needs_redaction();
        let mut wire = if needs_redaction {
            redactor.redact_messages(&messages)
        } else {
            messages
        };

        // 工具面：默认工具集 +（若历史里加载过场景包）按其白名单收敛。
        let specs = tools::tool_specs(self.config.jiabu_url.is_some(), self.config.allow_write);
        let specs = self.narrow_by_skill(family_id, &history, specs).await;
        let ctx = ToolContext {
            db: self.db.clone(),
            store: self.store.clone(),
            family_id,
            member_id,
            allow_pii: self.config.allow_pii,
            redactor: redactor.clone(),
            jiabu_url: self.config.jiabu_url.clone(),
            http: self.http.clone(),
            provider: self.config.provider,
            skills_dir: self.config.skills_dir.clone(),
            allow_write: self.config.allow_write,
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

            if resp.tool_calls.is_empty() {
                let content = resp.content.trim();
                let (reply, summary) = if content.is_empty() {
                    (prompt::FALLBACK_REPLY, "模型未返回文本，使用兜底回复")
                } else {
                    (content, "给出结论")
                };
                final_reply = reply.to_string();
                steps.push(StepTrace {
                    step,
                    tool: None,
                    summary: summary.to_string(),
                });
                break;
            }

            let mut calls = resp.tool_calls;
            let mut call_ids = HashSet::new();
            for call in &mut calls {
                // 兼容省略或复用调用 ID 的模型，每个结果都需要唯一的对应 ID。
                while call.id.trim().is_empty() || !call_ids.insert(call.id.clone()) {
                    call.id = format!("call_{}", Uuid::new_v4().simple());
                }
            }
            let mut group = Vec::with_capacity(1 + calls.len());
            group.push(ChatMessage::Assistant {
                content: resp.content,
                tool_calls: Some(calls.clone()),
            });
            for tc in calls {
                tool_calls += 1;
                // 工具失败不中断整轮 —— 把失败原因回喂给模型，它自己决定换路。
                let content =
                    match tools::dispatch(&ctx, &tc.function.name, &tc.function.arguments).await {
                        Ok(v) => v,
                        Err(e) => format!("工具执行失败：{e}"),
                    };
                group.push(tools::tool_message(&tc.id, &content));
                steps.push(StepTrace {
                    step,
                    tool: Some(tc.function.name),
                    summary: summarize(&content),
                });
            }
            self.store.append_batch(session_id, &group).await?;
            if needs_redaction {
                wire.extend(redactor.redact_messages(&group));
            } else {
                wire.extend(group);
            }
        }

        if final_reply.is_empty() {
            final_reply = prompt::FALLBACK_REPLY.to_string();
            steps.push(StepTrace {
                step: self.config.max_steps,
                tool: None,
                summary: "达到步数上限，使用兜底回复".to_string(),
            });
        }
        self.store
            .append(
                session_id,
                &ChatMessage::Assistant {
                    content: final_reply.clone(),
                    tool_calls: None,
                },
            )
            .await?;

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

    /// 按最近加载的场景包收敛工具面。
    ///
    /// 场景包声明的 `tools` 白名单是「这个场景该用哪些工具」，不是授权 ——
    /// 服务端仍然逐个鉴权。但收敛工具面能显著降低模型跑偏的概率，
    /// 也让场景包的意图真正落到行为上。
    ///
    /// 知识层工具（use_skill / search_wiki / wiki_page）始终保留 ——
    /// 否则模型加载了场景包后就再也换不了场景、查不了详情。
    async fn narrow_by_skill(
        &self,
        family_id: Uuid,
        history: &[ChatMessage],
        specs: Vec<ToolSpec>,
    ) -> Vec<ToolSpec> {
        const ALWAYS: [&str; 3] = ["use_skill", "search_wiki", "wiki_page"];

        let mut last_skill: Option<String> = None;
        for m in history {
            if let ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } = m
            {
                for c in calls {
                    if c.function.name == "use_skill" {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&c.function.arguments)
                        {
                            if let Some(sid) = v.get("skill_id").and_then(|x| x.as_str()) {
                                last_skill = Some(sid.to_string());
                            }
                        }
                    }
                }
            }
        }
        let Some(sid) = last_skill else {
            return specs;
        };
        let Ok(manifest) =
            super::skills::load_skill(&self.db, family_id, &self.config.skills_dir, &sid).await
        else {
            return specs;
        };
        if manifest.tools.is_empty() {
            return specs;
        }
        specs
            .into_iter()
            .filter(|s| {
                ALWAYS.contains(&s.function.name.as_str())
                    || super::skills::tool_allowed(&manifest, &s.function.name)
            })
            .collect()
    }

    /// 模型体检：判断当前模型会不会调工具、会不会用工具结果。
    pub async fn probe(&self) -> Result<super::probe::ProbeResult, AgentError> {
        Ok(super::probe::probe(self.provider.as_ref()).await)
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
        member_id: None,
        skills_dir: std::path::PathBuf::from("skills"),
        allow_write: false,
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
            skills_dir: std::path::PathBuf::from("skills"),
            allow_write: false,
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
        let err = rt.run(Uuid::new_v4(), None,"   ", None).await.unwrap_err();
        assert!(matches!(err, AgentError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn returns_final_answer_without_tools() {
        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text(
            "明天下午三点有家长会。",
        )]));
        let rt = runtime(provider, config());
        let out = rt
            .run(Uuid::new_v4(), None, "明天下午有事吗", None)
            .await
            .unwrap();
        assert_eq!(out.reply, "明天下午三点有家长会。");
        assert_eq!(out.tool_calls, 0);
        assert!(!out.redacted);
        assert_eq!(out.steps.len(), 1);
        assert!(out.steps[0].tool.is_none());
    }

    #[tokio::test]
    async fn sends_and_replays_multiple_tool_results_in_order() {
        let provider = Arc::new(StubProvider::new(vec![
            ChatResponse {
                content: "我来记一下".into(),
                tool_calls: vec![
                    tool_call("c1", "remember", r#"{"content":"喜欢喝茶"}"#),
                    tool_call("c2", "not_a_tool", "{}"),
                    tool_call("c3", "remember", "{"),
                ],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("  已记住。  "),
            ChatResponse::final_text("你喜欢喝茶。"),
        ]));
        let rt = runtime(provider.clone(), config());
        let family_id = Uuid::new_v4();
        let out = rt
            .run(family_id, None, "记住我喜欢喝茶", None)
            .await
            .unwrap();
        assert_eq!(out.reply, "已记住。");
        assert_eq!(out.tool_calls, 3);
        let history = rt.store.history(out.session_id, 20).await.unwrap();
        assert_eq!(history.len(), 6);
        assert!(
            matches!(history.last(), Some(ChatMessage::Assistant { content, tool_calls: None }) if content == &out.reply)
        );

        let continued = rt
            .run(family_id, None, "我喜欢什么", Some(out.session_id))
            .await
            .unwrap();
        assert_eq!(continued.session_id, out.session_id);
        assert_eq!(continued.reply, "你喜欢喝茶。");

        let requests = provider.calls.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let followup = &requests[1].messages;
        assert_eq!(followup.len(), 6);
        let ChatMessage::Assistant {
            content,
            tool_calls: Some(calls),
        } = &followup[2]
        else {
            panic!("工具结果之前必须有 assistant 工具调用消息");
        };
        assert_eq!(content, "我来记一下");
        assert_eq!(calls.len(), 3);
        for (index, (id, expected)) in [
            ("c1", r#""ok":true"#),
            ("c2", "工具执行失败：unknown tool"),
            ("c3", "工具执行失败：invalid tool arguments"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(calls[index].id, id);
            assert!(
                matches!(&followup[index + 3], ChatMessage::Tool { tool_call_id, content } if tool_call_id == id && content.contains(expected))
            );
        }
        assert_eq!(
            serde_json::to_value(&followup[1..]).unwrap(),
            serde_json::to_value(&history[..5]).unwrap()
        );
        assert_eq!(requests[2].messages.len(), 8);
        assert_eq!(
            serde_json::to_value(&requests[2].messages[1..7]).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        assert_eq!(requests[2].messages[7].text(), "我喜欢什么");
    }

    #[tokio::test]
    async fn fills_missing_and_duplicate_call_ids() {
        let provider = Arc::new(StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![
                    tool_call("c1", "not_a_tool", "{}"),
                    tool_call("", "not_a_tool", "{}"),
                    tool_call("c1", "not_a_tool", "{}"),
                ],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("工具未找到。"),
        ]));
        let rt = runtime(provider.clone(), config());
        let out = rt
            .run(Uuid::new_v4(), None, "随便问问", None)
            .await
            .unwrap();
        let history = rt.store.history(out.session_id, 20).await.unwrap();
        let requests = provider.calls.lock().unwrap();
        let group = &requests[1].messages[2..];
        let ChatMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } = &group[0]
        else {
            panic!("缺少工具调用消息");
        };
        let ids: HashSet<_> = calls.iter().map(|call| call.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert!(!ids.contains(""));
        assert_eq!(calls[0].id, "c1");
        for (call, result) in calls.iter().zip(&group[1..]) {
            assert!(
                matches!(result, ChatMessage::Tool { tool_call_id, .. } if tool_call_id == &call.id)
            );
        }
        assert_eq!(history.len(), 6);
        assert_eq!(
            serde_json::to_value(&history[1..5]).unwrap(),
            serde_json::to_value(group).unwrap()
        );
    }

    #[tokio::test]
    async fn stops_at_max_steps_with_fallback() {
        // 模型永远要调工具 —— 循环必须被 max_steps 掐断。
        let responses: Vec<ChatResponse> = (0..10)
            .map(|i| ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call(&format!("c{i}"), "not_a_tool", "{}")],
                finish_reason: Some("tool_calls".into()),
            })
            .collect();
        let provider = Arc::new(StubProvider::new(responses));
        let mut cfg = config();
        cfg.max_steps = 3;
        let rt = runtime(provider, cfg);
        let out = rt
            .run(Uuid::new_v4(), None, "无限循环", None)
            .await
            .unwrap();
        assert_eq!(out.tool_calls, 3, "工具调用次数应受 max_steps 限制");
        assert_eq!(out.reply, prompt::FALLBACK_REPLY);
        let history = rt.store.history(out.session_id, 20).await.unwrap();
        assert_eq!(history.len(), 8);
        assert!(
            matches!(history.last(), Some(ChatMessage::Assistant { content, tool_calls: None }) if content == prompt::FALLBACK_REPLY)
        );
        assert_eq!(
            history
                .iter()
                .filter(|message| message.text() == prompt::FALLBACK_REPLY)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn persists_fallback_instead_of_blank_reply() {
        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text(" \n ")]));
        let rt = runtime(provider, config());
        let out = rt.run(Uuid::new_v4(), None, "你好", None).await.unwrap();
        assert_eq!(out.reply, prompt::FALLBACK_REPLY);
        assert_eq!(out.steps.len(), 1);
        let history = rt.store.history(out.session_id, 20).await.unwrap();
        assert!(
            matches!(history.as_slice(), [ChatMessage::User { .. }, ChatMessage::Assistant { content, tool_calls: None }] if content == prompt::FALLBACK_REPLY)
        );
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
        let out = rt.run(fam, None,"记住我喜欢喝茶", None).await.unwrap();

        let provider2 = Arc::new(StubProvider::new(vec![ChatResponse::final_text("好的")]));
        let rt2 = AgentRuntime::new(config(), provider2, store.clone(), db);
        let out2 = rt2
            .run(fam, None, "我刚才说了什么", Some(out.session_id))
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
            StubProvider::new(vec![
                ChatResponse {
                    content: "检查 a@b.cn".into(),
                    tool_calls: vec![tool_call("new", "not_a_tool", "{}")],
                    finish_reason: Some("tool_calls".into()),
                },
                ChatResponse::final_text("好的"),
            ])
            .with_kind(super::super::config::ProviderKind::Cloud, true),
        );
        let mut cfg = config();
        cfg.provider = super::super::config::ProviderKind::Cloud;
        cfg.allow_pii = false;
        let rt = runtime(provider.clone(), cfg);
        let family_id = Uuid::new_v4();
        let session_id = rt
            .store
            .ensure_session(family_id, None, None)
            .await
            .unwrap();
        rt.store
            .append_batch(
                session_id,
                &[
                    ChatMessage::Assistant {
                        content: String::new(),
                        tool_calls: Some(vec![tool_call("old", "list_people", "{}")]),
                    },
                    tools::tool_message("old", r#"{"email":"a@b.cn"}"#),
                ],
            )
            .await
            .unwrap();
        let out = rt
            .run(family_id, None, "我的邮箱是 a@b.cn", Some(session_id))
            .await
            .unwrap();
        assert!(out.redacted);
        let history = rt.store.history(session_id, 20).await.unwrap();
        assert_eq!(history[1].text(), r#"{"email":"a@b.cn"}"#);
        assert_eq!(history[2].text(), "我的邮箱是 a@b.cn");
        assert_eq!(history[3].text(), "检查 a@b.cn");
        let requests = provider.calls.lock().unwrap();
        let wire = &requests[1].messages;
        assert!(
            matches!(&wire[2], ChatMessage::Tool { tool_call_id, content } if tool_call_id == "old" && content.contains("[邮箱已隐藏]"))
        );
        assert_eq!(wire[3].text(), "我的邮箱是 [邮箱已隐藏]");
        assert_eq!(wire[4].text(), "检查 [邮箱已隐藏]");
    }

    #[tokio::test]
    async fn local_provider_does_not_redact() {
        let provider = Arc::new(StubProvider::new(vec![ChatResponse::final_text("好的")]));
        let rt = runtime(provider, config());
        let out = rt.run(Uuid::new_v4(), None,"a@b.cn", None).await.unwrap();
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
