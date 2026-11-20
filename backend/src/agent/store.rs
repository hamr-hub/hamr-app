//! 会话与记忆的持久化抽象。
//!
//! 为什么抽象成 trait：Agent 主循环是本模块里最容易出错的逻辑（循环终止、
//! 消息回灌顺序、脱敏时机），而它依赖数据库。把存储抽出来之后，主循环可以在
//! 内存实现上完整单测，不需要起一个 Postgres。
//!
//! 生产用 [`PgStore`]，测试用 [`MemStore`]。

use std::future::Future;
use std::pin::Pin;
#[cfg(test)]
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::proactive::{AgentReminder, ProactiveLogRow, ProactiveRecord, ReminderStatus};
use super::provider::ChatMessage;

/// 存储操作返回的 future。
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

/// 记忆分层（借鉴 Linger 的分层记忆：家规 / 偏好 ≠ 普通事实）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// 长期事实：过敏史、资产、约定事实等。
    Fact,
    /// 偏好线索：出现一次只作线索，重复出现会被巩固为家规。
    Preference,
    /// 家规：高优先级、每轮必带的硬性偏好 / 规则。
    Rule,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Fact => "fact",
            MemoryKind::Preference => "preference",
            MemoryKind::Rule => "rule",
        }
    }

    /// 模型给的值可能非法 —— 非法值交给上层报错回喂模型，而不是悄悄吞掉。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "fact" | "事实" => Some(MemoryKind::Fact),
            "preference" | "偏好" => Some(MemoryKind::Preference),
            "rule" | "家规" | "规则" => Some(MemoryKind::Rule),
            _ => None,
        }
    }

    /// 分层重要度：家规最高。巩固升级与提示词排序都用它。
    pub fn importance(self) -> i16 {
        match self {
            MemoryKind::Fact => 3,
            MemoryKind::Preference => 4,
            MemoryKind::Rule => 5,
        }
    }
}

impl From<crate::models::AgentMemory> for AgentMemory {
    fn from(r: crate::models::AgentMemory) -> Self {
        AgentMemory {
            id: r.id,
            family_id: r.family_id,
            content: r.content,
            source: r.source,
            created_at: r.created_at,
            // DB 有 CHECK 约束兜底；历史脏数据按普通事实处理，不报错。
            kind: MemoryKind::parse(&r.kind).unwrap_or(MemoryKind::Fact),
            importance: r.importance,
            reinforced_count: r.reinforced_count,
            last_reinforced_at: r.last_reinforced_at,
        }
    }
}

/// 一个会话。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub family_id: Uuid,
    /// 归属的家庭成员；为空表示家庭公共会话。
    pub member_id: Option<Uuid>,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 一条长期记忆。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentMemory {
    pub id: Uuid,
    pub family_id: Uuid,
    pub content: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub kind: MemoryKind,
    pub importance: i16,
    /// 被重复确认的次数；偏好达到 2 次会自动升级为家规。
    pub reinforced_count: i32,
    pub last_reinforced_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub enum StoreError {
    Db(String),
    NotFound,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Db(m) => write!(f, "store db error: {m}"),
            StoreError::NotFound => write!(f, "not found"),
        }
    }
}

impl std::error::Error for StoreError {}

/// 会话 / 记忆存储。
pub trait ConversationStore: Send + Sync {
    /// 取得会话 ID；`session_id` 为 None 时新建一个。
    ///
    /// `member_id` 标识这次对话属于哪个家庭成员（None = 家庭公共会话）。
    /// 它只用于标注会话归属与提示词视角，**不作为数据访问边界** ——
    /// 数据边界仍是 family_id。
    fn ensure_session<'a>(
        &'a self,
        family_id: Uuid,
        member_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> StoreFuture<'a, Uuid>;

    /// 读取历史消息（按时间正序，最多 limit 条）。
    fn history<'a>(&'a self, session_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<ChatMessage>>;

