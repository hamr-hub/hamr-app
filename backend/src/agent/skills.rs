//! 技能包（Linger Skill 的家庭版对应物）。
//!
//! 一个技能就是一个含 `SKILL.md` 的目录，版本化且**不可变** ——
//! 发布后的版本目录不允许原地修改，只能发新版本（与 Linger 一致）。
//!
//! `SKILL.md` 结构：
//!
//! ```markdown
//! ---
//! name: 家庭关系修复
//! description: 处理家庭成员之间的沟通与关系议题，被问及关系摩擦时使用。
//! version: 1.0.0
//! tools: [list_people, list_events]
//! ---
//! # 正文（给模型看的操作指引）
//! ```
//!
//! `description` 是**给模型看的** —— 模型据此决定要不要加载这个技能，
//! 所以它必须写清「什么时候用」而不是「这是什么」。
//!
//! ## 最重要的约束：技能只引导，不授权
//!
//! Linger 的原话是「A skill guides an agent. It does not give the agent any permission.」
//! HamR 把它翻译成可执行的规则：
//!
//! - `tools` 字段声明的是**该技能期望使用的工具**，用于收敛暴露给模型的工具面；
//! - 但服务端仍对每一次工具调用独立鉴权 —— 技能表里没有的工具，模型调了也拿不到数据；
//! - 写操作仍然要走「提议 → 用户确认」，技能不能绕过审批。
//!
//! 文件在磁盘，数据库只记索引与开关（每个家庭可分别启用 / 停用）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// 技能清单。
#[derive(Clone, Debug, Serialize)]
pub struct SkillManifest {
    pub skill_id: String,
    pub version: String,
    /// 展示名（frontmatter 的 name）。
    pub name: String,
    /// 给模型看的描述 —— 决定模型要不要加载它。
    pub description: String,
    /// 该技能期望使用的工具白名单。空 = 不额外收敛。
    pub tools: Vec<String>,
    /// 正文。
    pub body: String,
}

/// 数据库里的技能索引行。
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct SkillRow {
    pub skill_id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub path: String,
    pub tools: Vec<String>,
}

/// 解析 `SKILL.md`。
///
/// 只支持 frontmatter 的一个小子集（`key: value` 与 `key: [a, b]`）——
/// 刻意不做完整 YAML 解析：技能是本项目自己产出的文件，
/// 用一个能在 20 行内讲清楚、能单测的解析器比引入依赖更划算。
pub fn parse_skill(raw: &str) -> Result<SkillManifest, String> {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---") else {
        return Err("SKILL.md must start with a YAML frontmatter block (---)".to_string());
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    let end = rest
        .find("\n---")
        .ok_or_else(|| "frontmatter block is not closed (missing ---)".to_string())?;
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();

    let mut map: HashMap<String, String> = HashMap::new();
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        map.insert(k.trim().to_lowercase(), v.trim().to_string());
    }

    let name = map
        .get("name")
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| "frontmatter must provide a non-empty `name`".to_string())?;
    let description = map.get("description").cloned().unwrap_or_default();
    let version = map.get("version").cloned().unwrap_or_else(|| "0.0.0".to_string());

    let tools = map
        .get("tools")
        .map(|v| parse_list(v))
        .unwrap_or_default();

    Ok(SkillManifest {
        skill_id: map.get("id").cloned().unwrap_or_default(),
        version,
        name,
        description,
        tools,
        body,
    })
}

/// 解析工具白名单：`[a, b]` 或 `a, b`。
fn parse_list(raw: &str) -> Vec<String> {
    let inner = raw.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 技能的磁盘根目录下的相对路径。
fn skill_file(root: &Path, path: &str) -> PathBuf {
    root.join(path).join("SKILL.md")
}

/// 从磁盘读取并解析一个技能文件。
pub fn read_skill_file(root: &Path, path: &str) -> Result<SkillManifest, String> {
    let file = skill_file(root, path);
    let raw = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read skill file {}: {e}", file.display()))?;
    let mut m = parse_skill(&raw)?;
    if m.skill_id.is_empty() {
        // 没写 id 就用目录名（<skill_id>/<version>）
        m.skill_id = path
            .split('/')
            .next()
            .unwrap_or(path)
            .to_string();
    }
    Ok(m)
}

/// 扫描磁盘上所有可用技能（`<root>/<skill_id>/<version>/SKILL.md`）。
pub fn scan_disk(root: &Path) -> Vec<(String, String, String)> {
    // (skill_id, version, relative path)
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }
        let Some(skill_id) = skill_dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(versions) = std::fs::read_dir(&skill_dir) else {
            continue;
        };
        for v in versions.flatten() {
            let version_dir = v.path();
            if !version_dir.is_dir() {
                continue;
            }
            if !version_dir.join("SKILL.md").exists() {
                continue;
            }
            let Some(version) = version_dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            out.push((
                skill_id.to_string(),
                version.to_string(),
                format!("{skill_id}/{version}"),
            ));
        }
    }
    out.sort();
    out
}

/// 技能包里是否带了可执行脚本目录。
///
/// v1 不允许 —— 与「不执行模型可控代码」的不变量冲突。
fn has_scripts(root: &Path, path: &str) -> bool {
    root.join(path).join("scripts").exists()
}

