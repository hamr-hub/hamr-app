//! LLM Provider 抽象。
//!
//! 只定义一个 trait：[`LlmProvider`]。本地模型（Ollama / LM Studio）和云端模型
//! （任意 OpenAI 兼容 API）共用同一套 OpenAI Chat Completions 协议，
//! 所以只需要一个实现 [`OpenAiCompatProvider`] —— 「可插拔」体现在
//! *换一个 base_url + model + key* 就能换模型，而不是换代码。
//!
//! 之所以不引入 `async-trait`：本仓库的 MSRV 约束到 Rust 1.82（见 Dockerfile），
//! 而 trait 里的 `async fn` 不满足 dyn 兼容。用 `BoxFuture` 手写，零额外依赖。

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::config::{AgentConfig, ProviderKind};

/// Provider 调用的异步结果。
pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChatResponse, ProviderError>> + Send + 'a>>;

/// 一条对话消息。
///
/// 用内部标签（internally tagged）枚举而不是 struct + role 字段，
/// 让「role」在 JSON 里和 OpenAI 协议完全一致，不需要额外的转换层。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default)]
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        content: String,
        tool_call_id: String,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage::User {
            content: content.into(),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            ChatMessage::System { content }
            | ChatMessage::User { content }
            | ChatMessage::Assistant { content, .. }
            | ChatMessage::Tool { content, .. } => content,
        }
    }
}

/// 模型发起的一次函数调用。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_call_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_call_type() -> String {
    "function".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 字符串（OpenAI 协议里 arguments 是字符串，不是对象）。
    #[serde(default)]
    pub arguments: String,
}

/// 工具声明（发给模型的函数定义）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type", default = "default_call_type")]
    pub spec_type: String,
    pub function: FunctionDef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    /// JSON Schema。
    pub parameters: Value,
}

/// 一次模型调用请求。
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
}

/// 一次模型调用响应。
#[derive(Clone, Debug)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

impl ChatResponse {
    /// 构造一个「直接给结论」的响应（测试与本地降级用）。
    pub fn final_text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
        }
    }
}

/// Provider 调用失败。
#[derive(Clone, Debug)]
pub enum ProviderError {
    /// 配置问题（URL 不合法等）。
    Config(String),
    /// 网络层失败（连不上、超时）。
    Transport(String),
    /// 服务端返回非 2xx。
    Http { status: u16, body: String },
    /// 响应体不符合预期。
    Decode(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Config(m) => write!(f, "provider config error: {m}"),
            ProviderError::Transport(m) => write!(f, "provider transport error: {m}"),
            ProviderError::Http { status, body } => {
                write!(f, "provider http {status}: {body}")
            }
            ProviderError::Decode(m) => write!(f, "provider decode error: {m}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// 可插拔的模型后端。
pub trait LlmProvider: Send + Sync {
    /// 发起一轮对话。
    fn chat<'a>(&'a self, req: ChatRequest) -> ProviderFuture<'a>;

    /// 自述信息（用于 `/api/v1/agent/status`，绝不含密钥）。
    fn describe(&self) -> ProviderInfo;
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderInfo {
    pub kind: &'static str,
    pub base_url: String,
    pub model: String,
    /// 是否会对出网内容做脱敏。
    pub redacts_pii: bool,
}

/// OpenAI 兼容实现。Ollama 的 `/v1/chat/completions` 与云端 API 协议一致，
/// 因此同一个实现同时服务本地与云端。
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    kind: ProviderKind,
    redacts_pii: bool,
}

impl OpenAiCompatProvider {
    pub fn new(cfg: &AgentConfig) -> Result<Self, ProviderError> {
        if cfg.base_url.is_empty() {
            return Err(ProviderError::Config("base_url is empty".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| ProviderError::Config(format!("http client build failed: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            model: cfg.model.clone(),
            api_key: cfg.api_key.clone(),
            kind: cfg.provider,
            redacts_pii: cfg.needs_redaction(),
        })
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// 组装请求体。抽出来是为了能直接对协议形状做断言。
    fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": req.messages,
            "stream": false,
        });
        if !req.tools.is_empty() {
            body["tools"] = serde_json::to_value(&req.tools).unwrap_or(Value::Null);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        body
    }

    async fn chat_impl(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = self.build_body(&req);
        let mut rb = self.client.post(self.endpoint()).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }

        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !status.is_success() {
            // 截断 body：错误页可能很大，也避免把服务端回显的请求头（含 Key）带进日志。
            let snippet: String = text.chars().take(500).collect();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: snippet,
            });
        }

        parse_chat_response(&text)
    }
}

impl LlmProvider for OpenAiCompatProvider {
    fn chat<'a>(&'a self, req: ChatRequest) -> ProviderFuture<'a> {
        Box::pin(async move { self.chat_impl(req).await })
    }

    fn describe(&self) -> ProviderInfo {
        ProviderInfo {
            kind: self.kind.as_str(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            redacts_pii: self.redacts_pii,
        }
    }
}

