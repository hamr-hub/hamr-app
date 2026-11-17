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

use super::provider::ChatMessage;

/// 存储操作返回的 future。
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

/// 一个会话。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub family_id: Uuid,
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
    fn ensure_session<'a>(
        &'a self,
        family_id: Uuid,
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

    /// 列出长期记忆。
    fn memories<'a>(&'a self, family_id: Uuid, limit: i64) -> StoreFuture<'a, Vec<AgentMemory>>;

    /// 写入一条长期记忆。
    fn add_memory<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
    ) -> StoreFuture<'a, Uuid>;

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
            sqlx::query("INSERT INTO agent_sessions (id, family_id) VALUES ($1, $2)")
                .bind(id)
                .bind(family_id)
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
                "SELECT * FROM agent_memories WHERE family_id = $1 ORDER BY created_at DESC LIMIT $2",
            )
            .bind(family_id)
            .bind(limit)
            .fetch_all(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(rows
                .into_iter()
                .map(|r| AgentMemory {
                    id: r.id,
                    family_id: r.family_id,
                    content: r.content,
                    source: r.source,
                    created_at: r.created_at,
                })
                .collect())
        })
    }

    fn add_memory<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
    ) -> StoreFuture<'a, Uuid> {
        Box::pin(async move {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO agent_memories (id, family_id, content, source) VALUES ($1, $2, $3, $4)",
            )
            .bind(id)
            .bind(family_id)
            .bind(content)
            .bind(source)
            .execute(&self.db)
            .await
            .map_err(|e| StoreError::Db(e.to_string()))?;
            Ok(id)
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
            out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            if limit > 0 && out.len() > limit as usize {
                out.truncate(limit as usize);
            }
            Ok(out)
        })
    }

    fn add_memory<'a>(
        &'a self,
        family_id: Uuid,
        content: &'a str,
        source: &'a str,
    ) -> StoreFuture<'a, Uuid> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            let m = AgentMemory {
                id: Uuid::new_v4(),
                family_id,
                content: content.to_string(),
                source: source.to_string(),
                created_at: Utc::now(),
            };
            let id = m.id;
            g.memories.push(m);
            Ok(id)
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
        let sid = store.ensure_session(fam, None).await.unwrap();
        let again = store.ensure_session(fam, Some(sid)).await.unwrap();
        assert_eq!(sid, again, "已存在的会话应被复用");

        // 传一个不存在的 session_id 应该新建，而不是报错
        let fresh = store
            .ensure_session(fam, Some(Uuid::new_v4()))
            .await
            .unwrap();
        assert_ne!(fresh, sid);
    }

    #[tokio::test]
    async fn mem_store_roundtrips_messages() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None).await.unwrap();
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
        let sid = store.ensure_session(fam, None).await.unwrap();
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
        store.ensure_session(fam_a, None).await.unwrap();
        store.ensure_session(fam_b, None).await.unwrap();
        let list = store.list_sessions(fam_a, 10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].family_id, fam_a);
    }

    #[tokio::test]
    async fn mem_store_delete_session_removes_messages() {
        let store = MemStore::new();
        let fam = Uuid::new_v4();
        let sid = store.ensure_session(fam, None).await.unwrap();
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
            .add_memory(fam_a, "孩子对花生过敏", "agent")
            .await
            .unwrap();
        store.add_memory(fam_b, "养了只猫", "agent").await.unwrap();
        let mems = store.memories(fam_a, 10).await.unwrap();
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].content, "孩子对花生过敏");
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
