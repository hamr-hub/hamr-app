//! 审批写操作（Linger approvals 的家庭版对应物）。
//!
//! PROJ-019 的既有不变量：**写操作必须经用户显式确认**。
//! 本模块把这条不变量落成数据结构：
//!
//! 1. 模型**只能提议** —— `propose()` 把意图写进 `agent_write_ops`，
//!    状态 `proposed`，附带一段给用户看的预览文案。业务表（tasks / events）不动。
//! 2. **落库由用户触发** —— `confirm()` 是 HTTP 端点，不是模型工具。
//!    模型不能自己批准自己写的数据。
//! 3. **幂等** —— `operation_id` 是主键。重复确认同一次提议不会写出两条待办，
//!    而是返回第一次的结果。
//!
//! 家庭场景的特殊性：提议可能来自某个成员，但确认可以由**另一个成员**做
//! （比如「爸爸让管家记下孩子要打疫苗」，由妈妈确认）。因此 `member_id` 只是
//! 记录「谁提的」，确认时不做成员一致性校验 —— 真要收紧可以在后续迭代加。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// 写操作种类。
pub mod kind {
    /// 新建待办。
    pub const CREATE_TASK: &str = "create_task";
    /// 新建日程。
    pub const CREATE_EVENT: &str = "create_event";
}

/// 写操作状态。
pub mod status {
    /// 已提议，等待用户确认。
    pub const PROPOSED: &str = "proposed";
    /// 已确认并落库。
    pub const CONFIRMED: &str = "confirmed";
    /// 已被否决。
    pub const REJECTED: &str = "rejected";
    /// 落库失败。
    pub const FAILED: &str = "failed";
    /// 提议挂太久已作废。
    pub const EXPIRED: &str = "expired";
}

/// 提议最长有效天数。
///
/// 超过这个时间还没确认的提议视为作废 —— 挂着三个月的「记得买菜」已经
/// 不是当时那件事了，确认它等于替用户做了一个早就过期的决定。
pub const MAX_PROPOSAL_AGE_DAYS: i64 = 30;

/// 一条写操作。
#[derive(Clone, Debug, Serialize)]
pub struct WriteOp {
    pub operation_id: Uuid,
    pub family_id: Uuid,
    pub member_id: Option<Uuid>,
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub preview: String,
    pub result: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 待办载荷。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskPayload {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assigned_to: Option<Uuid>,
}

/// 日程载荷。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventPayload {
    pub title: String,
    pub start_time: DateTime<Utc>,
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub all_day: Option<bool>,
}

/// 校验并规范化待办载荷。
pub fn parse_task_payload(payload: &Value) -> Result<TaskPayload, String> {
    let p: TaskPayload = serde_json::from_value(payload.clone())
        .map_err(|e| format!("invalid task payload: {e}"))?;
    let title = p.title.trim();
    if title.is_empty() {
        return Err("task title is required".to_string());
    }
    if title.chars().count() > 200 {
        return Err("task title too long (max 200 chars)".to_string());
    }
    let priority = p.priority.as_deref().map(str::trim).map(str::to_lowercase);
    if let Some(pr) = &priority {
        if !matches!(pr.as_str(), "low" | "medium" | "high" | "urgent") {
            return Err(format!("invalid task priority: {pr}"));
        }
    }
    Ok(TaskPayload {
        title: title.to_string(),
        description: p.description,
        due_date: p.due_date,
        priority: priority.or(Some("medium".to_string())),
        assigned_to: p.assigned_to,
    })
}

/// 校验并规范化日程载荷。
pub fn parse_event_payload(payload: &Value) -> Result<EventPayload, String> {
    let p: EventPayload = serde_json::from_value(payload.clone())
        .map_err(|e| format!("invalid event payload: {e}"))?;
    let title = p.title.trim();
    if title.is_empty() {
        return Err("event title is required".to_string());
    }
    if title.chars().count() > 200 {
        return Err("event title too long (max 200 chars)".to_string());
    }
    if let Some(end) = p.end_time {
        if end < p.start_time {
            return Err("event end_time is before start_time".to_string());
        }
    }
    Ok(EventPayload {
        title: title.to_string(),
        start_time: p.start_time,
        end_time: p.end_time,
        location: p.location,
        description: p.description,
        all_day: p.all_day,
    })
}