    /// 追加一条消息。
    fn append<'a>(&'a self, session_id: Uuid, msg: &'a ChatMessage) -> StoreFuture<'a, ()>;

    /// 列出会话。
    fn list_sessions<'a>(
        &'a self,
        family_id: Uuid,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentSession>>;

    /// 列出长期记忆（家规优先、偏好次之、事实垫后）。
    fn memories<'a>(&'a self, family_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<AgentMemory>>;

    /// 写入或强化一条长期记忆（原子操作）。
    ///
    /// 内容已存在 → 视为用户再次**确认**：强化次数 +1，偏好类就地升级为家规
    /// （clue → rule）；内容不存在 → 新插入一条。
    ///
    /// 两条写入路径（模型工具 `remember` 与手动记忆接口）必须走同一个方法 ——
    /// 否则会出现同一内容两条记忆，提示词里重复出现。
    fn remember<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
        kind: MemoryKind,
    ) -> StoreFuture<'a, AgentMemory>;

    /// 读取某时刻之后的主动判定流水（用于闸门去重与每日条数统计）。
    fn proactive_since<'a>(
        &'a self,
        family_id: Uuid,
        since: DateTime<Utc>,
    ) -> StoreFuture<'a, Vec<ProactiveLogRow>>;

    /// 记录一次主动判定；含提醒 upsert 时返回提醒 ID。
    ///
    /// `(family_id, dedupe_key)` 上有唯一约束：同一事项重复扫描走更新，
    /// 不会给家庭灌出一堆重复提醒。
    fn record_proactive<'a>(&'a self, record: ProactiveRecord) -> StoreFuture<'a, Option<Uuid>>;

    /// 列出提醒（可按状态过滤）。
    fn reminders<'a>(
        &'a self,
        family_id: Uuid,
        status: Option<ReminderStatus>,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentReminder>>;

    /// 更新提醒状态；返回是否真的更新到了。
    fn update_reminder_status<'a>(
        &'a self,
        family_id: Uuid,
        reminder_id: Uuid,
        status: ReminderStatus,
    ) -> StoreFuture<'a, bool>;

    /// 删除会话，返回是否真的删掉了。
    fn delete_session<'a>(&'a self, session_id: Uuid) -> StoreFuture<'a, bool>;
}

// ===== Postgres 实现 =====

pub struct PgStore {
    db: sqlx::PgPool,
}

impl PgStore {
    pub fn new(db: sqlx::PgPool) -> Self {
        Self { db }
    }
}

impl ConversationStore for PgStore {
    fn ensure_session<'a>(
        &'a self,
        family_id: Uuid,
        member_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> StoreFuture<'a, Uuid> {
        Box::pin(async move {
            if let Some(id) = session_id {
                let exists: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM agent_sessions WHERE id = $1")
                        .bind(id)
                        .fetch_optional(&self.db)
                        .await
                        .map_err(|e| StoreError::Db(e.to_string()))?;
                if exists.is_some() {
                    return Ok(id);
                }
                // 会话不存在时新建一个，而不是把错误抛给用户 ——
                // 客户端传了个过期 session_id 不该让整轮对话失败。
            }
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO agent_sessions (id, family_id, member_id) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(family_id)
            .bind(member_id)
            .execute(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(id)
        })
    }

    fn history<'a>(&'a self, session_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<ChatMessage>> {
        Box::pin(async move {
            // 取最近 limit 条，再在内存里翻正序 —— 模型的上下文必须是时间正序。
            let rows: Vec<(String, String, Option<serde_json::Value>, Option<String>)> =
                sqlx::query_as(
                    r#"SELECT role, content, tool_calls, tool_call_id
                   FROM (
                       SELECT role, content, tool_calls, tool_call_id, created_at
                       FROM agent_messages
                       WHERE session_id = $1
                       ORDER BY created_at DESC
                       LIMIT $2
                   ) t
                   ORDER BY created_at ASC"#,
                )
                .bind(session_id)
                .bind(limit)
                .fetch_all(&self.db)
                .await
                .map_err(|e| StoreError::Db(e.to_string()))?;

            Ok(rows.into_iter().map(row_to_message).collect())
        })
    }

