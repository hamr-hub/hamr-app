//! Agent 工具集。
//!
//! v1 的工具全部是**只读**的：Agent 可以看，但不能替用户改写家庭数据。
//! 唯一的例外是 `remember` —— 它写入 Agent 自己的长期记忆表，
//! 不碰 people / events / tasks / things / spaces。
//!
//! 一条重要的安全设计：**`family_id` 永远来自服务端上下文，不来自模型的参数**。
//! 模型说什么都不影响它能查哪个家庭 —— 否则模型被诱导传入别的 family_id
//! 就能读到别人家的数据。
//!
//! `jiabu_score` 是与 JiaBu 决策系统的边界接口：Agent 负责对话与行动编排，
//! 「幸福指数」这类量化评分仍然归 JiaBu 的规则引擎。

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::config::ProviderKind;
use super::privacy::Redactor;
use super::provider::{ChatMessage, ToolSpec};
use super::store::{ConversationStore, MemoryKind};

/// 工具执行上下文。
pub struct ToolContext {
    pub db: PgPool,
    pub store: std::sync::Arc<dyn ConversationStore>,
    /// 当前会话所属家庭。由服务端从请求体取出后固定下来。
    pub family_id: Uuid,
    /// 是否允许把原始 PII 交给模型（云端 + 用户显式放开时为 true）。
    pub allow_pii: bool,
    pub redactor: Redactor,
    /// JiaBu 地址；为 None 时不暴露 `jiabu_score` 工具。
    pub jiabu_url: Option<String>,
    pub http: reqwest::Client,
    /// 模型来源 —— 本地不做脱敏。
    pub provider: ProviderKind,
    /// 当前对话的家庭成员（None = 家庭公共视角）。
    pub member_id: Option<Uuid>,
    /// 技能包根目录。
    pub skills_dir: std::path::PathBuf,
    /// 是否允许发起写操作**提议**（默认关闭）。
    pub allow_write: bool,
}

impl ToolContext {
    /// 工具结果是否需要脱敏。
    pub fn needs_redaction(&self) -> bool {
        self.provider == ProviderKind::Cloud && !self.allow_pii
    }
}

/// 工具参数（模型给的 arguments）。
#[derive(Debug, Default, serde::Deserialize)]
struct ToolArgs {
    days: Option<i64>,
    status: Option<String>,
    category: Option<String>,
    content: Option<String>,
    kind: Option<String>,
    query: Option<String>,
    entity_type: Option<String>,
    key_or_title: Option<String>,
    title: Option<String>,
    description: Option<String>,
    due_date: Option<String>,
    priority: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
    location: Option<String>,
    skill_id: Option<String>,
}

/// 所有工具名。
///
/// 场景包（技能包）的 `tools` 白名单只能引用这些名字 —— 白名单里出现
/// 未知工具名时，技能包会被拒绝安装。写错了名字的技能包在运行时不会
/// 报错（白名单只是收敛项），但它会静默地什么都做不了，所以要在安装
/// 阶段就拦下来。
pub const KNOWN_TOOLS: &[&str] = &[
    "list_people",
    "list_events",
    "list_tasks",
    "list_things",
    "list_spaces",
    "get_dashboard",
    "remember",
    "jiabu_score",
    "search_wiki",
    "wiki_page",
    "use_skill",
    "propose_task",
    "propose_event",
];

