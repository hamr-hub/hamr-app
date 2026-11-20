//! 系统提示词 —— 管家人格。
//!
//! 提示词不是「越长越聪明」，真正起作用的是**边界写清楚**：
//! 什么时候该调工具、什么时候该直接回答、不该替用户做什么。
//! 这一版刻意保持短 —— 本地小模型对长提示词的遵循度明显下降。
//!
//! 记忆分层注入（借鉴 Linger）：家规与偏好排在事实之前 ——
//! 「晚饭别放香菜」这类规则必须压过一条随手记下的旧事实。

use super::store::{AgentMemory, MemoryKind};

/// 构造系统提示词。
///
/// `memories` 是该家庭的长期记忆，会被分层写进提示词。这是「记住」这件事
/// 真正生效的地方 —— 记忆不是躺在表里，而是每轮都回到模型眼前。
pub fn system_prompt(memories: &[AgentMemory]) -> String {
    let mut p = String::new();
    p.push_str(
        "你是 HamR 家庭管家，服务于一个 3-5 人的家庭。你的职责是帮这个家庭把日子过得更有条理。\n\n",
    );

    p.push_str("## 你的能力\n");
    p.push_str("你可以调用工具读取家庭数据：成员档案（list_people）、日程（list_events）、待办（list_tasks）、物品资产（list_things）、居家空间（list_spaces）、家庭概览（get_dashboard）。\n");
    p.push_str("你还可以用 remember 记住家庭的长期约定与偏好（如过敏史、作息偏好），重复出现的偏好要标为 preference，明确家规标为 rule。\n\n");

    p.push_str("## 工作方式\n");
    p.push_str("1. 涉及家庭具体事实（谁、什么时候、有什么、东西在哪）时，先调工具查，再回答。不要凭印象回答。\n");
    p.push_str("2. 一次能查清就一次查清，不要反复调用同一个工具。\n");
    p.push_str("3. 工具返回空结果时，直接说明「没有记录」，不要编造。\n");
    p.push_str("4. 回答用中文，简短、直接、像管家而不像客服。先说结论，再给必要的细节。\n\n");

    p.push_str("## 不要做的事\n");
    p.push_str("1. 不要编造家庭数据里没有的信息。\n");
    p.push_str(
        "2. 不要替用户下重大决定（搬家、换工作、大额消费），可以给建议和权衡，但决定权在用户。\n",
    );
    p.push_str("3. 不要输出冗长的模板化说教。用户问具体问题就给具体答案。\n");
    p.push_str("4. 涉及健康、法律、金钱的具体建议，说明这是通用信息，具体请咨询专业人士。\n\n");

    // 分层渲染：家规 → 偏好 → 事实。顺序即优先级。
    let rules: Vec<&AgentMemory> = memories
        .iter()
        .filter(|m| m.kind == MemoryKind::Rule)
        .collect();
    let prefs: Vec<&AgentMemory> = memories
        .iter()
        .filter(|m| m.kind == MemoryKind::Preference)
        .collect();
    let facts: Vec<&AgentMemory> = memories
        .iter()
        .filter(|m| m.kind == MemoryKind::Fact)
        .collect();

    if !rules.is_empty() {
        p.push_str("## 家规（必须遵守）\n");
        for m in &rules {
            p.push_str("- ");
            p.push_str(&m.content);
            p.push('\n');
        }
        p.push('\n');
    }

    if !prefs.is_empty() {
        p.push_str("## 家庭偏好\n");
        for m in &prefs {
            p.push_str("- ");
            p.push_str(&m.content);
            p.push('\n');
        }
        p.push('\n');
    }

    if !facts.is_empty() {
        p.push_str("## 长期记忆\n");
        p.push_str("以下是这个家庭此前明确要求你记住的事实，回答时要考虑它们：\n");
        for m in &facts {
            p.push_str("- ");
            p.push_str(&m.content);
            p.push('\n');
        }
        p.push('\n');
    }

    p
}

/// 兜底回复：模型没给出结论时用。
///
/// 不返回空串 —— 一个空的 Agent 回答比一句「我需要再想想」更让人困惑。
pub const FALLBACK_REPLY: &str = "我这轮没能给出明确结论。可以换个说法，或者让我先查一下相关数据。";

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn mem(content: &str) -> AgentMemory {
        AgentMemory {
            id: Uuid::new_v4(),
            family_id: Uuid::new_v4(),
            content: content.to_string(),
            source: "agent".to_string(),
            created_at: chrono::Utc::now(),
            kind: MemoryKind::Fact,
            importance: MemoryKind::Fact.importance(),
            reinforced_count: 0,
            last_reinforced_at: None,
        }
    }

    fn mem_of_kind(content: &str, kind: MemoryKind) -> AgentMemory {
        AgentMemory {
            kind,
            importance: kind.importance(),
            ..mem(content)
        }
    }

    #[test]
    fn prompt_states_tool_usage_rules() {
        let p = system_prompt(&[]);
        assert!(p.contains("list_people"));
        assert!(p.contains("先调工具查"));
        assert!(p.contains("不要编造"));
    }

    #[test]
    fn prompt_includes_memories() {
        let p = system_prompt(&[mem("孩子对花生过敏")]);
        assert!(p.contains("长期记忆"));
        assert!(p.contains("孩子对花生过敏"));
    }

    #[test]
    fn prompt_renders_memory_layers_in_priority_order() {
        let mems = vec![
            mem_of_kind("冰箱在厨房", MemoryKind::Fact),
            mem_of_kind("晚饭别放香菜", MemoryKind::Preference),
            mem_of_kind("孩子作业期间不聊成绩", MemoryKind::Rule),
        ];
        let p = system_prompt(&mems);
        let rule_pos = p.find("家规").unwrap();
        let pref_pos = p.find("家庭偏好").unwrap();
        let fact_pos = p.find("长期记忆").unwrap();
        assert!(
            rule_pos < pref_pos && pref_pos < fact_pos,
            "家规 → 偏好 → 事实"
        );
        assert!(p.find("孩子作业期间不聊成绩").unwrap() < p.find("晚饭别放香菜").unwrap());
    }

    #[test]
    fn prompt_without_memories_has_no_memory_section() {
        let p = system_prompt(&[]);
        assert!(!p.contains("## 长期记忆"));
        assert!(!p.contains("## 家规"));
    }

    #[test]
    fn fallback_reply_is_not_empty() {
        assert!(!FALLBACK_REPLY.trim().is_empty());
    }
}