/// 解析 OpenAI 兼容响应。
///
/// 单独抽成纯函数：协议解析是这个模块最容易出错的地方（`content` 可能为
/// null、`tool_calls` 可能缺失、`arguments` 可能是空串），必须有独立的单测。
pub fn parse_chat_response(raw: &str) -> Result<ChatResponse, ProviderError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| ProviderError::Decode(format!("invalid json: {e}")))?;

    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| ProviderError::Decode("missing choices[0]".to_string()))?;

    let message = choice
        .get("message")
        .ok_or_else(|| ProviderError::Decode("missing choices[0].message".to_string()))?;

    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();

    let tool_calls = match message.get("tool_calls") {
        Some(Value::Array(items)) => {
            let mut calls = Vec::with_capacity(items.len());
            for it in items {
                let id = it
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let call_type = it
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("function")
                    .to_string();
                let function = it.get("function").cloned().unwrap_or(Value::Null);
                let name = function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let arguments = match function.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    // 少数服务端会把 arguments 直接给成对象，兼容一下。
                    Some(Value::Object(_)) => function.get("arguments").unwrap().to_string(),
                    _ => "{}".to_string(),
                };
                calls.push(ToolCall {
                    id,
                    call_type,
                    function: FunctionCall { name, arguments },
                });
            }
            calls
        }
        _ => Vec::new(),
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ChatResponse {
        content,
        tool_calls,
        finish_reason,
    })
}

/// 测试用 Provider：按预设脚本返回响应，用来验证 Agent 主循环。
#[cfg(test)]
pub struct StubProvider {
    responses: std::sync::Mutex<Vec<ChatResponse>>,
    /// 记录收到的请求，供断言用。
    pub calls: std::sync::Mutex<Vec<ChatRequest>>,
    kind: ProviderKind,
    redacts_pii: bool,
}

#[cfg(test)]
impl StubProvider {
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
            calls: std::sync::Mutex::new(Vec::new()),
            kind: ProviderKind::Local,
            redacts_pii: false,
        }
    }

    pub fn with_kind(mut self, kind: ProviderKind, redacts_pii: bool) -> Self {
        self.kind = kind;
        self.redacts_pii = redacts_pii;
        self
    }

    /// 取走（并清空）已收到的请求。
    pub fn take_calls(&self) -> Vec<ChatRequest> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

