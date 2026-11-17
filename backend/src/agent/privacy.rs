//! PII 脱敏器。
//!
//! 接云端模型时，家庭成员姓名、电话、邮箱、证件号都不应该原样出网。
//! 但简单地把「张伟」替换成「成员1」会让 Agent 的建议失去指向性
//! （「张伟的生日快到了」→「成员1的生日快到了」，还是没法用）。
//!
//! 所以这里做的是**一致性替换**：同一份数据里的同一个人，替换成同一个别名
//! （成员A / 成员B / …），Agent 仍然能区分是谁，隐私却被保住了。
//!
//! 三条硬规则：
//!
//! 1. 只在**出网前**脱敏 —— 库里存的永远是原文，本地 Agent 看到的是原文。
//! 2. 别名替换是幂等的 —— 脱敏两次与脱敏一次结果相同。
//! 3. 脱敏不改变 JSON 结构 —— 只改文本内容，工具返回的 JSON 仍然可解析。

use regex::Regex;

/// 姓名 → 别名的一致性映射。
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    /// (原名, 别名)，按原名长度降序 —— 先替换长名，避免「张伟」被「张」抢先命中。
    aliases: Vec<(String, String)>,
}

impl Redactor {
    /// 用家庭成员姓名构建脱敏器。
    pub fn new(names: &[String]) -> Self {
        let mut unique: Vec<String> = Vec::new();
        for n in names {
            let t = n.trim();
            if t.is_empty() {
                continue;
            }
            if !unique.iter().any(|u| u == t) {
                unique.push(t.to_string());
            }
        }
        // 长名优先：否则「张伟」和「张伟伟」这种重叠会替换错。
        unique.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));

        let aliases = unique
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, format!("成员{}", alias_letter(i))))
            .collect();
        Self { aliases }
    }

    /// 空脱敏器（没有成员信息时也可用，只做通用模式脱敏）。
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            aliases: Vec::new(),
        }
    }

    /// 别名映射（供测试与调试查看）。
    #[cfg(test)]
    pub fn mappings(&self) -> &[(String, String)] {
        &self.aliases
    }

    /// 对文本脱敏。
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();

        // 1. 姓名 → 别名
        for (name, alias) in &self.aliases {
            if !name.is_empty() {
                out = out.replace(name.as_str(), alias.as_str());
            }
        }

        // 2. 通用 PII 模式
        for (re, placeholder) in patterns_ref().iter() {
            out = re.replace_all(&out, *placeholder).into_owned();
        }

        out
    }

    /// 对一批消息脱敏（保留结构）。
    pub fn redact_messages(
        &self,
        messages: &[crate::agent::provider::ChatMessage],
    ) -> Vec<crate::agent::provider::ChatMessage> {
        use crate::agent::provider::ChatMessage;
        messages
            .iter()
            .map(|m| match m {
                ChatMessage::System { content } => ChatMessage::System {
                    content: self.redact(content),
                },
                ChatMessage::User { content } => ChatMessage::User {
                    content: self.redact(content),
                },
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => ChatMessage::Assistant {
                    content: self.redact(content),
                    tool_calls: tool_calls.clone(),
                },
                ChatMessage::Tool {
                    content,
                    tool_call_id,
                } => ChatMessage::Tool {
                    content: self.redact(content),
                    tool_call_id: tool_call_id.clone(),
                },
            })
            .collect()
    }
}

