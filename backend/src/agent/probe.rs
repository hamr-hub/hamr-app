//! 模型体检 —— 判断「这个模型到底会不会用工具」。
//!
//! PROJ-019 风险表第一条：「本地小模型工具调用能力弱（7B 级经常不调工具或调错）」。
//! 这个风险一直缺一个可量化的答案 —— 现在有了。
//!
//! 体检跑两道题：
//!
//! 1. **会不会调工具**：问一个必须查数据才能回答的问题，工具只给一个（list_events）。
//!    模型直接答、或调错工具名，判定不通过。
//! 2. **会不会用结果**：把工具结果喂回去，看最终答案有没有用上工具返回的内容
//!    —— 这道题专门抓「调了工具但无视结果、自己编一个」的情况。
//!
//! 体检**不碰真实数据**：工具结果是合成的（含一个特征词），
//! 既不会泄漏家庭信息，也不会写入任何表。
//!
//! 体检结果可以给运维两个可执行结论：换更大的模型，还是开云端。

use std::time::Instant;

use serde::Serialize;

use super::provider::{ChatMessage, ChatRequest, LlmProvider, ToolCall};

/// 合成工具结果里的特征词 —— 体检用它判断模型有没有真的用工具结果。
const PROBE_MARKER: &str = "紫罗兰纪念日";

/// 体检用的问法。
const PROBE_QUESTION: &str = "家庭日程里有哪些安排？请查询日程，并原样列出返回的日程标题。";

/// 体检结果。
#[derive(Clone, Debug, Serialize)]
pub struct ProbeResult {
    /// 是否通过（会调工具 + 参数合法 + 用上了结果）。
    pub passed: bool,
    /// 0-3 分：会调工具 / 参数合法 / 用上了结果，各 1 分。
    pub score: u8,
    /// 结论档位：usable（可用）/ degraded（勉强）/ unsupported（不可用）。
    pub verdict: &'static str,
    /// 模型实际调用的工具名。
    pub called_tool: Option<String>,
    /// 工具参数是否合法。
    pub tool_args_valid: bool,
    /// 最终答案是否用上了工具结果。
    pub used_tool_result: bool,
    /// 两轮调用总耗时（毫秒）。
    pub latency_ms: u128,
    /// 逐项说明。
    pub notes: Vec<String>,
    /// 给运维的建议。
    pub recommendation: String,
}

/// 体检用的工具声明（只有一个，简化判定）。
fn probe_tool() -> super::provider::ToolSpec {
    super::provider::ToolSpec {
        spec_type: "function".to_string(),
        function: super::provider::FunctionDef {
            name: "list_events".to_string(),
            description: "查询家庭日程。回答涉及「什么时候、有什么安排」的问题时调用。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "days": { "type": "integer", "description": "往后看多少天，默认 30 天" }
                }
            }),
        },
    }
}

/// 校验工具参数是否合法。
fn args_valid(arguments: &str) -> bool {
    let raw = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments.trim()
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    if !v.is_object() {
        return false;
    }
    if let Some(days) = v.get("days") {
        let Some(n) = days.as_i64() else {
            return false;
        };
        if !(1..=365).contains(&n) {
            return false;
        }
    }
    true
}

/// 合成一份工具结果。
fn synthetic_tool_result() -> String {
    serde_json::json!([
        {
            "title": PROBE_MARKER,
            "start_time": "2026-12-31T18:00:00Z",
            "location": "体检专用地点"
        }
    ])
    .to_string()
}

/// 体检得分：会调工具 / 参数合法 / 用上了结果，各 1 分。
fn score_of(called: bool, args_ok: bool, used: bool) -> u8 {
    u8::from(called) + u8::from(args_ok) + u8::from(used)
}

/// 得分 → 档位。
fn verdict_of(score: u8) -> &'static str {
    match score {
        3 => "usable",
        1..=2 => "degraded",
        _ => "unsupported",
    }
}