    fn append<'a>(&'a self, session_id: Uuid, msg: &'a ChatMessage) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let (role, content, tool_calls, tool_call_id) = message_to_row(msg);
            sqlx::query(
                r#"INSERT INTO agent_messages (session_id, role, content, tool_calls, tool_call_id)
                   VALUES ($1, $2, $3, $4, $5)"#,
            )
            .bind(session_id)
            .bind(role)
            .bind(content)
            .bind(tool_calls)
            .bind(tool_call_id)
            .execute(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            // 顺带刷新会话的 updated_at，让「最近会话」列表按活跃度排序。
            sqlx::query("UPDATE agent_sessions SET updated_at = NOW() WHERE id = $1")
                .bind(session_id)
                .execute(&self.db)
                .await
                .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(())
        })
    }

    fn list_sessions<'a>(
        &'a self,
        family_id: Uuid,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentSession>> {
        Box::pin(async move {
            let rows = sqlx::query_as::<_, crate::models::AgentSession>(
                "SELECT * FROM agent_sessions WHERE family_id = $1 ORDER BY updated_at DESC LIMIT $2",
            )
            .bind(family_id)
            .bind(limit)
            .fetch_all(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(rows
                .into_iter()
                .map(|r| AgentSession {
                    id: r.id,
                    family_id: r.family_id,
                    member_id: r.member_id,
                    title: r.title,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })
                .collect())
        })
    }

    fn memories<'a>(&'a self, family_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<AgentMemory>> {
        Box::pin(async move {
            let rows = sqlx::query_as::<_, crate::models::AgentMemory>(
                "SELECT * FROM agent_memories WHERE family_id = $1
                 ORDER BY CASE kind WHEN 'rule' THEN 0 WHEN 'preference' THEN 1 ELSE 2 END,
                          importance DESC, created_at DESC
                 LIMIT $2",
            )
            .bind(family_id)
            .bind(limit)
            .fetch_all(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(rows.into_iter().map(AgentMemory::from).collect())
        })
    }

    fn remember<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
        kind: MemoryKind,
    ) -> StoreFuture<'a, AgentMemory> {
        Box::pin(async move {
            // 一次语句完成「插入或强化」：内容撞上唯一索引 (family_id, content)
            // 时走 UPDATE 分支 —— 强化次数 +1，偏好升级为家规。
            // SET 表达式读的是更新前的旧行，所以 kind 与 importance 的判断
            // 用的是旧 kind，两处一致。
            let row = sqlx::query_as::<_, crate::models::AgentMemory>(
                r#"INSERT INTO agent_memories (id, family_id, content, source, kind, importance)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (family_id, content) DO UPDATE SET
                     reinforced_count = agent_memories.reinforced_count + 1,
                     last_reinforced_at = NOW(),
                     kind = CASE WHEN agent_memories.kind = 'preference'
                                 THEN 'rule' ELSE agent_memories.kind END,
                     importance = CASE WHEN agent_memories.kind = 'preference'
                                       THEN 5 ELSE agent_memories.importance END
                   RETURNING *"#,
            )
            .bind(Uuid::new_v4())
            .bind(family_id)
            .bind(content)
            .bind(source)
            .bind(kind.as_str())
            .bind(kind.importance())
            .fetch_one(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(AgentMemory::from(row))
        })
    }

    fn proactive_since<'a>(
        &'a self,
        family_id: Uuid,
        since: DateTime<Utc>,
    ) -> StoreFuture<'a, Vec<ProactiveLogRow>> {
        Box::pin(async move {
            let rows = sqlx::query_as::<_, ProactiveLogRow>(
                "SELECT dedupe_key, decision, created_at FROM agent_proactive_log
                 WHERE family_id = $1 AND created_at >= $2
                 ORDER BY created_at ASC",
            )
            .bind(family_id)
            .bind(since)
            .fetch_all(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(rows)
        })
    }

    fn record_proactive<'a>(&'a self, record: ProactiveRecord) -> StoreFuture<'a, Option<Uuid>> {
        Box::pin(async move {
            let reminder_id = if let Some(r) = record.reminder {
                let id = sqlx::query_scalar::<_, Uuid>(
                    r#"INSERT INTO agent_reminders
                         (family_id, kind, subject, state, title, body, action,
                          urgency, status, dedupe_key, fire_at)
                       VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                       ON CONFLICT (family_id, dedupe_key) DO UPDATE SET
                         title = EXCLUDED.title, body = EXCLUDED.body, action = EXCLUDED.action,
                         urgency = EXCLUDED.urgency, status = EXCLUDED.status,
                         fire_at = EXCLUDED.fire_at, state = EXCLUDED.state,
                         updated_at = NOW()
                       RETURNING id"#,
                )
                .bind(record.family_id)
                .bind(&r.kind)
                .bind(r.subject)
                .bind(&r.state)
                .bind(&r.title)
                .bind(&r.body)
                .bind(&r.action)
                .bind(r.urgency.as_str())
                .bind(r.status.as_str())
                .bind(&record.dedupe_key)
                .bind(r.fire_at)
                .fetch_one(&self.db)
                .await
                .map_err(|e| StoreError::Db(e.to_string()))?;
                Some(id)
            } else {
                None
            };

            sqlx::query(
                "INSERT INTO agent_proactive_log
                   (family_id, reminder_id, dedupe_key, decision, reason, created_at)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(record.family_id)
            .bind(reminder_id)
            .bind(&record.dedupe_key)
            .bind(record.decision.as_str())
            .bind(&record.reason)
            .bind(record.at)
            .execute(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;

            Ok(reminder_id)
        })
    }

    fn reminders<'a>(
        &'a self,
        family_id: Uuid,
        status: Option<ReminderStatus>,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentReminder>> {
        Box::pin(async move {
            let rows = match status {
                Some(s) => {
                    sqlx::query_as::<_, AgentReminder>(
                        "SELECT * FROM agent_reminders
                         WHERE family_id = $1 AND status = $2
                         ORDER BY fire_at ASC LIMIT $3",
                    )
                    .bind(family_id)
                    .bind(s.as_str())
                    .bind(limit)
                    .fetch_all(&self.db)
                    .await
                }
                None => {
                    sqlx::query_as::<_, AgentReminder>(
                        "SELECT * FROM agent_reminders
                         WHERE family_id = $1 ORDER BY fire_at ASC LIMIT $2",
                    )
                    .bind(family_id)
                    .bind(limit)
                    .fetch_all(&self.db)
                    .await
                }
            }
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(rows)
        })
    }

    fn update_reminder_status<'a>(
        &'a self,
        family_id: Uuid,
        reminder_id: Uuid,
        status: ReminderStatus,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let res = sqlx::query(
                "UPDATE agent_reminders SET status = $3, updated_at = NOW()
                 WHERE id = $1 AND family_id = $2",
            )
            .bind(reminder_id)
            .bind(family_id)
            .bind(status.as_str())
            .execute(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(res.rows_affected() > 0)
        })
    }

    fn delete_session<'a>(&'a self, session_id: Uuid) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let res = sqlx::query("DELETE FROM agent_sessions WHERE id = $1")
                .bind(session_id)
                .execute(&self.db)
                .await
                .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(res.rows_affected() > 0)
        })
    }
}