/// 第 i 个成员对应的字母（0 → A，25 → Z，26 → AA）。
fn alias_letter(i: usize) -> String {
    let letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut out = Vec::new();
    let mut n = i;
    loop {
        out.push(letters.as_bytes()[(n % 26) as usize] as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out.reverse();
    out.into_iter().collect()
}

/// 通用 PII 模式。顺序有意义：先匹配更具体的（邮箱、证件号），再匹配宽泛的（长数字串）。
///
/// 银行卡/连续数字这一条要放在手机号后面 —— 否则 11 位手机号会先被当成「数字串」。
fn patterns() -> Vec<(Regex, &'static str)> {
    vec![
        (
            Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap(),
            "[邮箱已隐藏]",
        ),
        (Regex::new(r"\b\d{17}[\dXx]\b").unwrap(), "[证件号已隐藏]"),
        (Regex::new(r"\b1[3-9]\d{9}\b").unwrap(), "[电话已隐藏]"),
        (Regex::new(r"\b\d{3,4}-\d{7,8}\b").unwrap(), "[电话已隐藏]"),
        (Regex::new(r"\b\d{16,19}\b").unwrap(), "[卡号已隐藏]"),
    ]
}

/// 用 `OnceLock` 缓存编译好的正则 —— `regex::Regex` 编译不便宜，
/// 而脱敏会在每次工具结果上跑。
fn patterns_ref() -> &'static Vec<(Regex, &'static str)> {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(patterns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::ChatMessage;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn replaces_names_consistently() {
        let r = Redactor::new(&names(&["张伟", "李娜"]));
        let out = r.redact("张伟今天加班，李娜要接孩子。张伟会晚到。");
        assert!(!out.contains("张伟"));
        assert!(!out.contains("李娜"));
        assert_eq!(
            out.matches("成员A").count(),
            2,
            "同一个人应替换成同一个别名"
        );
        assert_eq!(out.matches("成员B").count(), 1);
    }

    #[test]
    fn longer_name_wins_over_shorter() {
        let r = Redactor::new(&names(&["张伟", "张伟伟"]));
        let out = r.redact("张伟伟来了，张伟在楼下");
        assert!(!out.contains("张伟伟"));
        // 张伟 必须只替换掉独立的那个，不能把「张伟伟」切成「成员A伟」
        assert_eq!(out.matches("成员").count(), 2);
    }

    #[test]
    fn masks_phone_email_and_id() {
        let r = Redactor::empty();
        let out = r.redact("电话 13812345678，邮箱 a.b@example.com，证件 11010119900307123X");
        assert!(!out.contains("13812345678"));
        assert!(!out.contains("a.b@example.com"));
        assert!(!out.contains("11010119900307123X"));
        assert!(out.contains("[电话已隐藏]"));
        assert!(out.contains("[邮箱已隐藏]"));
        assert!(out.contains("[证件号已隐藏]"));
    }

    #[test]
    fn masks_bank_card() {
        let r = Redactor::empty();
        let out = r.redact("卡号 6222021234567890123");
        assert!(!out.contains("6222021234567890123"));
        assert!(out.contains("[卡号已隐藏]"));
    }

    #[test]
    fn is_idempotent() {
        let r = Redactor::new(&names(&["张伟"]));
        let once = r.redact("张伟的手机号 13812345678");
        let twice = r.redact(&once);
        assert_eq!(once, twice, "脱敏必须幂等");
    }

    #[test]
    fn alias_letters_roll_over() {
        assert_eq!(alias_letter(0), "A");
        assert_eq!(alias_letter(25), "Z");
        assert_eq!(alias_letter(26), "AA");
    }

    #[test]
    fn redacts_messages_preserving_structure() {
        let r = Redactor::new(&names(&["张伟"]));
        let msgs = vec![
            ChatMessage::user("张伟在吗"),
            ChatMessage::Assistant {
                content: "在的".to_string(),
                tool_calls: None,
            },
            ChatMessage::Tool {
                content: "{\"name\":\"张伟\"}".to_string(),
                tool_call_id: "c1".to_string(),
            },
        ];
        let out = r.redact_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert!(!out[0].text().contains("张伟"));
        assert_eq!(out[1].text(), "在的");
        assert!(!out[2].text().contains("张伟"));
        // 结构不变：role 与 tool_call_id 原样保留
        match &out[2] {
            ChatMessage::Tool { tool_call_id, .. } => assert_eq!(tool_call_id, "c1"),
            _ => panic!("role 变了"),
        }
    }

    #[test]
    fn empty_redactor_still_masks_generic_pii() {
        let r = Redactor::new(&[]);
        let out = r.redact("邮箱 x@y.cn");
        assert!(!out.contains("x@y.cn"));
    }
}