/// 对当前 provider 跑一次体检。
pub async fn probe(provider: &dyn LlmProvider) -> ProbeResult {
    let started = Instant::now();
    let mut notes: Vec<String> = Vec::new();

    // --- 第一题：会不会调工具 ---
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("你是一个用于测试的家庭管家助手。回答前需要数据时必须调用提供的工具。"),
            ChatMessage::user(PROBE_QUESTION),
        ],
        tools: vec![probe_tool()],
        temperature: Some(0.0),
    };

    let first = match provider.chat(req).await {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("第一轮模型调用失败：{e}"));
            return ProbeResult {
                passed: false,
                score: 0,
                verdict: "unsupported",
                called_tool: None,
                tool_args_valid: false,
                used_tool_result: false,
                latency_ms: started.elapsed().as_millis(),
                notes,
                recommendation: "模型不可达 —— 先检查 base_url / 模型名 / 网络，再谈体检结论。".to_string(),
            };
        }
    };

    if first.tool_calls.is_empty() {
        notes.push("模型没有发起工具调用 —— 它可能凭印象回答了。".to_string());
        return ProbeResult {
            passed: false,
            score: 0,
            verdict: verdict_of(0),
            called_tool: None,
            tool_args_valid: false,
            used_tool_result: false,
            latency_ms: started.elapsed().as_millis(),
            notes,
            recommendation: "该模型不主动调工具 —— 换更大的模型（建议 14B+）或开启云端模型；当前配置下家庭问题会得到凭印象的回答。".to_string(),
        };
    }

    let call: &ToolCall = &first.tool_calls[0];
    let called_tool = Some(call.function.name.clone());
    notes.push(format!("模型调用了工具 {}。", call.function.name));

    if call.function.name != "list_events" {
        notes.push("调用了错误的工具名 —— 工具选择判断有问题。".to_string());
        return ProbeResult {
            passed: false,
            score: 1,
            verdict: "degraded",
            called_tool,
            tool_args_valid: false,
            used_tool_result: false,
            latency_ms: started.elapsed().as_millis(),
            notes,
            recommendation: "模型会调工具但选错工具 —— 工具描述需要写清适用场景；建议更大模型或云端模型。".to_string(),
        };
    }

    let tool_args_valid = args_valid(&call.function.arguments);
    if tool_args_valid {
        notes.push("工具参数合法。".to_string());
    } else {
        notes.push(format!("工具参数不合法：{}", call.function.arguments));
    }

    // --- 第二题：会不会用工具结果 ---
    let messages = vec![
        ChatMessage::system("你是一个用于测试的家庭管家助手。回答前需要数据时必须调用提供的工具。"),
        ChatMessage::user(PROBE_QUESTION),
        ChatMessage::Assistant {
            content: first.content.clone(),
            tool_calls: Some(first.tool_calls.clone()),
        },
        ChatMessage::Tool {
            content: synthetic_tool_result(),
            tool_call_id: call.id.clone(),
        },
    ];

    let second = match provider
        .chat(ChatRequest {
            messages,
            tools: vec![probe_tool()],
            temperature: Some(0.0),
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("第二轮模型调用失败：{e}"));
            return ProbeResult {
                passed: false,
                score: 2,
                verdict: "degraded",
                called_tool,
                tool_args_valid,
                used_tool_result: false,
                latency_ms: started.elapsed().as_millis(),
                notes,
                recommendation: "第二轮调用失败 —— 体检未完成。".to_string(),
            };
        }
    };

    let used_tool_result = second.content.contains(PROBE_MARKER);
    if used_tool_result {
        notes.push("最终答案用上了工具返回的内容。".to_string());
    } else {
        notes.push("最终答案没有体现工具返回的内容 —— 模型可能在无视工具结果自行编造。".to_string());
    }

    let passed = tool_args_valid && used_tool_result;
    let recommendation = if passed {
        "该模型具备可用的工具调用能力，可用于家庭场景。".to_string()
    } else if !tool_args_valid {
        "模型会调工具但参数不可靠 —— 建议更大模型或云端模型；本地模型可先收紧工具 schema 再复测。".to_string()
    } else {
        "模型调了工具但无视结果 —— 这类模型给出的家庭信息不可信，建议更换模型。".to_string()
    };

    ProbeResult {
        passed,
        score: score_of(true, tool_args_valid, used_tool_result),
        verdict: verdict_of(score_of(true, tool_args_valid, used_tool_result)),
        called_tool,
        tool_args_valid,
        used_tool_result,
        latency_ms: started.elapsed().as_millis(),
        notes,
        recommendation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::{ChatResponse, FunctionCall, StubProvider};

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
    async fn passes_when_model_calls_tool_and_uses_result() {
        let provider = StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "list_events", "{\"days\": 7}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("明天有紫罗兰纪念日，在体检专用地点。"),
        ]);
        let r = probe(&provider).await;
        assert!(r.passed, "notes: {:?}", r.notes);
        assert_eq!(r.called_tool.as_deref(), Some("list_events"));
        assert!(r.tool_args_valid);
        assert!(r.used_tool_result);
    }

    #[tokio::test]
    async fn fails_when_model_does_not_call_tool() {
        let provider = StubProvider::new(vec![ChatResponse::final_text("明天没什么安排。")]);
        let r = probe(&provider).await;
        assert!(!r.passed);
        assert!(r.called_tool.is_none());
        assert!(r.recommendation.contains("不主动调工具"));
    }

    #[tokio::test]
    async fn fails_when_model_calls_wrong_tool() {
        let provider = StubProvider::new(vec![ChatResponse {
            content: String::new(),
            tool_calls: vec![tool_call("c1", "list_things", "{}")],
            finish_reason: Some("tool_calls".into()),
        }]);
        let r = probe(&provider).await;
        assert!(!r.passed);
        assert!(r.recommendation.contains("选错工具"));
    }

    #[tokio::test]
    async fn fails_when_tool_arguments_invalid() {
        let provider = StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "list_events", "{\"days\": \"一周\"}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("明天有紫罗兰纪念日。"),
        ]);
        let r = probe(&provider).await;
        assert!(!r.passed);
        assert!(!r.tool_args_valid);
        assert!(r.used_tool_result, "结果用上了，但参数不合法仍不通过");
    }

    #[tokio::test]
    async fn fails_when_model_ignores_tool_result() {
        let provider = StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "list_events", "{}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("明天应该没什么事。"),
        ]);
        let r = probe(&provider).await;
        assert!(!r.passed);
        assert!(r.tool_args_valid);
        assert!(!r.used_tool_result);
        assert!(r.recommendation.contains("无视结果"));
    }

    #[tokio::test]
    async fn reports_score_and_verdict_usable() {
        let provider = StubProvider::new(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![tool_call("c1", "list_events", "{\"days\": 7}")],
                finish_reason: Some("tool_calls".into()),
            },
            ChatResponse::final_text("明天有紫罗兰纪念日。"),
        ]);
        let r = probe(&provider).await;
        assert_eq!(r.score, 3, "三项判定各占 1 分");
        assert_eq!(r.verdict, "usable");
    }

    #[tokio::test]
    async fn reports_verdict_unsupported_when_model_never_calls_tool() {
        let provider = StubProvider::new(vec![ChatResponse::final_text("明天没什么安排。")]);
        let r = probe(&provider).await;
        assert_eq!(r.score, 0);
        assert_eq!(r.verdict, "unsupported");
    }

    #[test]
    fn args_validation_rules() {
        assert!(args_valid("{}"));
        assert!(args_valid("{\"days\": 30}"));
        assert!(args_valid(""));
        assert!(!args_valid("{\"days\": 0}"));
        assert!(!args_valid("{\"days\": 9999}"));
        assert!(!args_valid("{\"days\": \"seven\"}"));
        assert!(!args_valid("not json"));
        assert!(!args_valid("[1,2]"));
    }
}