/// 校验一个技能包是否可以安装。
///
/// 两条硬规则：
///
/// 1. **不带 `scripts/`** —— 技能包是提示词，不是可执行程序。
/// 2. **`tools` 白名单里只能有已知工具名** —— 写错名字的技能包在运行时
///    不会报错（白名单只是收敛项），但会静默地什么都做不了，
///    所以要在安装阶段就拦下来。
pub fn validate_skill(root: &Path, path: &str, m: &SkillManifest) -> Result<(), String> {
    if has_scripts(root, path) {
        return Err(format!(
            "skill package must not contain a scripts/ directory: {path}"
        ));
    }
    for t in &m.tools {
        if !super::tools::KNOWN_TOOLS.contains(&t.as_str()) {
            return Err(format!("unknown tool in skill whitelist: {t}"));
        }
    }
    Ok(())
}

/// 把磁盘上的技能登记到某个家庭（可选立即启用）。
pub async fn install(
    db: &PgPool,
    family_id: Uuid,
    root: &Path,
    skill_id: &str,
    version: &str,
    enabled: bool,
) -> Result<SkillRow, String> {
    let path = format!("{skill_id}/{version}");
    let manifest = read_skill_file(root, &path)?;
    validate_skill(root, &path, &manifest)?;
    let row: SkillRow = sqlx::query_as(
        "INSERT INTO agent_skills
           (family_id, skill_id, version, name, description, path, tools, enabled)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (family_id, skill_id, version) DO UPDATE SET
           name = EXCLUDED.name, description = EXCLUDED.description,
           path = EXCLUDED.path, tools = EXCLUDED.tools,
           enabled = EXCLUDED.enabled, updated_at = NOW()
         RETURNING skill_id, version, name, description, path, tools",
    )
    .bind(family_id)
    .bind(skill_id)
    .bind(version)
    .bind(&manifest.name)
    .bind(&manifest.description)
    .bind(&path)
    .bind(&manifest.tools)
    .bind(enabled)
    .fetch_one(db)
    .await
    .map_err(|e| format!("skill install failed: {e}"))?;
    Ok(row)
}

/// 启用 / 停用某个家庭的某个技能版本。
///
/// 启用时会把**同 `skill_id` 的其它版本**自动停用 —— 同一时刻只允许一个
/// 版本生效，否则已启用家庭的行为不可复现（谁也不知道当前生效的是哪版）。
pub async fn set_enabled(
    db: &PgPool,
    family_id: Uuid,
    skill_id: &str,
    version: &str,
    enabled: bool,
) -> Result<bool, String> {
    if enabled {
        sqlx::query(
            "UPDATE agent_skills SET enabled = false, updated_at = NOW()
             WHERE family_id = $1 AND skill_id = $2 AND version <> $3 AND enabled = true",
        )
        .bind(family_id)
        .bind(skill_id)
        .bind(version)
        .execute(db)
        .await
        .map_err(|e| format!("skill version switch failed: {e}"))?;
    }
    let n = sqlx::query(
        "UPDATE agent_skills SET enabled = $4, updated_at = NOW()
         WHERE family_id = $1 AND skill_id = $2 AND version = $3",
    )
    .bind(family_id)
    .bind(skill_id)
    .bind(version)
    .bind(enabled)
    .execute(db)
    .await
    .map_err(|e| format!("skill update failed: {e}"))?;
    Ok(n.rows_affected() > 0)
}

/// 列出该家庭已安装的技能（启用与否都返回）。
pub async fn list_installed(
    db: &PgPool,
    family_id: Uuid,
) -> Result<Vec<(SkillRow, bool)>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, Vec<String>, bool)>(
        "SELECT skill_id, version, name, description, path, tools, enabled
         FROM agent_skills WHERE family_id = $1
         ORDER BY skill_id ASC, version DESC",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("skill list failed: {e}"))?;

    Ok(rows
        .into_iter()
        .map(
            |(skill_id, version, name, description, path, tools, enabled)| {
                (
                    SkillRow {
                        skill_id,
                        version,
                        name,
                        description,
                        path,
                        tools,
                    },
                    enabled,
                )
            },
        )
        .collect())
}

/// 加载某个家庭启用的技能清单（含正文）。
pub async fn load_enabled(
    db: &PgPool,
    family_id: Uuid,
    root: &Path,
) -> Result<Vec<SkillManifest>, String> {
    let rows = list_installed(db, family_id).await?;
    let mut out = Vec::new();
    for (row, enabled) in rows {
        if !enabled {
            continue;
        }
        match read_skill_file(root, &row.path) {
            Ok(mut m) => {
                // DB 的路径是权威（磁盘结构可能变），描述与工具也以磁盘为准，
                // 因为技能版本不可变，改了文件就等于发了新版本。
                m.skill_id = row.skill_id;
                m.version = row.version;
                out.push(m);
            }
            Err(e) => {
                tracing::warn!("skill {} unavailable: {e}", row.skill_id);
            }
        }
    }
    Ok(out)
}