/// 生成预览文案 —— 用户确认前先看这段。
pub fn build_preview(kind: &str, payload: &Value) -> String {
    match kind {
        kind::CREATE_TASK => match parse_task_payload(payload) {
            Ok(p) => {
                let due = p
                    .due_date
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "未设截止时间".to_string());
                format!(
                    "新建待办「{}」，截止 {}，优先级 {}",
                    p.title,
                    due,
                    p.priority.unwrap_or_else(|| "medium".into())
                )
            }
            Err(e) => format!("（待办载荷不合法：{e}）"),
        },
        kind::CREATE_EVENT => match parse_event_payload(payload) {
            Ok(p) => {
                let when = p.start_time.format("%Y-%m-%d %H:%M").to_string();
                let loc = p
                    .location
                    .as_deref()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| format!("，地点 {l}"))
                    .unwrap_or_default();
                format!("新建日程「{}」，开始 {}{}", p.title, when, loc)
            }
            Err(e) => format!("（日程载荷不合法：{e}）"),
        },
        other => format!("（未知写操作类型：{other}）"),
    }
}

/// 一行写操作记录（数据库列顺序，见 `SELECT_COLS`）。
type WriteOpRow = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    Value,
    String,
    String,
    Option<Value>,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn row_to_op(row: WriteOpRow) -> WriteOp {
    WriteOp {
        operation_id: row.0,
        family_id: row.1,
        member_id: row.2,
        kind: row.3,
        payload: row.4,
        status: row.5,
        preview: row.6,
        result: row.7,
        created_at: row.8,
        updated_at: row.9,
    }
}

const SELECT_COLS: &str = "operation_id, family_id, member_id, kind, payload, status, preview, result, created_at, updated_at";

/// 提议一次写操作。
///
/// `operation_id` 由调用方给出并作为幂等键：同一次提议重放不会产生第二条记录。
pub async fn propose(
    db: &PgPool,
    family_id: Uuid,
    member_id: Option<Uuid>,
    kind: &str,
    payload: Value,
    operation_id: Option<Uuid>,
) -> Result<WriteOp, String> {
    // 载荷先校验 —— 不合法的提议不该进表
    match kind {
        kind::CREATE_TASK => {
            parse_task_payload(&payload)?;
        }
        kind::CREATE_EVENT => {
            parse_event_payload(&payload)?;
        }
        other => return Err(format!("unsupported write op kind: {other}")),
    }

    let op_id = operation_id.unwrap_or_else(Uuid::new_v4);
    let preview = build_preview(kind, &payload);

    let row = sqlx::query_as::<_, WriteOpRow>(
        "INSERT INTO agent_write_ops
           (operation_id, family_id, member_id, kind, payload, status, preview)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (operation_id) DO UPDATE SET
           payload = EXCLUDED.payload, preview = EXCLUDED.preview, updated_at = NOW()
         RETURNING operation_id, family_id, member_id, kind, payload, status, preview, result, created_at, updated_at",
    )
    .bind(op_id)
    .bind(family_id)
    .bind(member_id)
    .bind(kind)
    .bind(payload)
    .bind(status::PROPOSED)
    .bind(&preview)
    .fetch_one(db)
    .await
    .map_err(|e| format!("write op propose failed: {e}"))?;

    Ok(row_to_op(row))
}

/// 确认一次写操作 —— 真正落库。幂等。
pub async fn confirm(
    db: &PgPool,
    family_id: Uuid,
    operation_id: Uuid,
) -> Result<WriteOp, String> {
    let row = sqlx::query_as::<_, WriteOpRow>(
        &format!("SELECT {SELECT_COLS} FROM agent_write_ops WHERE operation_id = $1 AND family_id = $2")
    )
    .bind(operation_id)
    .bind(family_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("write op lookup failed: {e}"))?;

    let Some(row) = row else {
        return Err(format!("write op not found: {operation_id}"));
    };
    let op = row_to_op(row);

    // 幂等：已确认过就直接返回原结果，不重复写业务表
    if op.status == status::CONFIRMED {
        return Ok(op);
    }
    if op.status == status::REJECTED {
        return Err("write op was already rejected".to_string());
    }
    if op.status != status::PROPOSED {
        return Err(format!("write op is not confirmable (status={})", op.status));
    }

    // 过期：提议挂着太久就不再是「刚才那件事」了 —— 确认它等于替用户做了一个
    // 早就过期的决定。过期就作废，不落库。
    if op.created_at < Utc::now() - chrono::Duration::days(MAX_PROPOSAL_AGE_DAYS) {
        sqlx::query(
            "UPDATE agent_write_ops SET status = $2, updated_at = NOW() WHERE operation_id = $1",
        )
        .bind(operation_id)
        .bind(status::EXPIRED)
        .execute(db)
        .await
        .map_err(|e| format!("write op expire failed: {e}"))?;
        return Err(format!(
            "write op expired (older than {MAX_PROPOSAL_AGE_DAYS} days): {operation_id}"
        ));
    }

    let result = match op.kind.as_str() {
        kind::CREATE_TASK => {
            let p = parse_task_payload(&op.payload)?;
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO tasks (family_id, title, description, priority, due_date, assigned_to, status, created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,'todo',$7) RETURNING id",
            )
            .bind(family_id)
            .bind(&p.title)
            .bind(&p.description)
            .bind(&p.priority)
            .bind(p.due_date)
            .bind(p.assigned_to)
            .bind(op.member_id)
            .fetch_one(db)
            .await
            .map_err(|e| format!("create task failed: {e}"))?;
            serde_json::json!({ "id": id, "title": p.title })
        }
        kind::CREATE_EVENT => {
            let p = parse_event_payload(&op.payload)?;
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO events (family_id, title, description, start_time, end_time, location, all_day)
                 VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
            )
            .bind(family_id)
            .bind(&p.title)
            .bind(&p.description)
            .bind(p.start_time)
            .bind(p.end_time)
            .bind(&p.location)
            .bind(p.all_day.unwrap_or(false))
            .fetch_one(db)
            .await
            .map_err(|e| format!("create event failed: {e}"))?;
            serde_json::json!({ "id": id, "title": p.title })
        }
        other => {
            let _ = sqlx::query(
                "UPDATE agent_write_ops SET status = $2, updated_at = NOW() WHERE operation_id = $1",
            )
            .bind(operation_id)
            .bind(status::FAILED)
            .execute(db)
            .await;
            return Err(format!("unsupported write op kind: {other}"));
        }
    };

    let updated = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, String, Value, String, String, Option<Value>, DateTime<Utc>, DateTime<Utc>)>(
        &format!("UPDATE agent_write_ops SET status = $2, result = $3, updated_at = NOW()
                  WHERE operation_id = $1 RETURNING {SELECT_COLS}")
    )
    .bind(operation_id)
    .bind(status::CONFIRMED)
    .bind(&result)
    .fetch_one(db)
    .await
    .map_err(|e| format!("write op confirm update failed: {e}"))?;

    Ok(row_to_op(updated))
}

