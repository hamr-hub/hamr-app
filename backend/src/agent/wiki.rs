//! 家庭 Wiki —— 家庭版 Agent 的知识层。
//!
//! 对应 Linger 的 personal wiki，但**派生源完全不同**：
//!
//! - Linger 从 Gmail / Slack / 日历连接器灌入，属于「二次采集」，天然带噪声，
//!   还要靠模型提议 + 应用校验来合并。
//! - HamR 已有结构化的五维数据底座（人 / 时 / 事 / 物 / 境），
//!   所以家庭 Wiki 是五维数据的**确定性派生物** —— 由应用侧重建，不经过模型。
//!
//! 这条区别决定了本模块的设计：
//!
//! 1. **模型只读不可写**。页面、分节、来源引用、变更日志全部由 `rebuild()` 写。
//!    模型能检索、能引用，但不能改写页面 —— 避免「模型编造的家庭事实」被固化成知识。
//! 2. **每条事实有来源**。`agent_wiki_sources` 记录该节来自五维表的哪一行。
//! 3. **变更有日志**。`agent_wiki_changelog` append-only，由应用侧追加，
//!    回答「这条记忆是什么时候变的」。
//! 4. **两段式检索**。先看摘要索引（进提示词），细节按需取全文 ——
//!    对应 Linger 的「首条消息注入摘要索引，详情按需加载」。
//!
//! 隐私：Wiki 正文可能含电话 / 邮箱等 PII，它会随提示词进入上下文 ——
//! 但出网前统一经过 `privacy::Redactor`，与五维数据走同一条脱敏闸门。

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// 实体类型。
pub mod entity_type {
    /// 家庭成员（来自 people 表）。
    pub const MEMBER: &str = "member";
    /// 物品资产（来自 things 表）。
    pub const THING: &str = "thing";
    /// 居家空间（来自 spaces 表）。
    pub const SPACE: &str = "space";
    /// 日程（来自 events 表）。
    pub const EVENT: &str = "event";
    /// 待办（来自 tasks 表）。
    pub const TASK: &str = "task";
    /// 家庭总览页。
    pub const FAMILY: &str = "family";
}

/// 一个实体页。
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct WikiPage {
    pub id: Uuid,
    pub family_id: Uuid,
    pub entity_type: String,
    pub entity_key: String,
    pub title: String,
    pub summary: String,
    pub updated_at: DateTime<Utc>,
}

/// 页内一节。
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct WikiSection {
    pub id: Uuid,
    pub page_id: Uuid,
    pub heading: String,
    pub body: String,
    pub sort_order: i32,
    pub updated_at: DateTime<Utc>,
}

/// 一条来源引用。
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct WikiSource {
    pub id: Uuid,
    pub section_id: Uuid,
    pub source_kind: String,
    pub source_id: Option<Uuid>,
    pub excerpt: String,
}

/// 一节 + 它的来源。
#[derive(Clone, Debug, Serialize)]
pub struct WikiSectionWithSources {
    pub section: WikiSection,
    pub sources: Vec<WikiSource>,
}

/// 一个页面的完整内容。
#[derive(Clone, Debug, Serialize)]
pub struct WikiPageDetail {
    pub page: WikiPage,
    pub sections: Vec<WikiSectionWithSources>,
}

/// 一条检索命中。
#[derive(Clone, Debug, Serialize)]
pub struct WikiHit {
    pub entity_type: String,
    pub entity_key: String,
    pub title: String,
    pub snippet: String,
}

/// 重建结果统计。
#[derive(Clone, Debug, Serialize)]
pub struct RebuildReport {
    /// 实体页总数。
    pub pages: usize,
    /// 本次内容发生变化的页数。
    pub changed: usize,
}

// ===== 页与节的写入（仅应用侧调用） =====