/// 构造工具列表。
///
/// - `jiabu_enabled` 决定是否暴露幸福指数工具；
/// - `allow_write` 决定是否暴露写操作**提议**工具（默认关闭）。
pub fn tool_specs(jiabu_enabled: bool, allow_write: bool) -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "list_people".to_string(),
                description: "列出家庭成员档案（姓名、角色、生日、联系方式、备注）。回答任何涉及「谁」的问题前先调用。".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "list_events".to_string(),
                description: "查询家庭日程。回答涉及「什么时候、有什么安排」的问题时调用。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "days": {
                            "type": "integer",
                            "description": "往后看多少天，默认 30 天"
                        }
                    }
                }),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "list_tasks".to_string(),
                description: "查询待办任务。回答涉及「有什么要做、谁负责」的问题时调用。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "任务状态筛选：todo / in_progress / done / cancelled，不填则查全部"
                        }
                    }
                }),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "list_things".to_string(),
                description: "查询家庭物品与资产。回答涉及「东西在哪、什么快过期了」的问题时调用。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "物品状态筛选：active / broken / sold / discarded"
                        },
                        "category": {
                            "type": "string",
                            "description": "物品分类筛选，如 电器 / 家具 / 食品"
                        }
                    }
                }),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "list_spaces".to_string(),
                description: "查询家庭空间（房间、区域）。回答涉及「在家里哪个位置」的问题时调用。".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "get_dashboard".to_string(),
                description: "获取家庭概览统计：成员数、待办数、近期日程数、物品数、空间数。".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        },
        ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "remember".to_string(),
                description: "记录一条长期记忆（家庭偏好、过敏史、重要约定等），之后的对话会自动带上。只记录用户明确要求记住或对话中明确成立的长期事实，不要记录临时状态。kind 取值：fact=长期事实（默认），preference=偏好线索（被再次确认会升级为家规），rule=明确的家规/硬性规则。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "记忆内容，一句话"
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["fact", "preference", "rule"],
                            "description": "记忆类型，默认 fact；用户说「以后都/必须/别再」这类硬性要求时用 rule；口味、作息等偏好用 preference"
                        }
                    },
                    "required": ["content"]
                }),
            },
        },
    ];

    // 家庭 Wiki：知识层只读工具，任何时候都可用。
    specs.push(ToolSpec {
        spec_type: "function".to_string(),
        function: super::provider::FunctionDef {
            name: "search_wiki".to_string(),
            description: "检索家庭知识库（Wiki）。Wiki 是家庭成员、物品、空间、日程、待办的结构化汇总，回答「家里有哪些人/东西/房间」这类问题前先查它，比逐个列表工具更省事。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "检索关键词，如「牛奶」「妈妈」「厨房」"
                    }
                },
                "required": ["query"]
            }),
        },
    });
    specs.push(ToolSpec {
        spec_type: "function".to_string(),
        function: super::provider::FunctionDef {
            name: "wiki_page".to_string(),
            description: "读取家庭知识库中某个实体的完整页面（分节 + 来源）。entity_type 取 member/thing/space/event/task/family，key_or_title 可传实体标题或 key。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_type": {
                        "type": "string",
                        "enum": ["member", "thing", "space", "event", "task", "family"],
                        "description": "实体类型"
                    },
                    "key_or_title": {
                        "type": "string",
                        "description": "实体标题或 key"
                    }
                },
                "required": ["entity_type", "key_or_title"]
            }),
        },
    });
    specs.push(ToolSpec {
        spec_type: "function".to_string(),
        function: super::provider::FunctionDef {
            name: "use_skill".to_string(),
            description: "加载一个家庭场景包（技能包）的详细操作指引。场景包描述里写明了适用场景，遇到相符的家庭议题时加载它并按其指引回答。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill_id": {
                        "type": "string",
                        "description": "场景包标识"
                    }
                },
                "required": ["skill_id"]
            }),
        },
    });

    if jiabu_enabled {
        specs.push(ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "jiabu_score".to_string(),
                description: "获取家庭幸福指数评分（JiaBu 决策引擎计算，含人/时/事/物/境五维）。回答「我们家怎么样、幸福指数多少」时调用。".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        });
    }

    if allow_write {
        specs.push(ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "propose_task".to_string(),
                description: "提议为家庭新增一条待办。你不能直接写入 —— 这里只登记一条待用户确认的提议，用户确认后才会真正创建。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "待办标题，一句话" },
                        "description": { "type": "string", "description": "补充说明（可选）" },
                        "due_date": { "type": "string", "description": "截止时间，RFC3339（可选）" },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "urgent"],
                            "description": "优先级，默认 medium"
                        }
                    },
                    "required": ["title"]
                }),
            },
        });
        specs.push(ToolSpec {
            spec_type: "function".to_string(),
            function: super::provider::FunctionDef {
                name: "propose_event".to_string(),
                description: "提议为家庭新增一条日程。你不能直接写入 —— 这里只登记一条待用户确认的提议，用户确认后才会真正创建。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "日程标题" },
                        "start_time": { "type": "string", "description": "开始时间，RFC3339" },
                        "end_time": { "type": "string", "description": "结束时间，RFC3339（可选）" },
                        "location": { "type": "string", "description": "地点（可选）" },
                        "description": { "type": "string", "description": "补充说明（可选）" }
                    },
                    "required": ["title", "start_time"]
                }),
            },
        });
    }

    specs
}