#[cfg(test)]
impl LlmProvider for StubProvider {
    fn chat<'a>(&'a self, req: ChatRequest) -> ProviderFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(req);
            let mut queue = self.responses.lock().unwrap();
            if queue.is_empty() {
                return Err(ProviderError::Decode("stub provider exhausted".into()));
            }
            Ok(queue.remove(0))
        })
    }

    fn describe(&self) -> ProviderInfo {
        ProviderInfo {
            kind: self.kind.as_str(),
            base_url: "stub://".to_string(),
            model: "stub".to_string(),
            redacts_pii: self.redacts_pii,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg_local() -> AgentConfig {
        AgentConfig {
            enabled: true,
            provider: ProviderKind::Local,
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            model: "qwen2.5:7b".to_string(),
            api_key: None,
            allow_pii: false,
            max_steps: 8,
            timeout: Duration::from_secs(30),
            max_history: 20,
            jiabu_url: None,
        }
    }

    #[test]
    fn endpoint_has_no_double_slash() {
        let p = OpenAiCompatProvider::new(&cfg_local()).unwrap();
        assert_eq!(p.endpoint(), "http://127.0.0.1:11434/v1/chat/completions");
    }

    #[test]
    fn build_body_includes_model_and_messages() {
        let p = OpenAiCompatProvider::new(&cfg_local()).unwrap();
        let req = ChatRequest {
            messages: vec![ChatMessage::user("你好")],
            tools: vec![],
            temperature: Some(0.2),
        };
        let body = p.build_body(&req);
        assert_eq!(body["model"], "qwen2.5:7b");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "你好");
        assert_eq!(body["stream"], false);
        // temperature 是 f32，0.2 在二进制浮点里不是精确值 —— 用容差比较，
        // 直接 assert_eq!(json!(0.2)) 会因为 0.20000000298023224 失败。
        let temp = body["temperature"].as_f64().expect("temperature number");
        assert!(
            (temp - 0.2).abs() < 1e-6,
            "temperature 应约为 0.2，实际 {temp}"
        );
        assert!(body.get("tools").is_none(), "没有工具时不应带 tools 字段");
    }

    #[test]
    fn build_body_serializes_tools() {
        let p = OpenAiCompatProvider::new(&cfg_local()).unwrap();
        let tool = ToolSpec {
            spec_type: "function".to_string(),
            function: FunctionDef {
                name: "list_people".to_string(),
                description: "列出家庭成员".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        };
        let req = ChatRequest {
            messages: vec![],
            tools: vec![tool],
            temperature: None,
        };
        let body = p.build_body(&req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "list_people");
    }

    #[test]
    fn parses_plain_text_response() {
        let raw = r#"{"choices":[{"message":{"role":"assistant","content":"孩子在楼下"},"finish_reason":"stop"}]}"#;
        let r = parse_chat_response(raw).expect("parse");
        assert_eq!(r.content, "孩子在楼下");
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parses_tool_calls() {
        let raw = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"call_1","type":"function","function":{"name":"list_tasks","arguments":"{\"status\":\"todo\"}"}}
        ]},"finish_reason":"tool_calls"}]}"#;
        let r = parse_chat_response(raw).expect("parse");
        assert_eq!(r.content, "", "content 为 null 时应退化成空串");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "call_1");
        assert_eq!(r.tool_calls[0].call_type, "function");
        assert_eq!(r.tool_calls[0].function.name, "list_tasks");
        assert!(r.tool_calls[0].function.arguments.contains("todo"));
    }

    #[test]
    fn parses_tool_call_with_object_arguments() {
        let raw = r#"{"choices":[{"message":{"tool_calls":[
            {"id":"c1","function":{"name":"list_people","arguments":{"family_id":"x"}}}
        ]}}]}"#;
        let r = parse_chat_response(raw).expect("parse");
        assert!(r.tool_calls[0].function.arguments.contains("family_id"));
    }

    #[test]
    fn rejects_missing_choices() {
        let err = parse_chat_response(r#"{"foo":1}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Decode(_)));
    }

    #[test]
    fn rejects_invalid_json() {
        let err = parse_chat_response("not json").unwrap_err();
        assert!(matches!(err, ProviderError::Decode(_)));
    }

    #[test]
    fn describe_reports_redaction_flag() {
        let p = OpenAiCompatProvider::new(&cfg_local()).unwrap();
        let info = p.describe();
        assert_eq!(info.kind, "local");
        assert!(!info.redacts_pii, "本地模型不需要脱敏");
    }
}
