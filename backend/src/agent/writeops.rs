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
use sqlx::{PgPool, Postgres, Transaction};
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

async fn lock_op(
    tx: &mut Transaction<'_, Postgres>,
    family_id: Uuid,
    operation_id: Uuid,
) -> Result<WriteOp, String> {
    sqlx::query_as::<_, WriteOpRow>(&format!(
        "SELECT {SELECT_COLS} FROM agent_write_ops
         WHERE operation_id = $1 AND family_id = $2 FOR UPDATE"
    ))
    .bind(operation_id)
    .bind(family_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| format!("write op lookup failed: {e}"))?
    .map(row_to_op)
    .ok_or_else(|| format!("write op not found: {operation_id}"))
}

async fn commit_transition(
    mut tx: Transaction<'_, Postgres>,
    op: &WriteOp,
    new_status: &str,
    result: Option<&Value>,
) -> Result<WriteOp, String> {
    let updated = sqlx::query_as::<_, WriteOpRow>(&format!(
        "UPDATE agent_write_ops
         SET status = $3, result = COALESCE($4, result), updated_at = NOW()
         WHERE operation_id = $1 AND family_id = $2 AND status = $5
         RETURNING {SELECT_COLS}"
    ))
    .bind(op.operation_id)
    .bind(op.family_id)
    .bind(new_status)
    .bind(result)
    .bind(status::PROPOSED)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| format!("write op transition to {new_status} failed: {e}"))?;

    tx.commit()
        .await
        .map_err(|e| format!("write op commit failed: {e}"))?;
    Ok(row_to_op(updated))
}

/// 确认一次写操作 —— 真正落库。幂等。
pub async fn confirm(db: &PgPool, family_id: Uuid, operation_id: Uuid) -> Result<WriteOp, String> {
    let mut tx = db
        .begin()
        .await
        .map_err(|e| format!("write op transaction failed: {e}"))?;
    let op = lock_op(&mut tx, family_id, operation_id).await?;

    match op.status.as_str() {
        status::CONFIRMED => return Ok(op),
        status::REJECTED => return Err("write op was already rejected".to_string()),
        status::PROPOSED => {}
        other => return Err(format!("write op is not confirmable (status={other})")),
    }

    // 过期是终态，提交后再向调用方返回错误。
    if op.created_at < Utc::now() - chrono::Duration::days(MAX_PROPOSAL_AGE_DAYS) {
        commit_transition(tx, &op, status::EXPIRED, None).await?;
        return Err(format!(
            "write op expired (older than {MAX_PROPOSAL_AGE_DAYS} days): {operation_id}"
        ));
    }

    // 业务数据与审批状态使用同一事务，提前返回时 SQLx 会回滚。

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
            .fetch_one(&mut *tx)
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
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| format!("create event failed: {e}"))?;
            serde_json::json!({ "id": id, "title": p.title })
        }
        other => return Err(format!("unsupported write op kind: {other}")),
    };

    commit_transition(tx, &op, status::CONFIRMED, Some(&result)).await
}

/// 否决一次写操作。幂等。
pub async fn reject(db: &PgPool, family_id: Uuid, operation_id: Uuid) -> Result<WriteOp, String> {
    let mut tx = db
        .begin()
        .await
        .map_err(|e| format!("write op transaction failed: {e}"))?;
    let op = lock_op(&mut tx, family_id, operation_id).await?;

    match op.status.as_str() {
        status::REJECTED => Ok(op),
        status::PROPOSED => commit_transition(tx, &op, status::REJECTED, None).await,
        other => Err(format!("write op is not rejectable (status={other})")),
    }
}