/// 执行一次工具调用，返回给模型的文本（通常是 JSON 字符串）。
pub async fn dispatch(ctx: &ToolContext, name: &str, arguments: &str) -> Result<String, String> {
    let args: ToolArgs = if arguments.trim().is_empty() {
        ToolArgs::default()
    } else {
        serde_json::from_str(arguments).map_err(|e| format!("invalid tool arguments: {e}"))?
    };

    let result = match name {
        "list_people" => list_people(ctx).await,
        "list_events" => list_events(ctx, args.days).await,
        "list_tasks" => list_tasks(ctx, args.status).await,
        "list_things" => list_things(ctx, args.status, args.category).await,
        "list_spaces" => list_spaces(ctx).await,
        "get_dashboard" => get_dashboard(ctx).await,
        "remember" => remember(ctx, args.content, args.kind).await,
        "jiabu_score" => jiabu_score(ctx).await,
        "search_wiki" => search_wiki(ctx, args.query).await,
        "wiki_page" => wiki_page(ctx, args.entity_type, args.key_or_title).await,
        "use_skill" => use_skill(ctx, args.skill_id).await,
        "propose_task" => propose_task(ctx, args).await,
        "propose_event" => propose_event(ctx, args).await,
        other => Err(format!("unknown tool: {other}")),
    };

    match result {
        Ok(text) => {
            if ctx.needs_redaction() {
                Ok(ctx.redactor.redact(&text))
            } else {
                Ok(text)
            }
        }
        Err(e) => Err(e),
    }
}

async fn list_people(ctx: &ToolContext) -> Result<String, String> {
    let rows = sqlx::query_as::<_, crate::models::Person>(
        "SELECT * FROM people WHERE family_id = $1 ORDER BY created_at ASC",
    )
    .bind(ctx.family_id)
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| format!("list_people failed: {e}"))?;
    serde_json::to_string(&rows).map_err(|e| format!("serialize people failed: {e}"))
}

async fn list_events(ctx: &ToolContext, days: Option<i64>) -> Result<String, String> {
    let days = days.unwrap_or(30).clamp(1, 365);
    let from = Utc::now() - chrono::Duration::days(days);
    let rows = sqlx::query_as::<_, crate::models::Event>(
        r#"SELECT * FROM events
           WHERE family_id = $1 AND start_time >= $2
           ORDER BY start_time ASC
           LIMIT 200"#,
    )
    .bind(ctx.family_id)
    .bind(from)
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| format!("list_events failed: {e}"))?;
    serde_json::to_string(&rows).map_err(|e| format!("serialize events failed: {e}"))
}