/// 否决一次写操作。
pub async fn reject(
    db: &PgPool,
    family_id: Uuid,
    operation_id: Uuid,
) -> Result<WriteOp, String> {
    let updated = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, String, Value, String, String, Option<Value>, DateTime<Utc>, DateTime<Utc>)>(
        &format!("UPDATE agent_write_ops SET status = $3, updated_at = NOW()
                  WHERE operation_id = $1 AND family_id = $2
                  RETURNING {SELECT_COLS}")
    )
    .bind(operation_id)
    .bind(family_id)
    .bind(status::REJECTED)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("write op reject failed: {e}"))?;

    updated
        .map(row_to_op)
        .ok_or_else(|| format!("write op not found: {operation_id}"))
}

/// 列出写操作。
pub async fn list_ops(
    db: &PgPool,
    family_id: Uuid,
    status_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<WriteOp>, String> {
    let rows: Vec<WriteOpRow> =
        match status_filter {
            Some(s) => {
                sqlx::query_as(&format!(
                    "SELECT {SELECT_COLS} FROM agent_write_ops
                     WHERE family_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3"
                ))
                .bind(family_id)
                .bind(s)
                .bind(limit)
                .fetch_all(db)
                .await
            }
            None => {
                sqlx::query_as(&format!(
                    "SELECT {SELECT_COLS} FROM agent_write_ops
                     WHERE family_id = $1 ORDER BY created_at DESC LIMIT $2"
                ))
                .bind(family_id)
                .bind(limit)
                .fetch_all(db)
                .await
            }
        }
        .map_err(|e| format!("write op list failed: {e}"))?;

    Ok(rows.into_iter().map(row_to_op).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn task_payload_requires_title() {
        assert!(parse_task_payload(&json!({"title": "  "})).is_err());
        assert!(parse_task_payload(&json!({"description": "无标题"})).is_err());
    }

    #[test]
    fn task_payload_rejects_bad_priority() {
        assert!(parse_task_payload(&json!({"title": "x", "priority": "urgentish"})).is_err());
        let p = parse_task_payload(&json!({"title": "x"})).unwrap();
        assert_eq!(p.priority.as_deref(), Some("medium"), "未指定时取默认优先级");
    }

    #[test]
    fn event_payload_rejects_end_before_start() {
        let p = json!({
            "title": "家长会",
            "start_time": "2026-11-20T10:00:00Z",
            "end_time": "2026-11-20T09:00:00Z"
        });
        assert!(parse_event_payload(&p).is_err());
    }

    #[test]
    fn preview_is_human_readable() {
        let p = json!({"title": "交物业费", "priority": "high"});
        let s = build_preview(kind::CREATE_TASK, &p);
        assert!(s.contains("新建待办「交物业费」"));
        assert!(s.contains("high"));
    }

    #[test]
    fn preview_reports_invalid_payload() {
        let s = build_preview(kind::CREATE_TASK, &json!({}));
        assert!(s.contains("不合法"));
    }

    #[test]
    fn unknown_kind_preview_is_marked() {
        let s = build_preview("delete_everything", &json!({}));
        assert!(s.contains("未知写操作类型"));
    }
}