/// 列出写操作。
pub async fn list_ops(
    db: &PgPool,
    family_id: Uuid,
    status_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<WriteOp>, String> {
    let rows: Vec<WriteOpRow> = match status_filter {
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
        assert_eq!(
            p.priority.as_deref(),
            Some("medium"),
            "未指定时取默认优先级"
        );
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

    #[sqlx::test]
    #[ignore = "requires a local PostgreSQL DATABASE_URL with database creation privileges"]
    async fn postgres_concurrent_confirm_and_reject(db: PgPool) {
        use std::time::Duration;

        let family_id = Uuid::new_v4();
        let op = propose(
            &db,
            family_id,
            Some(Uuid::new_v4()),
            kind::CREATE_TASK,
            json!({"title": "交物业费"}),
            None,
        )
        .await
        .unwrap();
        let id = op.operation_id;

        // Pause the first confirmation at its task insert, while it holds the approval row lock.
        let mut blocker = db.begin().await.unwrap();
        sqlx::query("LOCK TABLE tasks IN SHARE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let first = confirm(&db, family_id, id);
        tokio::pin!(first);
        tokio::select! {
            result = &mut first => panic!("confirmation finished while insert was blocked: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let insert_waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM pg_locks
                         WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database())
                           AND relation = 'tasks'::regclass AND mode = 'RowExclusiveLock' AND NOT granted)",
                    )
                    .fetch_one(&db)
                    .await
                    .unwrap();
                    if insert_waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }) => result.expect("first confirmation did not reach its task insert"),
        }

        let (first, replay, rejected, released) = tokio::join!(
            first,
            confirm(&db, family_id, id),
            reject(&db, family_id, id),
            blocker.commit(),
        );
        released.unwrap();
        let first = first.unwrap();
        assert_eq!(first.status, status::CONFIRMED);
        assert_eq!(first.family_id, family_id);
        assert_eq!(first.member_id, op.member_id);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(replay.unwrap()).unwrap()
        );
        assert!(rejected.unwrap_err().contains("status=confirmed"));

        let task_id: Uuid = sqlx::query_scalar("SELECT id FROM tasks WHERE family_id = $1")
            .bind(family_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            first.result,
            Some(json!({"id": task_id, "title": "交物业费"}))
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE family_id = $1")
            .bind(family_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test]
    #[ignore = "requires a local PostgreSQL DATABASE_URL with database creation privileges"]
    async fn postgres_confirmation_rolls_back_business_insert(db: PgPool) {
        let family_id = Uuid::new_v4();
        let op = propose(
            &db,
            family_id,
            None,
            kind::CREATE_EVENT,
            json!({"title": "家长会", "start_time": "2026-11-20T10:00:00Z"}),
            None,
        )
        .await
        .unwrap();
        let id = op.operation_id;
        sqlx::query("ALTER TABLE agent_write_ops ADD CONSTRAINT block_confirm CHECK (status <> 'confirmed')")
            .execute(&db)
            .await
            .unwrap();

        let err = confirm(&db, family_id, id).await.unwrap_err();
        assert!(err.contains("block_confirm"), "{err}");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE family_id = $1")
            .bind(family_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let pending = list_ops(&db, family_id, Some(status::PROPOSED), 1)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            serde_json::to_value(&pending[0]).unwrap(),
            serde_json::to_value(op).unwrap()
        );

        sqlx::query("ALTER TABLE agent_write_ops DROP CONSTRAINT block_confirm")
            .execute(&db)
            .await
            .unwrap();
        let confirmed = confirm(&db, family_id, id).await.unwrap();
        let event_id: Uuid = sqlx::query_scalar("SELECT id FROM events WHERE family_id = $1")
            .bind(family_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            confirmed.result,
            Some(json!({"id": event_id, "title": "家长会"}))
        );
    }

    #[sqlx::test]
    #[ignore = "requires a local PostgreSQL DATABASE_URL with database creation privileges"]
    async fn postgres_reject_respects_family_and_terminal_status(db: PgPool) {
        let family_id = Uuid::new_v4();
        let other_family = Uuid::new_v4();
        let op = propose(
            &db,
            family_id,
            None,
            kind::CREATE_TASK,
            json!({"title": "待办"}),
            None,
        )
        .await
        .unwrap();
        let id = op.operation_id;
        let not_found = format!("write op not found: {id}");
        assert_eq!(confirm(&db, other_family, id).await.unwrap_err(), not_found);
        assert_eq!(reject(&db, other_family, id).await.unwrap_err(), not_found);

        let rejected = reject(&db, family_id, id).await.unwrap();
        assert_eq!(rejected.status, status::REJECTED);
        let replay = reject(&db, family_id, id).await.unwrap();
        assert_eq!(
            serde_json::to_value(rejected).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        assert_eq!(
            confirm(&db, family_id, id).await.unwrap_err(),
            "write op was already rejected"
        );

        let expired_id = propose(
            &db,
            family_id,
            None,
            kind::CREATE_TASK,
            json!({"title": "过期"}),
            None,
        )
        .await
        .unwrap()
        .operation_id;
        sqlx::query("UPDATE agent_write_ops SET created_at = $2 WHERE operation_id = $1")
            .bind(expired_id)
            .bind(Utc::now() - chrono::Duration::days(MAX_PROPOSAL_AGE_DAYS + 1))
            .execute(&db)
            .await
            .unwrap();
        assert!(confirm(&db, family_id, expired_id)
            .await
            .unwrap_err()
            .contains("write op expired"));
        assert!(reject(&db, family_id, expired_id)
            .await
            .unwrap_err()
            .contains("status=expired"));
    }
}