async fn list_tasks(ctx: &ToolContext, status: Option<String>) -> Result<String, String> {
    // 状态值来自模型，必须校验 —— 直接拼进 SQL 会造成注入。
    let status = status
        .map(|s| s.trim().to_lowercase())
        .filter(|s| matches!(s.as_str(), "todo" | "in_progress" | "done" | "cancelled"));

    let rows = match status {
        Some(s) => {
            sqlx::query_as::<_, crate::models::Task>(
                "SELECT * FROM tasks WHERE family_id = $1 AND status = $2 ORDER BY due_date ASC NULLS LAST LIMIT 200",
            )
            .bind(ctx.family_id)
            .bind(&s)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_tasks failed: {e}"))?
        }
        None => {
            sqlx::query_as::<_, crate::models::Task>(
                "SELECT * FROM tasks WHERE family_id = $1 ORDER BY due_date ASC NULLS LAST LIMIT 200",
            )
            .bind(ctx.family_id)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_tasks failed: {e}"))?
        }
    };
    serde_json::to_string(&rows).map_err(|e| format!("serialize tasks failed: {e}"))
}

async fn list_things(
    ctx: &ToolContext,
    status: Option<String>,
    category: Option<String>,
) -> Result<String, String> {
    let status = status
        .map(|s| s.trim().to_lowercase())
        .filter(|s| matches!(s.as_str(), "active" | "broken" | "sold" | "discarded"));
    let category = category
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());

    let rows = match (status, category) {
        (Some(s), Some(c)) => {
            sqlx::query_as::<_, crate::models::Thing>(
                "SELECT * FROM things WHERE family_id = $1 AND status = $2 AND category = $3 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(ctx.family_id)
            .bind(&s)
            .bind(&c)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_things failed: {e}"))?
        }
        (Some(s), None) => {
            sqlx::query_as::<_, crate::models::Thing>(
                "SELECT * FROM things WHERE family_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(ctx.family_id)
            .bind(&s)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_things failed: {e}"))?
        }
        (None, Some(c)) => {
            sqlx::query_as::<_, crate::models::Thing>(
                "SELECT * FROM things WHERE family_id = $1 AND category = $2 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(ctx.family_id)
            .bind(&c)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_things failed: {e}"))?
        }
        (None, None) => {
            sqlx::query_as::<_, crate::models::Thing>(
                "SELECT * FROM things WHERE family_id = $1 ORDER BY created_at DESC LIMIT 200",
            )
            .bind(ctx.family_id)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| format!("list_things failed: {e}"))?
        }
    };
    serde_json::to_string(&rows).map_err(|e| format!("serialize things failed: {e}"))
}

async fn list_spaces(ctx: &ToolContext) -> Result<String, String> {
    let rows = sqlx::query_as::<_, crate::models::Space>(
        "SELECT * FROM spaces WHERE family_id = $1 ORDER BY created_at ASC",
    )
    .bind(ctx.family_id)
    .fetch_all(&ctx.db)
    .await
    .map_err(|e| format!("list_spaces failed: {e}"))?;
    serde_json::to_string(&rows).map_err(|e| format!("serialize spaces failed: {e}"))
}

async fn get_dashboard(ctx: &ToolContext) -> Result<String, String> {
    let stats = sqlx::query_as::<_, crate::models::DashboardStats>(
        r#"SELECT
             $1::uuid AS family_id,
             (SELECT COUNT(*) FROM people WHERE family_id = $1) AS people_count,
             (SELECT COUNT(*) FROM events WHERE family_id = $1 AND start_time >= NOW()) AS upcoming_events,
             (SELECT COUNT(*) FROM tasks WHERE family_id = $1 AND status IN ('todo','in_progress')) AS pending_tasks,
             (SELECT COUNT(*) FROM things WHERE family_id = $1) AS things_count,
             (SELECT COUNT(*) FROM spaces WHERE family_id = $1) AS spaces_count"#,
    )
    .bind(ctx.family_id)
    .fetch_one(&ctx.db)
    .await
    .map_err(|e| format!("get_dashboard failed: {e}"))?;
    serde_json::to_string(&stats).map_err(|e| format!("serialize dashboard failed: {e}"))
}

async fn remember(
    ctx: &ToolContext,
    content: Option<String>,
    kind: Option<String>,
) -> Result<String, String> {
    let content = content
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "remember requires non-empty content".to_string())?;
    if content.chars().count() > 500 {
        return Err("memory content too long (max 500 chars)".to_string());
    }

    // kind 由模型给出，但取值必须由我们判定 —— 非法值直接回喂模型让它改。
    let kind = match kind.as_deref().map(str::trim) {
        None | Some("") => MemoryKind::Fact,
        Some(raw) => MemoryKind::parse(raw).ok_or_else(|| {
            format!("invalid memory kind: {raw} (expected one of fact / preference / rule)")
        })?,
    };

    // clue → rule：内容已存在说明用户再次确认同一条 —— 强化它，
    // 偏好类会就地升级为家规，而不是插入一条重复记忆。
    // 存储层用一条语句完成「插入或强化」，并发下也不会写出两行。
    let m = ctx
        .store
        .remember(ctx.family_id, &content, "agent", kind)
        .await
        .map_err(|e| format!("remember failed: {e}"))?;

    Ok(json!({
        "ok": true,
        "memory_id": m.id,
        "reinforced": m.reinforced_count > 0,
        "kind": m.kind.as_str(),
        "reinforced_count": m.reinforced_count,
    })
    .to_string())
}