/// ChatMessage → (role, content, tool_calls, tool_call_id)
fn message_to_row(
    msg: &ChatMessage,
) -> (String, String, Option<serde_json::Value>, Option<String>) {
    use super::provider::ChatMessage;
    match msg {
        ChatMessage::System { content } => ("system".to_string(), content.clone(), None, None),
        ChatMessage::User { content } => ("user".to_string(), content.clone(), None, None),
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => (
            "assistant".to_string(),
            content.clone(),
            tool_calls
                .as_ref()
                .map(|tc| serde_json::to_value(tc).unwrap_or(serde_json::Value::Null)),
            None,
        ),
        ChatMessage::Tool {
            content,
            tool_call_id,
        } => (
            "tool".to_string(),
            content.clone(),
            None,
            Some(tool_call_id.clone()),
        ),
    }
}

/// (role, content, tool_calls, tool_call_id) → ChatMessage
fn row_to_message(row: (String, String, Option<serde_json::Value>, Option<String>)) -> ChatMessage {
    use super::provider::{ChatMessage, ToolCall};
    let (role, content, tool_calls, tool_call_id) = row;
    match role.as_str() {
        "system" => ChatMessage::System { content },
        "assistant" => ChatMessage::Assistant {
            content,
            tool_calls: tool_calls.and_then(|v| serde_json::from_value::<Vec<ToolCall>>(v).ok()),
        },
        "tool" => ChatMessage::Tool {
            content,
            tool_call_id: tool_call_id.unwrap_or_default(),
        },
        _ => ChatMessage::User { content },
    }
}

