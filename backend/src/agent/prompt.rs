//! 系统提示词 —— 管家人格。
//!
//! 提示词不是「越长越聪明」，真正起作用的是**边界写清楚**：
//! 什么时候该调工具、什么时候该直接回答、不该替用户做什么。
//! 这一版刻意保持短 —— 本地小模型对长提示词的遵循度明显下降。
//!
//! 家庭版新增三块内容（对应 Linger 的知识层与场景包）：
//!
//! - **成员视角**：你现在服务的是哪位成员 —— 同一句「明天有什么」，
//!   对爸爸和对妈妈该给的信息密度不一样。
//! - **家庭 Wiki 索引**：只放一行一条的摘要索引，细节由模型按需用工具取。
//! - **场景包目录**：列出可用的场景包（名称 + 适用场景），模型判断需要时
//!   再用 `use_skill` 加载正文 —— 对应 Linger 的「索引进上下文、详情按需取」。
//!
//! 记忆分层注入（借鉴 Linger）：家规与偏好排在事实之前 ——
//! 「晚饭别放香菜」这类规则必须压过一条随手记下的旧事实。

use super::store::{AgentMemory, MemoryKind};

/// 构造系统提示词所需的输入。
#[derive(Default)]
pub struct PromptInput<'a> {
    /// 该家庭的长期记忆（已按分层排序）。
    pub memories: &'a [AgentMemory],
    /// 家庭 Wiki 摘要索引（已渲染）。
    pub wiki_index: Option<&'a str>,
    /// 当前对话对应的家庭成员姓名。
    pub member_name: Option<&'a str>,
    /// 可用场景包目录（已渲染）。
    pub skill_catalog: Option<&'a str>,
}

/// 构造系统提示词。
///
/// `memories` 是该家庭的长期记忆，会被分层写进提示词。这是「记住」这件事
/// 真正生效的地方 —— 记忆不是躺在表里，而是每轮都回到模型眼前。
pub fn system_prompt(input: PromptInput<'_>) -> String {
    let mut p = String::new();
    p.push_str(
        "你是 HamR 家庭管家，服务一个 3-5 人的家庭。你的职责是帮这个家庭把日子过得更有条理。\n\n",
    );

    // 成员视角
    if let Some(name) = input.member_name {
        p.push_str(&format!(
            "你现在服务的家庭成员是「{name}」。回答时以这位成员的视角组织内容。\n\n"
        ));
    }

    p.push_str("## 你的能力\n");
    p.push_str("你可以调用工具读取家庭数据：成员档案（list_people）、日程（list_events）、待办（list_tasks）、物品资产（list_things）、居家空间（list_spaces）、家庭概览（get_dashboard）。\n");
    p.push_str("你还可以用 remember 记住家庭的长期约定与偏好（如过敏史、作息偏好），重复出现的偏好要标为 preference，明确家规标为 rule。\n\n");

    // 知识层
    p.push_str("## 家庭知识库\n");
    p.push_str("家庭知识库（Wiki）是成员、物品、空间、日程的结构化汇总，每条事实都能追溯到来源。查「家里有哪些人/东西/房间」这类问题时先用 search_wiki，需要细节再取 wiki_page。\n");
    p.push_str("下面是这个家庭知识库的索引（一行一条，详情按需查）：\n");
    if let Some(idx) = input.wiki_index {
        if !idx.trim().is_empty() {
            p.push_str(idx);
        } else {
            p.push_str("（知识库还是空的，可以直接用上面的数据工具查。）\n");
        }
    }
    p.push('\n');

    // 场景包
    if let Some(cat) = input.skill_catalog {
        if !cat.trim().is_empty() {
            p.push_str("## 可用场景包\n");
            p.push_str("遇到相符的家庭议题时，用 use_skill 加载对应场景包，按它的指引回答：\n");
            p.push_str(cat);
            p.push('\n');
        }
    }

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
    p.push_str("4. 涉及健康、法律、金钱的具体建议，说明这是通用信息，具体请咨询专业人士。\n");
    p.push_str("5. 不要转述其他家庭成员的隐私（健康、收入、个人困扰），除非用户本人就是那位成员，或该成员已明确同意。\n");
    p.push_str("6. 涉及家庭数据的新增（待办、日程）只能提议，不能替用户决定 —— 用 propose_* 登记提议，交给用户确认。\n\n");

    // 分层渲染：家规 → 偏好 → 事实。顺序即优先级。
    let rules: Vec<&AgentMemory> = input.memories.iter().filter(|m| m.kind == MemoryKind::Rule).collect();
    let prefs: Vec<&AgentMemory> = input
        .memories
        .iter()
        .filter(|m| m.kind == MemoryKind::Preference)
        .collect();
    let facts: Vec<&AgentMemory> = input.memories.iter().filter(|m| m.kind == MemoryKind::Fact).collect();

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

    fn plain(memories: &[AgentMemory]) -> String {
        system_prompt(PromptInput {
            memories,
            ..Default::default()
        })
    }

    #[test]
    fn prompt_states_tool_usage_rules() {
        let p = plain(&[]);
        assert!(p.contains("list_people"));
        assert!(p.contains("先调工具查"));
        assert!(p.contains("不要编造"));
    }

    #[test]
    fn prompt_includes_memories() {
        let p = plain(&[mem("孩子对花生过敏")]);
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
        let p = plain(&mems);
        let rule_pos = p.find("家规").unwrap();
        let pref_pos = p.find("家庭偏好").unwrap();
        let fact_pos = p.find("长期记忆").unwrap();
        assert!(rule_pos < pref_pos && pref_pos < fact_pos, "家规 → 偏好 → 事实");
        assert!(p.find("孩子作业期间不聊成绩").unwrap() < p.find("晚饭别放香菜").unwrap());
    }

    #[test]
    fn prompt_without_memories_has_no_memory_section() {
        let p = plain(&[]);
        assert!(!p.contains("## 长期记忆"));
        assert!(!p.contains("## 家规"));
    }

    #[test]
    fn prompt_includes_member_viewpoint() {
        let p = system_prompt(PromptInput {
            member_name: Some("张伟"),
            ..Default::default()
        });
        assert!(p.contains("张伟"));
        assert!(p.contains("视角"));
    }

    #[test]
    fn prompt_includes_wiki_index_and_skill_catalog() {
        let p = system_prompt(PromptInput {
            wiki_index: Some("- [成员] 张伟\n"),
            skill_catalog: Some("- family-harmony：处理家庭关系议题\n"),
            ..Default::default()
        });
        assert!(p.contains("家庭知识库"));
        assert!(p.contains("- [成员] 张伟"));
        assert!(p.contains("use_skill"));
        assert!(p.contains("family-harmony"));
    }

    #[test]
    fn prompt_keeps_privacy_and_write_boundaries() {
        let p = plain(&[]);
        assert!(p.contains("不要转述其他家庭成员的隐私"));
        assert!(p.contains("只能提议"));
    }

    #[test]
    fn fallback_reply_is_not_empty() {
        assert!(!FALLBACK_REPLY.trim().is_empty());
    }
}