async fn jiabu_score(ctx: &ToolContext) -> Result<String, String> {
    let base = ctx
        .jiabu_url
        .as_ref()
        .ok_or_else(|| "jiabu_score is not configured".to_string())?;
    let url = format!("{}/api/score?family_id={}", base, ctx.family_id);
    let resp = ctx
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("jiabu request failed: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("jiabu read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("jiabu returned {status}"));
    }
    Ok(body)
}


/// 解析 RFC3339 时间戳。
fn parse_time(raw: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .map(|t| t.with_timezone(&chrono::Utc))
        .map_err(|e| format!("invalid timestamp {raw}: {e}"))
}

/// 检索家庭 Wiki。
async fn search_wiki(ctx: &ToolContext, query: Option<String>) -> Result<String, String> {
    let q = query
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "search_wiki requires a non-empty query".to_string())?;
    let hits = super::wiki::search(&ctx.db, ctx.family_id, &q, 20)
        .await
        .map_err(|e| format!("search_wiki failed: {e}"))?;
    if hits.is_empty() {
        return Ok(json!({"hits": [], "note": "家庭知识库中没有匹配的内容"}).to_string());
    }
    serde_json::to_string(&hits).map_err(|e| format!("serialize hits failed: {e}"))
}

/// 读取家庭 Wiki 的一个实体页。
async fn wiki_page(
    ctx: &ToolContext,
    entity_type: Option<String>,
    key_or_title: Option<String>,
) -> Result<String, String> {
    let et = entity_type
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "wiki_page requires entity_type".to_string())?;
    if !matches!(
        et.as_str(),
        super::wiki::entity_type::MEMBER
            | super::wiki::entity_type::THING
            | super::wiki::entity_type::SPACE
            | super::wiki::entity_type::EVENT
            | super::wiki::entity_type::TASK
            | super::wiki::entity_type::FAMILY
    ) {
        return Err(format!("invalid entity_type: {et}"));
    }
    let key = key_or_title
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "wiki_page requires key_or_title".to_string())?;
    let detail = super::wiki::page_by_ref(&ctx.db, ctx.family_id, &et, &key)
        .await
        .map_err(|e| format!("wiki_page failed: {e}"))?;
    match detail {
        Some(d) => serde_json::to_string(&d).map_err(|e| format!("serialize page failed: {e}")),
        None => Ok(json!({"found": false, "note": "家庭知识库里没有这个实体"}).to_string()),
    }
}