// ===== 内存实现（测试用） =====

#[derive(Default)]
#[cfg(test)]
struct MemInner {
    sessions: Vec<AgentSession>,
    messages: Vec<(Uuid, ChatMessage, DateTime<Utc>)>,
    memories: Vec<AgentMemory>,
    reminders: Vec<AgentReminder>,
    proactive_log: Vec<ProactiveLogRowEntry>,
}

/// 内存版主动判定流水（含 family 维度，行模型没有该字段）。
#[cfg(test)]
#[derive(Clone)]
struct ProactiveLogRowEntry {
    family_id: Uuid,
    #[allow(dead_code)]
    reminder_id: Option<Uuid>,
    row: ProactiveLogRow,
}

/// 内存存储。仅用于测试。
#[cfg(test)]
pub struct MemStore {
    inner: Mutex<MemInner>,
}

#[cfg(test)]
impl Default for MemStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(MemInner::default()),
        }
    }
}

#[cfg(test)]
impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl ConversationStore for MemStore {
    fn ensure_session<'a>(
        &'a self,
        family_id: Uuid,
        member_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> StoreFuture<'a, Uuid> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            if let Some(id) = session_id {
                if g.sessions.iter().any(|s| s.id == id) {
                    return Ok(id);
                }
            }
            let now = Utc::now();
            let s = AgentSession {
                id: Uuid::new_v4(),
                family_id,
                member_id,
                title: None,
                created_at: now,
                updated_at: now,
            };
            g.sessions.push(s.clone());
            Ok(s.id)
        })
    }

    fn history<'a>(&'a self, session_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<ChatMessage>> {
        Box::pin(async move {
            let g = self.inner.lock().unwrap();
            let mut msgs: Vec<&(Uuid, ChatMessage, DateTime<Utc>)> = g
                .messages
                .iter()
                .filter(|(sid, _, _)| *sid == session_id)
                .collect();
            msgs.sort_by_key(|(_, _, ts)| *ts);
            if limit > 0 && msgs.len() > limit as usize {
                msgs = msgs.split_off(msgs.len() - limit as usize);
            }
            Ok(msgs.into_iter().map(|(_, m, _)| m.clone()).collect())
        })
    }

    fn append<'a>(&'a self, session_id: Uuid, msg: &'a ChatMessage) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            g.messages.push((session_id, msg.clone(), Utc::now()));
            if let Some(s) = g.sessions.iter_mut().find(|s| s.id == session_id) {
                s.updated_at = Utc::now();
            }
            Ok(())
        })
    }

    fn list_sessions<'a>(
        &'a self,
        family_id: Uuid,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentSession>> {
        Box::pin(async move {
            let g = self.inner.lock().unwrap();
            let mut out: Vec<AgentSession> = g
                .sessions
                .iter()
                .filter(|s| s.family_id == family_id)
                .cloned()
                .collect();
            out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            if limit > 0 && out.len() > limit as usize {
                out.truncate(limit as usize);
            }
            Ok(out)
        })
    }

    fn memories<'a>(&'a self, family_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<AgentMemory>> {
        Box::pin(async move {
            let g = self.inner.lock().unwrap();
            let mut out: Vec<AgentMemory> = g
                .memories
                .iter()
                .filter(|m| m.family_id == family_id)
                .cloned()
                .collect();
            // 与 PgStore 同一套排序：家规 → 偏好 → 事实，同层按重要度与新度。
            out.sort_by(|a, b| {
                let rank = |k: MemoryKind| match k {
                    MemoryKind::Rule => 0,
                    MemoryKind::Preference => 1,
                    MemoryKind::Fact => 2,
                };
                rank(a.kind)
                    .cmp(&rank(b.kind))
                    .then(b.importance.cmp(&a.importance))
                    .then(b.created_at.cmp(&a.created_at))
            });
            if limit > 0 && out.len() > limit as usize {
                out.truncate(limit as usize);
            }
            Ok(out)
        })
    }

    fn remember<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
        kind: MemoryKind,
    ) -> StoreFuture<'a, AgentMemory> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            if let Some(m) = g
                .memories
                .iter_mut()
                .find(|m| m.family_id == family_id && m.content == content)
            {
                // 已存在 → 再次确认：强化并把偏好升级为家规。
                m.reinforced_count += 1;
                m.last_reinforced_at = Some(Utc::now());
                if m.kind == MemoryKind::Preference {
                    m.kind = MemoryKind::Rule;
                    m.importance = MemoryKind::Rule.importance();
                }
                return Ok(m.clone());
            }
            let m = AgentMemory {
                id: Uuid::new_v4(),
                family_id,
                content: content.to_string(),
                source: source.to_string(),
                created_at: Utc::now(),
                kind,
                importance: kind.importance(),
                reinforced_count: 0,
                last_reinforced_at: None,
            };
            g.memories.push(m.clone());
            Ok(m)
        })
    }

    fn proactive_since<'a>(
        &'a self,
        family_id: Uuid,
        since: DateTime<Utc>,
    ) -> StoreFuture<'a, Vec<ProactiveLogRow>> {
        Box::pin(async move {
            let g = self.inner.lock().unwrap();
            Ok(g.proactive_log
                .iter()
                .filter(|e| e.family_id == family_id && e.row.created_at >= since)
                .map(|e| e.row.clone())
                .collect())
        })
    }

    fn record_proactive<'a>(&'a self, record: ProactiveRecord) -> StoreFuture<'a, Option<Uuid>> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            let now = Utc::now();
            let mut reminder_id = None;

            if let Some(u) = record.reminder {
                let id =
                    if let Some(existing) = g.reminders.iter_mut().find(|r| {
                        r.family_id == record.family_id && r.dedupe_key == record.dedupe_key
                    }) {
                        existing.kind = u.kind;
                        existing.subject = u.subject;
                        existing.state = u.state;
                        existing.title = u.title;
                        existing.body = u.body;
                        existing.action = u.action;
                        existing.urgency = u.urgency.as_str().to_string();
                        existing.status = u.status.as_str().to_string();
                        existing.fire_at = u.fire_at;
                        existing.updated_at = now;
                        existing.id
                    } else {
                        let id = Uuid::new_v4();
                        g.reminders.push(AgentReminder {
                            id,
                            family_id: record.family_id,
                            kind: u.kind,
                            subject: u.subject,
                            state: u.state,
                            title: u.title,
                            body: u.body,
                            action: u.action,
                            urgency: u.urgency.as_str().to_string(),
                            status: u.status.as_str().to_string(),
                            dedupe_key: record.dedupe_key.clone(),
                            fire_at: u.fire_at,
                            created_at: now,
                            updated_at: now,
                        });
                        id
                    };
                reminder_id = Some(id);
            }

            g.proactive_log.push(ProactiveLogRowEntry {
                family_id: record.family_id,
                reminder_id,
                row: ProactiveLogRow {
                    dedupe_key: record.dedupe_key,
                    decision: record.decision.as_str().to_string(),
                    created_at: record.at,
                },
            });
            Ok(reminder_id)
        })
    }

    fn reminders<'a>(
        &'a self,
        family_id: Uuid,
        status: Option<ReminderStatus>,
        limit: i64,
    ) -> StoreFuture<'a, Vec<AgentReminder>> {
        Box::pin(async move {
            let g = self.inner.lock().unwrap();
            let mut out: Vec<AgentReminder> = g
                .reminders
                .iter()
                .filter(|r| r.family_id == family_id)
                .filter(|r| status.map(|s| r.status == s.as_str()).unwrap_or(true))
                .cloned()
                .collect();
            out.sort_by_key(|r| r.fire_at);
            if limit > 0 && out.len() > limit as usize {
                out.truncate(limit as usize);
            }
            Ok(out)
        })
    }

    fn update_reminder_status<'a>(
        &'a self,
        family_id: Uuid,
        reminder_id: Uuid,
        status: ReminderStatus,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            let Some(r) = g
                .reminders
                .iter_mut()
                .find(|r| r.id == reminder_id && r.family_id == family_id)
            else {
                return Ok(false);
            };
            r.status = status.as_str().to_string();
            r.updated_at = Utc::now();
            Ok(true)
        })
    }

    fn delete_session<'a>(&'a self, session_id: Uuid) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            let before = g.sessions.len();
            g.sessions.retain(|s| s.id != session_id);
            g.messages.retain(|(sid, _, _)| *sid != session_id);
            Ok(g.sessions.len() != before)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::ChatMessage;

    #[tokio::test]
    async fn mem_store_creates_and_reuses_sessions() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None, None).await.unwrap();
        let again = store.ensure_session(fam, None, Some(sid)).await.unwrap();
        assert_eq!(sid, again, "已存在的会话应被复用");

        // 传一个不存在的 session_id 应该新建，而不是报错
        let fresh = store
            .ensure_session(fam, Some(Uuid::new_v4()), None)
            .await
            .unwrap();
        assert_ne!(fresh, sid);
    }

    #[tokio::test]
    async fn mem_store_roundtrips_messages() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None, None).await.unwrap();
        store.append(sid, &ChatMessage::user("你好")).await.unwrap();
        store
            .append(
                sid,
                &ChatMessage::Assistant {
                    content: "在的".into(),
                    tool_calls: None,
                },
            )
            .await
            .unwrap();
        let hist = store.history(sid, 10).await.unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].text(), "你好");
        assert_eq!(hist[1].text(), "在的");
    }

    #[tokio::test]
    async fn mem_store_history_respects_limit_and_order() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None, None).await.unwrap();
        for i in 0..5 {
            store
                .append(sid, &ChatMessage::user(format!("m{i}")))
                .await
                .unwrap();
        }
        let hist = store.history(sid, 2).await.unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].text(), "m3", "应保留最近两条");
        assert_eq!(hist[1].text(), "m4");
    }

    #[tokio::test]
    async fn mem_store_scopes_sessions_by_family() {
        let store = MemStore::new();
        let fam_a = Uuid::new_v4();
        let fam_b = Uuid::new_v4();
        store.ensure_session(fam_a, None, None).await.unwrap();
        store.ensure_session(fam_b, None, None).await.unwrap();
        let list = store.list_sessions(fam_a, 10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].family_id, fam_a);
    }

    #[tokio::test]
    async fn mem_store_delete_session_removes_messages() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None, None).await.unwrap();
        store.append(sid, &ChatMessage::user("x")).await.unwrap();
        assert!(store.delete_session(sid).await.unwrap());
        assert!(store.history(sid, 10).await.unwrap().is_empty());
        assert!(
            !store.delete_session(sid).await.unwrap(),
            "重复删除应返回 false"
        );
    }

    #[tokio::test]
    async fn mem_store_memories_are_scoped() {
        let store = MemStore::new();
        let fam_a = Uuid::new_v4();
        let fam_b = Uuid::new_v4();
        store
            .remember(fam_a, "孩子对花生过敏", "agent", MemoryKind::Fact)
            .await
            .unwrap();
        store
            .remember(fam_b, "养了只猫", "agent", MemoryKind::Fact)
            .await
            .unwrap();
        let mems = store.memories(fam_a, 10).await.unwrap();
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].content, "孩子对花生过敏");
    }

    #[tokio::test]
    async fn mem_store_recall_orders_rules_first() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        store
            .remember(fam, "一条事实", "agent", MemoryKind::Fact)
            .await
            .unwrap();
        store
            .remember(fam, "一条家规", "agent", MemoryKind::Rule)
            .await
            .unwrap();
        store
            .remember(fam, "一个偏好", "agent", MemoryKind::Preference)
            .await
            .unwrap();
        let mems = store.memories(fam, 10).await.unwrap();
        assert_eq!(mems[0].kind, MemoryKind::Rule, "家规必须排在最前");
        assert_eq!(mems[1].kind, MemoryKind::Preference);
        assert_eq!(mems[2].kind, MemoryKind::Fact);
    }

    #[tokio::test]
    async fn preference_promotes_to_rule_on_second_confirm() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        // 线索第一次：作为偏好写入
        let first = store
            .remember(fam, "晚饭别放香菜", "agent", MemoryKind::Preference)
            .await
            .unwrap();
        assert_eq!(first.reinforced_count, 0, "首次写入不是强化");
        assert_eq!(first.kind, MemoryKind::Preference);

        // 第二次确认：强化，偏好升级为家规（clue → rule）
        let second = store
            .remember(fam, "晚饭别放香菜", "agent", MemoryKind::Preference)
            .await
            .unwrap();
        assert_eq!(second.reinforced_count, 1);
        assert_eq!(second.kind, MemoryKind::Rule, "第二次确认应升级为家规");
        assert!(second.last_reinforced_at.is_some());
        assert_eq!(second.id, first.id, "强化不应产生新行");

        // 只有一条记忆，不能因为重复「记住」而出现两条
        let mems = store.memories(fam, 10).await.unwrap();
        assert_eq!(mems.len(), 1, "重复记住不应新增记忆行");
    }

    #[test]
    fn message_row_roundtrip() {
        let msgs = vec![
            ChatMessage::system("s"),
            ChatMessage::user("u"),
            ChatMessage::Assistant {
                content: "a".into(),
                tool_calls: Some(vec![crate::agent::provider::ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: crate::agent::provider::FunctionCall {
                        name: "list_people".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
            ChatMessage::Tool {
                content: "t".into(),
                tool_call_id: "c1".into(),
            },
        ];
        for m in msgs {
            let row = message_to_row(&m);
            let back = row_to_message(row);
            match (&m, &back) {
                (ChatMessage::System { content: a }, ChatMessage::System { content: b }) => {
                    assert_eq!(a, b)
                }
                (ChatMessage::User { content: a }, ChatMessage::User { content: b }) => {
                    assert_eq!(a, b)
                }
                (
                    ChatMessage::Assistant {
                        content: a,
                        tool_calls: ta,
                    },
                    ChatMessage::Assistant {
                        content: b,
                        tool_calls: tb,
                    },
                ) => {
                    assert_eq!(a, b);
                    assert_eq!(ta.as_ref().map(|x| x.len()), tb.as_ref().map(|x| x.len()));
                    assert_eq!(ta.as_ref().map(|x| &x[0].id), tb.as_ref().map(|x| &x[0].id));
                }
                (
                    ChatMessage::Tool {
                        content: a,
                        tool_call_id: ia,
                    },
                    ChatMessage::Tool {
                        content: b,
                        tool_call_id: ib,
                    },
                ) => {
                    assert_eq!(a, b);
                    assert_eq!(ia, ib);
                }
                _ => panic!("role 在往返中变了"),
            }
        }
    }
}