/// upsert 一个实体页。返回 (page_id, 内容是否变化)。
async fn upsert_page(
    db: &PgPool,
    family_id: Uuid,
    entity_type: &str,
    entity_key: &str,
    title: &str,
    summary: &str,
) -> Result<(Uuid, bool), String> {
    let existing: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, title, summary FROM agent_wiki_pages
         WHERE family_id = $1 AND entity_type = $2 AND entity_key = $3",
    )
    .bind(family_id)
    .bind(entity_type)
    .bind(entity_key)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("wiki page lookup failed: {e}"))?;

    match existing {
        Some((id, t, s)) if t == title && s == summary => Ok((id, false)),
        Some((id, _, _)) => {
            sqlx::query(
                "UPDATE agent_wiki_pages SET title = $2, summary = $3, updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(title)
            .bind(summary)
            .execute(db)
            .await
            .map_err(|e| format!("wiki page update failed: {e}"))?;
            Ok((id, true))
        }
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_wiki_pages (family_id, entity_type, entity_key, title, summary)
                 VALUES ($1,$2,$3,$4,$5) RETURNING id",
            )
            .bind(family_id)
            .bind(entity_type)
            .bind(entity_key)
            .bind(title)
            .bind(summary)
            .fetch_one(db)
            .await
            .map_err(|e| format!("wiki page insert failed: {e}"))?;
            Ok((id, true))
        }
    }
}

/// 同步一节的内容与来源引用。返回内容是否变化。
async fn sync_section(
    db: &PgPool,
    page_id: Uuid,
    heading: &str,
    body: &str,
    sort_order: i32,
    sources: &[(&str, Option<Uuid>, &str)],
) -> Result<bool, String> {
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, body FROM agent_wiki_sections
         WHERE page_id = $1 AND heading = $2",
    )
    .bind(page_id)
    .bind(heading)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("wiki section lookup failed: {e}"))?;

    let (section_id, changed) = match existing {
        Some((id, old_body)) => {
            if old_body == body {
                (id, false)
            } else {
                sqlx::query(
                    "UPDATE agent_wiki_sections SET body = $2, sort_order = $3, updated_at = NOW()
                     WHERE id = $1",
                )
                .bind(id)
                .bind(body)
                .bind(sort_order)
                .execute(db)
                .await
                .map_err(|e| format!("wiki section update failed: {e}"))?;
                (id, true)
            }
        }
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_wiki_sections (page_id, heading, body, sort_order)
                 VALUES ($1,$2,$3,$4) RETURNING id",
            )
            .bind(page_id)
            .bind(heading)
            .bind(body)
            .bind(sort_order)
            .fetch_one(db)
            .await
            .map_err(|e| format!("wiki section insert failed: {e}"))?;
            (id, true)
        }
    };

    // 来源引用整体重建：内容没变时也刷一次，保证来源不丢
    sqlx::query("DELETE FROM agent_wiki_sources WHERE section_id = $1")
        .bind(section_id)
        .execute(db)
        .await
        .map_err(|e| format!("wiki source cleanup failed: {e}"))?;
    for (kind, sid, excerpt) in sources {
        sqlx::query(
            "INSERT INTO agent_wiki_sources (section_id, source_kind, source_id, excerpt)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(section_id)
        .bind(*kind)
        .bind(*sid)
        .bind(*excerpt)
        .execute(db)
        .await
        .map_err(|e| format!("wiki source insert failed: {e}"))?;
    }

    Ok(changed)
}

/// 追加一条变更日志（append-only）。
async fn log_change(
    db: &PgPool,
    family_id: Uuid,
    page_id: Uuid,
    entity_type: &str,
    entity_key: &str,
    action: &str,
    detail: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_wiki_changelog
           (family_id, page_id, entity_type, entity_key, action, detail)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(family_id)
    .bind(page_id)
    .bind(entity_type)
    .bind(entity_key)
    .bind(action)
    .bind(detail)
    .execute(db)
    .await
    .map_err(|e| format!("wiki changelog insert failed: {e}"))?;
    Ok(())
}