/// 加载场景包（技能包）正文。
async fn use_skill(ctx: &ToolContext, skill_id: Option<String>) -> Result<String, String> {
    let sid = skill_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "use_skill requires a skill_id".to_string())?;
    let m = super::skills::load_skill(&ctx.db, ctx.family_id, &ctx.skills_dir, &sid)
        .await
        .map_err(|e| format!("use_skill failed: {e}"))?;
    Ok(super::skills::render_skill(&m))
}

/// 提议新增待办（不直接写入业务表）。
async fn propose_task(ctx: &ToolContext, args: ToolArgs) -> Result<String, String> {
    if !ctx.allow_write {
        return Err("write proposals are disabled (HAMR_AGENT_ALLOW_WRITE=false)".to_string());
    }
    let title = args
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "propose_task requires a title".to_string())?;
    let due_date = match args.due_date.as_deref() {
        Some(d) if !d.trim().is_empty() => Some(parse_time(d)?),
        _ => None,
    };
    let payload = json!({
        "title": title,
        "description": args.description,
        "due_date": due_date,
        "priority": args.priority,
    });
    let op = super::writeops::propose(
        &ctx.db,
        ctx.family_id,
        ctx.member_id,
        super::writeops::kind::CREATE_TASK,
        payload,
        None,
    )
    .await
    .map_err(|e| format!("propose_task failed: {e}"))?;
    Ok(json!({
        "ok": true,
        "operation_id": op.operation_id,
        "status": op.status,
        "preview": op.preview,
        "note": "已登记待确认的提议，请用户在界面上确认后才会真正创建。"
    })
    .to_string())
}

/// 提议新增日程（不直接写入业务表）。
async fn propose_event(ctx: &ToolContext, args: ToolArgs) -> Result<String, String> {
    if !ctx.allow_write {
        return Err("write proposals are disabled (HAMR_AGENT_ALLOW_WRITE=false)".to_string());
    }
    let title = args
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "propose_event requires a title".to_string())?;
    let start_time = args
        .start_time
        .as_deref()
        .map(parse_time)
        .transpose()?
        .ok_or_else(|| "propose_event requires start_time".to_string())?;
    let end_time = match args.end_time.as_deref() {
        Some(d) if !d.trim().is_empty() => Some(parse_time(d)?),
        _ => None,
    };
    let payload = json!({
        "title": title,
        "start_time": start_time,
        "end_time": end_time,
        "location": args.location,
        "description": args.description,
    });
    let op = super::writeops::propose(
        &ctx.db,
        ctx.family_id,
        ctx.member_id,
        super::writeops::kind::CREATE_EVENT,
        payload,
        None,
    )
    .await
    .map_err(|e| format!("propose_event failed: {e}"))?;
    Ok(json!({
        "ok": true,
        "operation_id": op.operation_id,
        "status": op.status,
        "preview": op.preview,
        "note": "已登记待确认的提议，请用户在界面上确认后才会真正创建。"
    })
    .to_string())
}