/// 按 skill_id 加载一个启用中的技能。
pub async fn load_skill(
    db: &PgPool,
    family_id: Uuid,
    root: &Path,
    skill_id: &str,
) -> Result<SkillManifest, String> {
    let row: Option<(String, String, String, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT skill_id, version, name, description, path, tools
         FROM agent_skills
         WHERE family_id = $1 AND skill_id = $2 AND enabled = true
         ORDER BY version DESC LIMIT 1",
    )
    .bind(family_id)
    .bind(skill_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("skill lookup failed: {e}"))?;

    let Some((sid, version, _name, _desc, path, _tools)) = row else {
        return Err(format!("skill not found or not enabled: {skill_id}"));
    };
    let mut m = read_skill_file(root, &path)?;
    m.skill_id = sid;
    m.version = version;
    Ok(m)
}

/// 把技能正文渲染成提示词片段。
pub fn render_skill(m: &SkillManifest) -> String {
    let mut s = String::new();
    s.push_str(&format!("### 场景包：{}（v{}）\n", m.name, m.version));
    if !m.description.is_empty() {
        s.push_str(&format!("适用场景：{}\n", m.description));
    }
    if !m.body.trim().is_empty() {
        s.push('\n');
        s.push_str(m.body.trim());
        s.push('\n');
    }
    s
}

/// 把启用的技能渲染成提示词里的目录（只有名称 + 适用场景，正文按需加载）。
pub fn render_catalog(items: &[SkillManifest]) -> String {
    let mut s = String::new();
    for m in items {
        let desc = if m.description.trim().is_empty() {
            "（未写适用场景）".to_string()
        } else {
            m.description.trim().to_string()
        };
        s.push_str(&format!("- {}（{}）：{}\n", m.skill_id, m.name, desc));
    }
    s
}

/// 技能白名单校验。
///
/// 空白名单 = 不额外收敛（仍走服务端全局只读约束）。
pub fn tool_allowed(m: &SkillManifest, tool: &str) -> bool {
    m.tools.is_empty() || m.tools.iter().any(|t| t == tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "---\nname: 家庭关系修复\ndescription: 处理家庭成员之间的沟通议题\nversion: 1.0.0\ntools: [list_people, list_events]\n---\n# 正文\n先了解家庭成员。\n";

    #[test]
    fn parses_frontmatter_and_body() {
        let m = parse_skill(RAW).unwrap();
        assert_eq!(m.name, "家庭关系修复");
        assert_eq!(m.description, "处理家庭成员之间的沟通议题");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.tools, vec!["list_people", "list_events"]);
        assert!(m.body.contains("先了解家庭成员"));
        assert!(!m.body.contains("name:"));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse_skill("# 只是正文").is_err());
    }

    #[test]
    fn rejects_unclosed_frontmatter() {
        assert!(parse_skill("---\nname: x\n").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        assert!(parse_skill("---\ndescription: 只有描述\n---\n正文").is_err());
    }

    #[test]
    fn parses_tools_without_brackets() {
        let m = parse_skill("---\nname: x\ntools: list_people, list_tasks\n---\n正文").unwrap();
        assert_eq!(m.tools, vec!["list_people", "list_tasks"]);
    }

    #[test]
    fn missing_tools_means_no_narrowing() {
        let m = parse_skill("---\nname: x\n---\n正文").unwrap();
        assert!(m.tools.is_empty());
        assert!(tool_allowed(&m, "list_people"), "空白名单不应限制工具");
    }

    #[test]
    fn whitelist_narrows_tools() {
        let m = parse_skill(RAW).unwrap();
        assert!(tool_allowed(&m, "list_people"));
        assert!(!tool_allowed(&m, "list_things"), "不在白名单的工具应收敛");
    }

    #[test]
    fn render_includes_name_and_body() {
        let m = parse_skill(RAW).unwrap();
        let s = render_skill(&m);
        assert!(s.contains("家庭关系修复"));
        assert!(s.contains("先了解家庭成员"));
    }

    #[test]
    fn validation_accepts_known_tools() {
        let m = parse_skill(RAW).unwrap();
        assert!(validate_skill(Path::new("/nonexistent"), "x/1.0.0", &m).is_ok());
    }

    #[test]
    fn validation_rejects_unknown_tool() {
        let m = parse_skill("---\nname: x\ntools: [list_people, teleport]\n---\n正文").unwrap();
        let err = validate_skill(Path::new("/nonexistent"), "x/1.0.0", &m)
            .expect_err("未知工具名应被拒绝");
        assert!(err.contains("unknown tool"), "msg was: {err}");
    }

    #[test]
    fn validation_rejects_scripts_dir() {
        let dir = std::env::temp_dir().join(format!("hamr-skill-test-{}", uuid::Uuid::new_v4()));
        let pkg = dir.join("x/1.0.0");
        std::fs::create_dir_all(pkg.join("scripts")).expect("create scripts dir");
        let m = parse_skill("---\nname: x\n---\n正文").unwrap();
        let err = validate_skill(&dir, "x/1.0.0", &m).expect_err("带 scripts/ 的技能包应被拒绝");
        assert!(err.contains("scripts/"), "msg was: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