/// 清理某个页面下已不存在的节（数据被删除时用）。
async fn prune_sections(
    db: &PgPool,
    page_id: Uuid,
    keep_headings: &[&str],
) -> Result<(), String> {
    let existing: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, heading FROM agent_wiki_sections WHERE page_id = $1")
            .bind(page_id)
            .fetch_all(db)
            .await
            .map_err(|e| format!("wiki section list failed: {e}"))?;
    for (id, heading) in existing {
        if !keep_headings.iter().any(|h| *h == heading) {
            sqlx::query("DELETE FROM agent_wiki_sections WHERE id = $1")
                .bind(id)
                .execute(db)
                .await
                .map_err(|e| format!("wiki section delete failed: {e}"))?;
        }
    }
    Ok(())
}

// ===== 重建 =====

/// 从五维数据确定性重建家庭 Wiki。
///
/// 只在数据真正变化时才写页 / 节，并追加变更日志 —— 避免每次调用都刷一屏日志。
pub async fn rebuild(db: &PgPool, family_id: Uuid) -> Result<RebuildReport, String> {
    let mut pages = 0usize;
    let mut changed_pages = 0usize;

    // ---- 家庭总览页 ----
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM people WHERE family_id = $1),
                (SELECT COUNT(*) FROM tasks WHERE family_id = $1 AND status IN ('todo','in_progress')),
                (SELECT COUNT(*) FROM events WHERE family_id = $1 AND start_time >= NOW()),
                (SELECT COUNT(*) FROM things WHERE family_id = $1),
                (SELECT COUNT(*) FROM spaces WHERE family_id = $1)",
    )
    .bind(family_id)
    .fetch_one(db)
    .await
    .map_err(|e| format!("wiki family stats failed: {e}"))?;

    let (family_page, family_changed) = upsert_page(
        db,
        family_id,
        entity_type::FAMILY,
        "family",
        "家庭总览",
        &format!(
            "{} 位成员、{} 件待办、{} 项近期日程、{} 件物品、{} 个空间",
            counts.0, counts.1, counts.2, counts.3, counts.4
        ),
    )
    .await?;
    pages += 1;
    if family_changed {
        changed_pages += 1;
        log_change(
            db,
            family_id,
            family_page,
            entity_type::FAMILY,
            "family",
            "updated",
            "家庭概览统计已刷新",
        )
        .await?;
    }

    // ---- 成员页 ----
    let people = sqlx::query_as::<_, crate::models::Person>(
        "SELECT * FROM people WHERE family_id = $1 ORDER BY created_at ASC",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki people load failed: {e}"))?;

    for p in &people {
        let role = p.role.clone().unwrap_or_else(|| "成员".to_string());
        let (page_id, page_changed) = upsert_page(
            db,
            family_id,
            entity_type::MEMBER,
            &p.id.to_string(),
            &p.name,
            &format!("{role}，家庭成员"),
        )
        .await?;
        pages += 1;

        let mut basics = format!("姓名：{}\n角色：{role}", p.name);
        if let Some(b) = p.birthday {
            basics.push_str(&format!("\n生日：{b}"));
        }
        if let Some(ph) = &p.phone {
            basics.push_str(&format!("\n电话：{ph}"));
        }
        if let Some(em) = &p.email {
            basics.push_str(&format!("\n邮箱：{em}"));
        }
        if let Some(n) = &p.notes {
            if !n.trim().is_empty() {
                basics.push_str(&format!("\n备注：{n}"));
            }
        }

        let basic_changed = sync_section(
            db,
            page_id,
            "基本信息",
            &basics,
            0,
            &[("people", Some(p.id), &basics)],
        )
        .await?;

        // 近期日程
        let events = sqlx::query_as::<_, crate::models::Event>(
            "SELECT * FROM events
             WHERE family_id = $1 AND participants @> ARRAY[$2]::uuid[]
               AND start_time >= NOW() - INTERVAL '7 days'
             ORDER BY start_time ASC LIMIT 20",
        )
        .bind(family_id)
        .bind(p.id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("wiki member events failed: {e}"))?;
        let events_body = if events.is_empty() {
            "近期无日程记录。".to_string()
        } else {
            events
                .iter()
                .map(|e| {
                    format!(
                        "- {}（{}）",
                        e.title,
                        e.start_time.format("%Y-%m-%d %H:%M")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let event_sources: Vec<(&str, Option<Uuid>, &str)> = events
            .iter()
            .map(|e| ("events", Some(e.id), e.title.as_str()))
            .collect();
        let events_changed = sync_section(
            db,
            page_id,
            "近期日程",
            &events_body,
            1,
            &event_sources,
        )
        .await?;

        // 相关待办
        let tasks = sqlx::query_as::<_, crate::models::Task>(
            "SELECT * FROM tasks
             WHERE family_id = $1 AND assigned_to = $2 AND status IN ('todo','in_progress')
             ORDER BY due_date ASC NULLS LAST LIMIT 20",
        )
        .bind(family_id)
        .bind(p.id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("wiki member tasks failed: {e}"))?;
        let tasks_body = if tasks.is_empty() {
            "当前没有负责中的待办。".to_string()
        } else {
            tasks
                .iter()
                .map(|t| {
                    let due = t
                        .due_date
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "无截止时间".to_string());
                    format!("- {}（截止 {due}）", t.title)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let task_sources: Vec<(&str, Option<Uuid>, &str)> = tasks
            .iter()
            .map(|t| ("tasks", Some(t.id), t.title.as_str()))
            .collect();
        let tasks_changed = sync_section(
            db,
            page_id,
            "负责的待办",
            &tasks_body,
            2,
            &task_sources,
        )
        .await?;

        prune_sections(db, page_id, &["基本信息", "近期日程", "负责的待办"]).await?;

        if page_changed || basic_changed || events_changed || tasks_changed {
            changed_pages += 1;
            log_change(
                db,
                family_id,
                page_id,
                entity_type::MEMBER,
                &p.id.to_string(),
                if page_changed { "created" } else { "updated" },
                &format!("成员页已更新：{}", p.name),
            )
            .await?;
        }
    }

    // ---- 物品页 ----
    let things = sqlx::query_as::<_, crate::models::Thing>(
        "SELECT * FROM things WHERE family_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki things load failed: {e}"))?;

    for t in &things {
        let summary = match (&t.category, &t.expiry_date) {
            (Some(c), Some(e)) => format!("{c}，{e} 到期"),
            (Some(c), None) => c.clone(),
            (None, Some(e)) => format!("{e} 到期"),
            (None, None) => "家庭物品".to_string(),
        };
        let (page_id, page_changed) = upsert_page(
            db,
            family_id,
            entity_type::THING,
            &t.id.to_string(),
            &t.name,
            &summary,
        )
        .await?;
        pages += 1;

        let mut body = format!("名称：{}\n分类：{}", t.name, t.category.clone().unwrap_or_default());
        if let Some(l) = &t.location {
            body.push_str(&format!("\n位置：{l}"));
        }
        if t.quantity > 1 {
            body.push_str(&format!("\n数量：{}{}", t.quantity, t.unit.clone().unwrap_or_default()));
        }
        if let Some(e) = t.expiry_date {
            body.push_str(&format!("\n到期日：{e}"));
        }
        body.push_str(&format!("\n状态：{}", t.status));
        if let Some(n) = &t.notes {
            if !n.trim().is_empty() {
                body.push_str(&format!("\n备注：{n}"));
            }
        }
        let changed = sync_section(db, page_id, "物品信息", &body, 0, &[("things", Some(t.id), &body)])
            .await?;
        prune_sections(db, page_id, &["物品信息"]).await?;

        if page_changed || changed {
            changed_pages += 1;
            log_change(
                db,
                family_id,
                page_id,
                entity_type::THING,
                &t.id.to_string(),
                if page_changed { "created" } else { "updated" },
                &format!("物品页已更新：{}", t.name),
            )
            .await?;
        }
    }

    // ---- 空间页 ----
    let spaces = sqlx::query_as::<_, crate::models::Space>(
        "SELECT * FROM spaces WHERE family_id = $1 ORDER BY created_at ASC",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki spaces load failed: {e}"))?;

    for s in &spaces {
        let (page_id, page_changed) = upsert_page(
            db,
            family_id,
            entity_type::SPACE,
            &s.id.to_string(),
            &s.name,
            &s.r#type.clone().unwrap_or_else(|| "空间".to_string()),
        )
        .await?;
        pages += 1;

        let mut body = format!("名称：{}\n类型：{}", s.name, s.r#type.clone().unwrap_or_default());
        if let Some(a) = s.area_sqm {
            body.push_str(&format!("\n面积：{a} 平方米"));
        }
        if let Some(d) = &s.description {
            body.push_str(&format!("\n说明：{d}"));
        }
        let changed = sync_section(db, page_id, "空间信息", &body, 0, &[("spaces", Some(s.id), &body)])
            .await?;
        prune_sections(db, page_id, &["空间信息"]).await?;

        if page_changed || changed {
            changed_pages += 1;
            log_change(
                db,
                family_id,
                page_id,
                entity_type::SPACE,
                &s.id.to_string(),
                if page_changed { "created" } else { "updated" },
                &format!("空间页已更新：{}", s.name),
            )
            .await?;
        }
    }

    Ok(RebuildReport {
        pages,
        changed: changed_pages,
    })
}

// ===== 读取 =====

/// 摘要索引 —— 进提示词的那一层。
pub async fn summary_index(
    db: &PgPool,
    family_id: Uuid,
    limit: i64,
) -> Result<Vec<WikiPage>, String> {
    let rows = sqlx::query_as::<_, WikiPage>(
        "SELECT id, family_id, entity_type, entity_key, title, summary, updated_at
         FROM agent_wiki_pages WHERE family_id = $1
         ORDER BY CASE entity_type
                    WHEN 'family' THEN 0 WHEN 'member' THEN 1 WHEN 'space' THEN 2
                    WHEN 'thing' THEN 3 WHEN 'task' THEN 4 ELSE 5 END, title ASC
         LIMIT $2",
    )
    .bind(family_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki index load failed: {e}"))?;
    Ok(rows)
}

/// 关键词检索（页标题 / 摘要 / 正文）。
pub async fn search(
    db: &PgPool,
    family_id: Uuid,
    query: &str,
    limit: i64,
) -> Result<Vec<WikiHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", q.replace('%', "\\%").replace('_', "\\_"));
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT DISTINCT p.entity_type, p.entity_key, p.title,
                LEFT(s.body, 200)
         FROM agent_wiki_pages p
         JOIN agent_wiki_sections s ON s.page_id = p.id
         WHERE p.family_id = $1
           AND (p.title ILIKE $2 OR p.summary ILIKE $2 OR s.body ILIKE $2)
         LIMIT $3",
    )
    .bind(family_id)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki search failed: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|(entity_type, entity_key, title, body)| WikiHit {
            entity_type,
            entity_key,
            title,
            snippet: body.replace('\n', " ").trim().to_string(),
        })
        .collect())
}

/// 取一个页面的完整内容（含分节与来源）。
pub async fn page_detail(
    db: &PgPool,
    family_id: Uuid,
    entity_type: &str,
    entity_key: &str,
) -> Result<Option<WikiPageDetail>, String> {
    let page = sqlx::query_as::<_, WikiPage>(
        "SELECT id, family_id, entity_type, entity_key, title, summary, updated_at
         FROM agent_wiki_pages
         WHERE family_id = $1 AND entity_type = $2 AND entity_key = $3",
    )
    .bind(family_id)
    .bind(entity_type)
    .bind(entity_key)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("wiki page detail load failed: {e}"))?;

    let Some(page) = page else {
        return Ok(None);
    };

    let sections = sqlx::query_as::<_, WikiSection>(
        "SELECT id, page_id, heading, body, sort_order, updated_at
         FROM agent_wiki_sections WHERE page_id = $1 ORDER BY sort_order ASC, id ASC",
    )
    .bind(page.id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki sections load failed: {e}"))?;

    let mut out = Vec::new();
    for s in sections {
        let sources = sqlx::query_as::<_, WikiSource>(
            "SELECT id, section_id, source_kind, source_id, excerpt
             FROM agent_wiki_sources WHERE section_id = $1 ORDER BY id ASC",
        )
        .bind(s.id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("wiki sources load failed: {e}"))?;
        out.push(WikiSectionWithSources {
            section: s,
            sources,
        });
    }

    Ok(Some(WikiPageDetail { page, sections: out }))
}

/// 变更日志（最近若干条）。
pub async fn changelog(
    db: &PgPool,
    family_id: Uuid,
    limit: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT to_jsonb(t) FROM (
            SELECT entity_type, entity_key, action, detail, created_at
            FROM agent_wiki_changelog WHERE family_id = $1
            ORDER BY created_at DESC LIMIT $2
         ) t",
    )
    .bind(family_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| format!("wiki changelog load failed: {e}"))?;

    Ok(rows)
}

/// 按「键或标题」取页面 —— 给模型用。
///
/// 模型手上通常只有标题（索引里不给 UUID），所以先按 key 精确匹配，
/// 再按标题模糊匹配。
pub async fn page_by_ref(
    db: &PgPool,
    family_id: Uuid,
    entity_type: &str,
    key_or_title: &str,
) -> Result<Option<WikiPageDetail>, String> {
    if let Some(detail) = page_detail(db, family_id, entity_type, key_or_title).await? {
        return Ok(Some(detail));
    }
    let needle = key_or_title.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let pattern = format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_"));
    let page: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, entity_type, entity_key FROM agent_wiki_pages
         WHERE family_id = $1 AND entity_type = $2 AND title ILIKE $3
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(family_id)
    .bind(entity_type)
    .bind(&pattern)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("wiki page ref lookup failed: {e}"))?;

    let Some((_t, et, key)) = page else {
        return Ok(None);
    };
    page_detail(db, family_id, &et, &key).await
}

/// 把摘要索引渲染成提示词片段。
///
/// 只给一行一条 —— 全文按需通过工具取，避免提示词膨胀。
pub fn render_index(pages: &[WikiPage]) -> String {
    if pages.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for p in pages {
        let kind = match p.entity_type.as_str() {
            entity_type::MEMBER => "成员",
            entity_type::THING => "物品",
            entity_type::SPACE => "空间",
            entity_type::EVENT => "日程",
            entity_type::TASK => "待办",
            entity_type::FAMILY => "家庭",
            other => other,
        };
        s.push_str(&format!("- [{}] {}\n", kind, p.title));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(entity_type: &str, title: &str) -> WikiPage {
        WikiPage {
            id: Uuid::new_v4(),
            family_id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            entity_key: Uuid::new_v4().to_string(),
            title: title.to_string(),
            summary: String::new(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn render_index_lists_pages_with_kind() {
        let pages = vec![
            page(entity_type::MEMBER, "张伟"),
            page(entity_type::THING, "牛奶"),
        ];
        let s = render_index(&pages);
        assert!(s.contains("[成员] 张伟"));
        assert!(s.contains("[物品] 牛奶"));
    }

    #[test]
    fn render_index_is_empty_for_no_pages() {
        assert!(render_index(&[]).is_empty());
    }
}