/// 把工具结果包成一条 `tool` 消息。
pub fn tool_message(tool_call_id: &str, content: &str) -> ChatMessage {
    ChatMessage::Tool {
        content: content.to_string(),
        tool_call_id: tool_call_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_are_wellformed() {
        let specs = tool_specs(false, false);
        assert!(!specs.is_empty());
        for s in &specs {
            assert_eq!(s.spec_type, "function");
            assert!(!s.function.name.is_empty());
            assert!(!s.function.description.is_empty());
            assert_eq!(
                s.function.parameters["type"], "object",
                "{} 的参数 schema 必须是 object",
                s.function.name
            );
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let specs = tool_specs(true, false);
        let mut names: Vec<&str> = specs.iter().map(|s| s.function.name.as_str()).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "工具名不能重复");
    }

    #[test]
    fn jiabu_tool_only_present_when_enabled() {
        let with = tool_specs(true, false);
        assert!(with.iter().any(|s| s.function.name == "jiabu_score"));
        let without = tool_specs(false, false);
        assert!(!without.iter().any(|s| s.function.name == "jiabu_score"));
    }

    #[test]
    fn write_proposal_tools_only_present_when_write_allowed() {
        // 写操作提议工具默认关闭 —— 关闭时模型看不到任何能触发落库路径的工具。
        let readonly = tool_specs(false, false);
        assert!(!readonly.iter().any(|s| s.function.name == "propose_task"));
        assert!(!readonly.iter().any(|s| s.function.name == "propose_event"));

        let writable = tool_specs(false, true);
        assert!(writable.iter().any(|s| s.function.name == "propose_task"));
        assert!(writable.iter().any(|s| s.function.name == "propose_event"));
    }

    #[test]
    fn remember_tool_requires_content_in_schema() {
        let specs = tool_specs(false, false);
        let remember = specs
            .iter()
            .find(|s| s.function.name == "remember")
            .expect("remember tool");
        assert_eq!(remember.function.parameters["required"][0], "content");
    }

    #[test]
    fn no_tool_exposes_family_id_parameter() {
        // family_id 必须由服务端注入 —— 模型参数里不该出现它。
        for s in tool_specs(true, false) {
            let props = &s.function.parameters["properties"];
            assert!(
                props.get("family_id").is_none(),
                "{} 不应接受 family_id 参数",
                s.function.name
            );
        }
    }

    #[tokio::test]
    async fn invalid_arguments_produce_error() {
        // test_ctx 里的 connect_lazy 需要 Tokio 上下文 —— 必须在 async 测试内构造。
        let ctx = crate::agent::runner::test_ctx();
        let err = dispatch(&ctx, "list_people", "{not json").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("invalid tool arguments"));
    }

    #[tokio::test]
    async fn unknown_tool_produces_error() {
        let ctx = crate::agent::runner::test_ctx();
        let err = dispatch(&ctx, "rm_rf", "{}").await;
        assert!(err.unwrap_err().contains("unknown tool"));
    }

    #[test]
    fn tool_message_keeps_tool_call_id() {
        let m = tool_message("call_9", "{\"ok\":true}");
        match m {
            ChatMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                assert_eq!(tool_call_id, "call_9");
                assert_eq!(content, "{\"ok\":true}");
            }
            _ => panic!("应构造 tool 消息"),
        }
    }

    #[tokio::test]
    async fn remember_rejects_invalid_kind() {
        let ctx = crate::agent::runner::test_ctx();
        let err = dispatch(&ctx, "remember", r#"{"content":"x","kind":"weird"}"#)
            .await
            .unwrap_err();
        assert!(err.contains("invalid memory kind"), "实际: {err}");
    }

    #[tokio::test]
    async fn remember_stores_with_kind_and_reinforces_on_repeat() {
        let ctx = crate::agent::runner::test_ctx();
        let fam = ctx.family_id;

        let first = dispatch(
            &ctx,
            "remember",
            r#"{"content":"晚饭别放香菜","kind":"preference"}"#,
        )
        .await
        .unwrap();
        assert!(first.contains("\"reinforced\":false"), "实际: {first}");
        assert!(first.contains("\"kind\":\"preference\""), "实际: {first}");

        // 同一内容再来一次 → 强化；偏好升级为家规
        let second = dispatch(
            &ctx,
            "remember",
            r#"{"content":"晚饭别放香菜","kind":"preference"}"#,
        )
        .await
        .unwrap();
        assert!(second.contains("\"reinforced\":true"), "实际: {second}");
        assert!(second.contains("\"kind\":\"rule\""), "实际: {second}");

        // 不应产生重复记忆行
        let mems = ctx.store.memories(fam, 50).await.unwrap();
        assert_eq!(mems.len(), 1, "重复 remember 不应新增记忆行");
        assert_eq!(mems[0].kind, MemoryKind::Rule);
    }
}
