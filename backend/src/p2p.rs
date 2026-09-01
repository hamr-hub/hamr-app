/// p2p.rs — HamR P2P 同步模块
///
/// 实现三层功能：
///   P1: mDNS 设备发现（局域网自动发现）
///   P2: 节点管理（Ed25519 身份持久化、设备列表、/p2p/peers /p2p/status API 数据）
///   P3: 数据同步（SyncMessage 广播 + last-write-wins 真实落库 + sync_log 幂等去重）
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use libp2p::{
    futures::StreamExt,
    gossipsub,
    gossipsub::IdentTopic,
    identity,
    mdns,
    noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    fs,
    sync::{mpsc, oneshot},
};

// ─────────────────────────────────────────────
// 数据结构
// ─────────────────────────────────────────────

/// P3: 同步消息（跨设备广播的数据变更事件）
///
/// 采用 last-write-wins 策略：timestamp 最大的消息获胜。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMessage {
    /// 全局唯一同步 ID（UUID v4）
    pub sync_id: String,
    /// 发送方 PeerId（字符串）
    pub peer_id: String,
    /// 操作的数据库表名（people/events/tasks/things/spaces）
    pub table: String,
    /// 操作类型（insert / update / delete）
    pub operation: String,
    /// 数据行的主键
    pub record_id: String,
    /// 操作的完整数据（JSON，delete 时可为 null）
    pub data: serde_json::Value,
    /// 操作时间戳（毫秒 Unix epoch，用于 LWW 冲突解决）
    pub timestamp: i64,
    /// 发送方设备 DID（did:key:...）
    pub device_did: String,
    /// 数据签名（base64，可选 —— Phase2 启用）
    pub signature: Option<String>,
}

impl SyncMessage {
    pub fn new(
        peer_id: String,
        device_did: String,
        table: impl Into<String>,
        operation: impl Into<String>,
        record_id: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            sync_id: uuid::Uuid::new_v4().to_string(),
            peer_id,
            table: table.into(),
            operation: operation.into(),
            record_id: record_id.into(),
            data,
            timestamp: Utc::now().timestamp_millis(),
            device_did,
            signature: None,
        }
    }
}

/// P3: 落库用的同步记录（HTTP `POST /api/v1/sync/incoming` 的线上契约）
///
/// 与 [`SyncMessage`] 的关系：`SyncMessage` 是 gossipsub 线格式（带 peer_id /
/// device_did / signature 等传输元信息），`SyncRecord` 只保留 LWW 合并真正需要
/// 的五个字段。两种命名通过 serde alias 互通，因此同一个 handler 既能吃
/// `{sync_id, table, primary_key, payload, ts_ms}`，也能直接吃序列化后的
/// `SyncMessage`（`record_id` / `data` / `timestamp`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    /// 全局唯一同步 ID —— 幂等键
    pub sync_id: String,
    /// 目标业务表（必须在 [`SYNCABLE_TABLES`] 白名单内）
    #[serde(alias = "table_name")]
    pub table: String,
    /// 业务行主键（UUID 字符串）
    #[serde(alias = "record_id")]
    pub primary_key: String,
    /// 整行数据（JSON object；delete 时可为 null）
    #[serde(alias = "data", default)]
    pub payload: serde_json::Value,
    /// 发生时间（毫秒 Unix epoch）—— LWW 比较基准
    #[serde(alias = "timestamp")]
    pub ts_ms: i64,
    /// 操作类型（upsert / insert / update / delete）
    #[serde(default = "default_operation")]
    pub operation: String,
}

fn default_operation() -> String {
    "upsert".to_string()
}

impl SyncRecord {
    /// delete 语义：不写入 payload，只删行（仍写 sync_log 以保持幂等）
    pub fn is_delete(&self) -> bool {
        self.operation.eq_ignore_ascii_case("delete")
    }
}

impl From<&SyncMessage> for SyncRecord {
    fn from(msg: &SyncMessage) -> Self {
        Self {
            sync_id: msg.sync_id.clone(),
            table: msg.table.clone(),
            primary_key: msg.record_id.clone(),
            payload: msg.data.clone(),
            ts_ms: msg.timestamp,
            operation: msg.operation.clone(),
        }
    }
}

/// 一条同步记录的处理结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
    /// 已写库（新行或更新的版本）
    Applied,
    /// sync_id 已在 sync_log 中 —— 重发，ack 但不写
    SkippedDuplicate,
    /// 本地版本更新（timestamp >= 来件）—— LWW 判负，ack 但不写
    SkippedStale,
}

impl SyncOutcome {
    pub fn reason(&self) -> &'static str {
        match self {
            SyncOutcome::Applied => "applied",
            SyncOutcome::SkippedDuplicate => "duplicate_sync_id",
            SyncOutcome::SkippedStale => "stale_timestamp",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// 表名不在白名单 —— 拒绝，绝不拼进 SQL
    #[error("unsupported sync table '{0}'")]
    UnsupportedTable(String),
    /// 记录本身不合法（空主键 / 主键非 UUID / payload 不是 object 等）
    #[error("invalid sync record: {0}")]
    InvalidRecord(String),
    /// 落库失败（连接断开、约束冲突、事务回滚……）
    #[error("sync store failure: {0}")]
    Store(String),
}

impl From<sqlx::Error> for SyncError {
    fn from(e: sqlx::Error) -> Self {
        SyncError::Store(e.to_string())
    }
}

/// 允许被 P2P sync 写入的表白名单。
///
/// **安全边界**：SQL 标识符无法参数化，表名只能字符串拼接进语句，所以
/// `record.table` 必须先过这张白名单 —— 否则对端（或任何能打到
/// `/api/v1/sync/incoming` 的调用方）就能借 `table` 字段做 SQL 注入。
pub const SYNCABLE_TABLES: [&str; 5] = ["people", "events", "tasks", "things", "spaces"];

pub fn is_syncable_table(table: &str) -> bool {
    SYNCABLE_TABLES.contains(&table)
}

/// P3: 同步落库抽象
///
/// 生产实现是 `sqlx::PgPool`（见下方 `impl SyncStore for PgPool`）；单测用
/// 内存 mock，因此 LWW / 幂等逻辑不需要真实 Postgres 就能验证。
///
/// 这里用显式 RPITIT（`-> impl Future + Send`）而不是 `async fn`，是为了让
/// 返回的 future 带上 `Send`，否则 axum handler 的 future 不满足 `Send`。
pub trait SyncStore {
    /// sync_id 是否已在 sync_log 中（幂等去重）
    fn is_applied(&self, sync_id: &str) -> impl Future<Output = Result<bool, SyncError>> + Send;

    /// 读取本地当前版本时间戳（毫秒）；行不存在返回 None
    fn current_ts_ms(
        &self,
        table: &str,
        primary_key: &str,
    ) -> impl Future<Output = Result<Option<i64>, SyncError>> + Send;

    /// 单事务内落库：整行替换（或删除）+ 写 sync_log
    fn apply_record(&self, record: &SyncRecord)
        -> impl Future<Output = Result<(), SyncError>> + Send;
}

/// P3: 处理一条收到的同步记录 —— 幂等去重 + last-write-wins 合并 + 真实落库
///
/// 判定顺序（任一步失败都 `error!` 并向上抛 `Err`，不静默吞）：
///   1. 校验：表名白名单、sync_id/primary_key 非空、payload 形状
///   2. 幂等：sync_id 已在 sync_log → `SkippedDuplicate`（ack 不写）
///   3. LWW ：来件 ts_ms <= 本地 updated_at → `SkippedStale`（ack 不写）
///   4. 落库：整行替换 + 写 sync_log（同一事务）→ `Applied`
///
/// 时间戳相等按 `SkippedStale` 处理：同一逻辑版本重复投递时保持本地不动，
/// 避免两端时钟同刻互相覆盖导致的写放大。
pub async fn handle_incoming_sync<S>(
    store: &S,
    record: &SyncRecord,
) -> Result<SyncOutcome, SyncError>
where
    S: SyncStore + Sync,
{
    // ── 1. 校验（在任何 SQL 之前） ──────────────────────────────
    if !is_syncable_table(&record.table) {
        tracing::error!(
            "[P2P][LWW] rejected sync_id={} — table '{}' not in whitelist {:?}",
            record.sync_id,
            record.table,
            SYNCABLE_TABLES
        );
        return Err(SyncError::UnsupportedTable(record.table.clone()));
    }
    if record.sync_id.trim().is_empty() {
        return Err(SyncError::InvalidRecord("sync_id is empty".into()));
    }
    if record.primary_key.trim().is_empty() {
        return Err(SyncError::InvalidRecord("primary_key is empty".into()));
    }
    if !record.is_delete() && !record.payload.is_object() {
        return Err(SyncError::InvalidRecord(format!(
            "payload of non-delete record must be a JSON object (sync_id={})",
            record.sync_id
        )));
    }

    // ── 2. 幂等去重：sync_log 里已有就只 ack ────────────────────
    let already = store.is_applied(&record.sync_id).await.map_err(|e| {
        tracing::error!(
            "[P2P][LWW] sync_log lookup failed for sync_id={}: {}",
            record.sync_id,
            e
        );
        e
    })?;
    if already {
        tracing::debug!(
            "[P2P][LWW] duplicate sync_id={} (table={}) — ack without write",
            record.sync_id,
            record.table
        );
        return Ok(SyncOutcome::SkippedDuplicate);
    }

    // ── 3. LWW：读本地版本再比时间戳 ────────────────────────────
    let local_ts = store
        .current_ts_ms(&record.table, &record.primary_key)
        .await
        .map_err(|e| {
            tracing::error!(
                "[P2P][LWW] current version lookup failed for {}#{}: {}",
                record.table,
                record.primary_key,
                e
            );
            e
        })?;
    if let Some(local_ts) = local_ts {
        if record.ts_ms <= local_ts {
            tracing::info!(
                "[P2P][LWW] stale sync_id={} ({}#{}) incoming_ts={} <= local_ts={} — ack without write",
                record.sync_id,
                record.table,
                record.primary_key,
                record.ts_ms,
                local_ts
            );
            return Ok(SyncOutcome::SkippedStale);
        }
    }

    // ── 4. 真实落库：整行替换 + sync_log（同一事务） ────────────
    store.apply_record(record).await.map_err(|e| {
        tracing::error!(
            "[P2P][LWW] apply failed for sync_id={} ({}#{} op={} ts={}): {}",
            record.sync_id,
            record.table,
            record.primary_key,
            record.operation,
            record.ts_ms,
            e
        );
        e
    })?;

    tracing::info!(
        "[P2P][LWW] applied sync_id={} {}#{} op={} ts={} (local_ts={:?})",
        record.sync_id,
        record.table,
        record.primary_key,
        record.operation,
        record.ts_ms,
        local_ts
    );
    Ok(SyncOutcome::Applied)
}

/// 主键必须是 UUID —— 业务五表的 `id` 都是 UUID 列，提前解析可把
/// "不是 UUID" 归到 InvalidRecord(422)，而不是让 Postgres 抛类型错(500)。
fn parse_primary_key(raw: &str) -> Result<uuid::Uuid, SyncError> {
    uuid::Uuid::parse_str(raw)
        .map_err(|e| SyncError::InvalidRecord(format!("primary_key '{raw}' is not a UUID: {e}")))
}

impl SyncStore for sqlx::PgPool {
    async fn is_applied(&self, sync_id: &str) -> Result<bool, SyncError> {
        let hit: Option<(String,)> =
            sqlx::query_as("SELECT sync_id FROM sync_log WHERE sync_id = $1")
                .bind(sync_id)
                .fetch_optional(self)
                .await?;
        Ok(hit.is_some())
    }

    async fn current_ts_ms(
        &self,
        table: &str,
        primary_key: &str,
    ) -> Result<Option<i64>, SyncError> {
        if !is_syncable_table(table) {
            return Err(SyncError::UnsupportedTable(table.to_string()));
        }
        let pk = parse_primary_key(primary_key)?;
        // table 已过白名单，此处拼接安全；pk 走 bind 参数
        let sql = format!("SELECT updated_at FROM {table} WHERE id = $1");
        // updated_at 按 Option 解码：万一某行的时间戳为 NULL，也只是"本地版本未知"
        // → 让来件获胜，而不是让整条同步在解码阶段炸掉。
        let row: Option<(Option<DateTime<Utc>>,)> =
            sqlx::query_as(&sql).bind(pk).fetch_optional(self).await?;
        Ok(row.and_then(|(ts,)| ts).map(|ts| ts.timestamp_millis()))
    }

    async fn apply_record(&self, record: &SyncRecord) -> Result<(), SyncError> {
        if !is_syncable_table(&record.table) {
            return Err(SyncError::UnsupportedTable(record.table.clone()));
        }
        let pk = parse_primary_key(&record.primary_key)?;
        let table = &record.table;

        let mut tx = self.begin().await?;

        // LWW 语义 = 整行替换：先删旧行，再按 payload 重建，避免逐列
        // COALESCE 时「对端把某列显式清空」被误当成「没带这列」。
        sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
            .bind(pk)
            .execute(&mut *tx)
            .await?;

        if !record.is_delete() {
            // jsonb_populate_record 按列名映射 payload，多余的 key 忽略、
            // 缺的列取 NULL —— 无需为五张表各写一份 INSERT。
            sqlx::query(&format!(
                "INSERT INTO {table} SELECT * FROM jsonb_populate_record(NULL::{table}, $1::jsonb)"
            ))
            .bind(&record.payload)
            .execute(&mut *tx)
            .await?;
        }

        // 同事务写台账：提交后这条 sync_id 就永久幂等
        sqlx::query(
            "INSERT INTO sync_log (sync_id, table_name, primary_key)
             VALUES ($1, $2, $3) ON CONFLICT (sync_id) DO NOTHING",
        )
        .bind(&record.sync_id)
        .bind(&record.table)
        .bind(&record.primary_key)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

/// P2: 已发现的对端设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// libp2p PeerId（字符串）
    pub peer_id: String,
    /// 已知地址列表（multiaddr）
    pub addresses: Vec<String>,
    /// 首次发现时间
    pub discovered_at: DateTime<Utc>,
    /// 最后在线时间（通过 mDNS 或 Ping 更新）
    pub last_seen: DateTime<Utc>,
    /// 连接状态
    pub connected: bool,
}

/// P2: 本节点状态快照（供 /p2p/status API 使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub peer_id: String,
    pub listen_addresses: Vec<String>,
    pub connected_peers: usize,
    pub known_peers: usize,
    pub gossipsub_topic: String,
    pub uptime_seconds: u64,
}

// ─────────────────────────────────────────────
// libp2p NetworkBehaviour 定义
// ─────────────────────────────────────────────

#[derive(NetworkBehaviour)]
pub struct HamrBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

// ─────────────────────────────────────────────
// 身份持久化（P2）
// ─────────────────────────────────────────────

/// 持久化存储的节点密钥（Ed25519）
#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    /// Ed25519 密钥对的 protobuf 编码（base64）
    keypair_proto_b64: String,
}

/// 从 `~/.hamr/identity` 加载或生成 libp2p 身份密钥
pub async fn load_or_create_keypair(data_dir: &str) -> Result<identity::Keypair> {
    let identity_path = Path::new(data_dir).join("identity");

    if identity_path.exists() {
        let raw = fs::read(&identity_path)
            .await
            .context("Failed to read identity file")?;
        let stored: StoredIdentity =
            serde_json::from_slice(&raw).context("Failed to parse identity file")?;
        let proto_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &stored.keypair_proto_b64,
        )
        .context("Failed to decode keypair")?;
        let kp = identity::Keypair::from_protobuf_encoding(&proto_bytes)
            .context("Failed to decode keypair from protobuf")?;
        tracing::info!("Loaded P2P identity: {}", PeerId::from(kp.public()));
        Ok(kp)
    } else {
        // 生成新的 Ed25519 密钥对
        let keypair = identity::Keypair::generate_ed25519();
        let proto_bytes = keypair
            .to_protobuf_encoding()
            .context("Failed to encode keypair")?;
        let stored = StoredIdentity {
            keypair_proto_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &proto_bytes,
            ),
        };

        // 确保目录存在
        if let Some(parent) = identity_path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("Failed to create data directory")?;
        }
        let content = serde_json::to_vec_pretty(&stored)?;
        fs::write(&identity_path, content)
            .await
            .context("Failed to write identity file")?;

        tracing::info!(
            "Generated new P2P identity: {}",
            PeerId::from(keypair.public())
        );
        Ok(keypair)
    }
}

// ─────────────────────────────────────────────
// P2P 节点命令（用于 API Handler 查询状态）
// ─────────────────────────────────────────────

pub enum P2PCommand {
    /// 获取已知 peers 列表
    GetPeers(oneshot::Sender<Vec<PeerInfo>>),
    /// 获取本节点状态
    GetStatus(oneshot::Sender<NodeStatus>),
    /// 广播一条同步消息
    Publish(SyncMessage),
}

// ─────────────────────────────────────────────
// P2P 节点（主体）
// ─────────────────────────────────────────────

pub struct P2PNode {
    pub peer_id: PeerId,
    swarm: Swarm<HamrBehaviour>,
    topic: IdentTopic,
    /// 已知 peers 表（peer_id -> PeerInfo）
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    /// 本节点监听地址
    listen_addresses: Arc<RwLock<Vec<String>>>,
    start_time: std::time::Instant,
    /// 落库句柄；None = 只广播不落库（无 DB 的降级模式）
    store: Option<sqlx::PgPool>,
}

impl P2PNode {
    /// 创建 P2P 节点
    ///
    /// `data_dir`：身份文件存储目录（如 `~/.hamr`）
    /// `store`   ：收到同步消息后落库用的连接池；传 None 则只记日志不写
    pub async fn new(data_dir: &str, store: Option<sqlx::PgPool>) -> Result<Self> {
        let keypair = load_or_create_keypair(data_dir).await?;
        let peer_id = PeerId::from(keypair.public());

        // GossipSub 配置
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(gossipsub::ValidationMode::Permissive)
            .message_id_fn(|msg| {
                // 用消息内容哈希作为 ID，防重复
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut s = DefaultHasher::new();
                msg.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            })
            .build()
            .map_err(|e| anyhow::anyhow!("GossipSub config error: {}", e))?;

        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(keypair.clone()),
            gossipsub_config,
        )
        .map_err(|e| anyhow::anyhow!("GossipSub init error: {}", e))?;

        // mDNS（局域网设备发现）
        let mdns = mdns::tokio::Behaviour::new(
            mdns::Config {
                ttl: Duration::from_secs(300),
                query_interval: Duration::from_secs(20),
                ..Default::default()
            },
            peer_id,
        )
        .map_err(|e| anyhow::anyhow!("mDNS init error: {}", e))?;

        // 构建 Swarm
        let swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| anyhow::anyhow!("TCP transport error: {}", e))?
            .with_behaviour(|_key| HamrBehaviour { gossipsub, mdns })
            .map_err(|e| anyhow::anyhow!("Behaviour build error: {}", e))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        let topic = IdentTopic::new("hamr-family-sync");

        Ok(Self {
            peer_id,
            swarm,
            topic,
            peers: Arc::new(RwLock::new(HashMap::new())),
            listen_addresses: Arc::new(RwLock::new(Vec::new())),
            start_time: std::time::Instant::now(),
            store,
        })
    }

    /// 返回 Arc 引用，供 API handler 克隆后查询
    pub fn peers_handle(&self) -> Arc<RwLock<HashMap<String, PeerInfo>>> {
        self.peers.clone()
    }

    pub fn listen_addresses_handle(&self) -> Arc<RwLock<Vec<String>>> {
        self.listen_addresses.clone()
    }

    /// 启动事件循环
    ///
    /// - `sync_rx`：接收来自业务层的 SyncMessage 并广播
    /// - `cmd_rx` ：接收来自 API handler 的查询命令
    pub async fn run(
        mut self,
        mut sync_rx: mpsc::Receiver<SyncMessage>,
        mut cmd_rx: mpsc::Receiver<P2PCommand>,
    ) -> Result<()> {
        // 监听随机 TCP 端口
        self.swarm
            .listen_on("/ip4/0.0.0.0/tcp/0".parse()?)
            .context("Failed to start TCP listener")?;

        // 订阅家庭同步 topic
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&self.topic)
            .map_err(|e| anyhow::anyhow!("GossipSub subscribe error: {}", e))?;

        tracing::info!(
            "[P2P] Node started. peer_id={}, topic={}",
            self.peer_id,
            self.topic
        );

        loop {
            tokio::select! {
                // ── Swarm 事件处理 ──────────────────────────────
                event = self.swarm.select_next_some() => {
                    match event {
                        // P1: mDNS 发现新设备
                        SwarmEvent::Behaviour(HamrBehaviourEvent::Mdns(
                            mdns::Event::Discovered(list),
                        )) => {
                            for (peer_id, multiaddr) in list {
                                tracing::info!("[P2P][mDNS] Discovered: {} @ {}", peer_id, multiaddr);
                                self.upsert_peer(peer_id, multiaddr, false);
                                // 主动拨号
                                if let Err(e) = self.swarm.dial(peer_id) {
                                    tracing::warn!("[P2P] Dial {} failed: {}", peer_id, e);
                                }
                            }
                        }

                        // P1: mDNS 设备离线
                        SwarmEvent::Behaviour(HamrBehaviourEvent::Mdns(
                            mdns::Event::Expired(list),
                        )) => {
                            for (peer_id, _) in list {
                                tracing::info!("[P2P][mDNS] Peer expired: {}", peer_id);
                                if let Ok(mut peers) = self.peers.write() {
                                    if let Some(p) = peers.get_mut(&peer_id.to_string()) {
                                        p.connected = false;
                                    }
                                }
                            }
                        }

                        // P3: 收到同步消息
                        SwarmEvent::Behaviour(HamrBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message {
                                propagation_source,
                                message,
                                ..
                            },
                        )) => {
                            match serde_json::from_slice::<SyncMessage>(&message.data) {
                                Ok(sync_msg) => {
                                    tracing::info!(
                                        "[P2P][Sync] Received {} on table '{}' from {}",
                                        sync_msg.operation,
                                        sync_msg.table,
                                        propagation_source
                                    );
                                    self.persist_incoming_sync(&sync_msg);
                                }
                                Err(e) => {
                                    tracing::warn!("[P2P] Failed to decode sync message: {}", e);
                                }
                            }
                        }

                        // P2: 连接建立
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            tracing::info!("[P2P] Connected: {}", peer_id);
                            let addr = endpoint.get_remote_address().clone();
                            self.upsert_peer(peer_id, addr, true);
                            // 将新 peer 加入 gossipsub mesh
                            self.swarm
                                .behaviour_mut()
                                .gossipsub
                                .add_explicit_peer(&peer_id);
                        }

                        // P2: 连接断开
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("[P2P] Disconnected: {}", peer_id);
                            if let Ok(mut peers) = self.peers.write() {
                                if let Some(p) = peers.get_mut(&peer_id.to_string()) {
                                    p.connected = false;
                                }
                            }
                        }

                        // 监听地址确认
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("[P2P] Listening on {}", address);
                            if let Ok(mut addrs) = self.listen_addresses.write() {
                                addrs.push(address.to_string());
                            }
                        }

                        _ => {}
                    }
                }

                // ── 业务层发来的广播请求 ────────────────────────
                Some(msg) = sync_rx.recv() => {
                    match serde_json::to_vec(&msg) {
                        Ok(data) => {
                            match self.swarm
                                .behaviour_mut()
                                .gossipsub
                                .publish(self.topic.clone(), data)
                            {
                                Ok(_) => tracing::debug!(
                                    "[P2P][Sync] Published {} on '{}' (id={})",
                                    msg.operation, msg.table, msg.sync_id
                                ),
                                Err(gossipsub::PublishError::InsufficientPeers) => {
                                    // 没有在线 peer 时静默处理（单设备模式正常）
                                    tracing::debug!("[P2P][Sync] No peers to publish to");
                                }
                                Err(e) => {
                                    tracing::warn!("[P2P][Sync] Publish error: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[P2P][Sync] Serialize error: {}", e);
                        }
                    }
                }

                // ── API 查询命令 ────────────────────────────────
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        P2PCommand::GetPeers(tx) => {
                            let list = self.peers
                                .read()
                                .map(|p| p.values().cloned().collect::<Vec<_>>())
                                .unwrap_or_default();
                            let _ = tx.send(list);
                        }

                        P2PCommand::GetStatus(tx) => {
                            let listen_addrs = self.listen_addresses
                                .read()
                                .map(|v| v.clone())
                                .unwrap_or_default();
                            let connected = self.peers
                                .read()
                                .map(|p| p.values().filter(|pi| pi.connected).count())
                                .unwrap_or(0);
                            let known = self.peers
                                .read()
                                .map(|p| p.len())
                                .unwrap_or(0);

                            let status = NodeStatus {
                                peer_id: self.peer_id.to_string(),
                                listen_addresses: listen_addrs,
                                connected_peers: connected,
                                known_peers: known,
                                gossipsub_topic: self.topic.to_string(),
                                uptime_seconds: self.start_time.elapsed().as_secs(),
                            };
                            let _ = tx.send(status);
                        }

                        P2PCommand::Publish(msg) => {
                            if let Ok(data) = serde_json::to_vec(&msg) {
                                let _ = self.swarm
                                    .behaviour_mut()
                                    .gossipsub
                                    .publish(self.topic.clone(), data);
                            }
                        }
                    }
                }
            }
        }
    }

    // ── 内部辅助方法 ──────────────────────────────────────────

    /// 更新或插入 peer 信息
    fn upsert_peer(&self, peer_id: PeerId, addr: Multiaddr, connected: bool) {
        if let Ok(mut peers) = self.peers.write() {
            let key = peer_id.to_string();
            let now = Utc::now();
            let entry = peers.entry(key).or_insert_with(|| PeerInfo {
                peer_id: peer_id.to_string(),
                addresses: Vec::new(),
                discovered_at: now,
                last_seen: now,
                connected,
            });
            entry.last_seen = now;
            entry.connected = connected;
            let addr_str = addr.to_string();
            if !entry.addresses.contains(&addr_str) {
                entry.addresses.push(addr_str);
            }
        }
    }

    /// P3: 把收到的同步消息交给 LWW 合并落库
    ///
    /// 落库是 I/O，不能阻在 `select!` 循环里（否则一次慢查询就卡住整个 swarm 的
    /// mDNS/gossipsub 事件处理），所以 spawn 出去跑；结果只记日志 —— gossipsub
    /// 没有应用层 ack，重发由发送方的 sync_id 幂等兜住。
    fn persist_incoming_sync(&self, msg: &SyncMessage) {
        let Some(db) = self.store.clone() else {
            tracing::debug!(
                "[P2P][LWW] no DB handle — skip persist for sync_id={} (table={})",
                msg.sync_id,
                msg.table
            );
            return;
        };
        let record = SyncRecord::from(msg);
        tokio::spawn(async move {
            match handle_incoming_sync(&db, &record).await {
                Ok(outcome) => tracing::debug!(
                    "[P2P][LWW] sync_id={} -> {}",
                    record.sync_id,
                    outcome.reason()
                ),
                // handle_incoming_sync 内部已 error! 过细节，这里只补 peer 上下文
                Err(e) => tracing::error!(
                    "[P2P][LWW] persist failed for sync_id={}: {}",
                    record.sync_id,
                    e
                ),
            }
        });
    }
}

// ─────────────────────────────────────────────
// P2P 服务句柄（供 API handler 和业务层使用）
// ─────────────────────────────────────────────

/// 轻量级句柄，可在多处克隆使用
#[derive(Clone)]
pub struct P2PHandle {
    pub peer_id: String,
    pub sync_tx: mpsc::Sender<SyncMessage>,
    pub cmd_tx: mpsc::Sender<P2PCommand>,
}

impl P2PHandle {
    /// 广播一条同步消息（fire-and-forget）
    pub async fn broadcast(&self, msg: SyncMessage) {
        let _ = self.sync_tx.send(msg).await;
    }

    /// 查询已知 peers
    pub async fn get_peers(&self) -> Vec<PeerInfo> {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(P2PCommand::GetPeers(tx)).await.is_ok() {
            rx.await.unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// 查询本节点状态
    pub async fn get_status(&self) -> Option<NodeStatus> {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(P2PCommand::GetStatus(tx)).await.is_ok() {
            rx.await.ok()
        } else {
            None
        }
    }
}

/// 启动 P2P 节点，返回句柄
///
/// `store`：收到对端同步消息后落库用的连接池（`AppState.db` 的克隆）。传 None
/// 时节点仍能发现设备与广播，但入站同步只记日志不写库。
pub async fn start_p2p_node(data_dir: &str, store: Option<sqlx::PgPool>) -> Result<P2PHandle> {
    let node = P2PNode::new(data_dir, store).await?;
    let peer_id = node.peer_id.to_string();

    let (sync_tx, sync_rx) = mpsc::channel::<SyncMessage>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<P2PCommand>(32);

    tokio::spawn(async move {
        if let Err(e) = node.run(sync_rx, cmd_rx).await {
            tracing::error!("[P2P] Node exited with error: {}", e);
        }
    });

    Ok(P2PHandle {
        peer_id,
        sync_tx,
        cmd_tx,
    })
}

// ─────────────────────────────────────────────
// 单测：LWW 合并 + 幂等去重
//
// 全程内存 mock —— 不起 libp2p，不连 Postgres。
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const PK: &str = "11111111-2222-3333-4444-555555555555";

    /// 内存版 [`SyncStore`]：一张 sync_log + 一张 (table, pk) -> (ts, payload) 行表，
    /// 外加故障注入与写次数计数（用来证明"跳过"真的没写库）。
    #[derive(Default)]
    struct MockStore {
        /// 已应用的 sync_id（即 sync_log 表）
        log: Mutex<Vec<String>>,
        /// (table, primary_key) -> (updated_at_ms, payload)
        rows: Mutex<HashMap<(String, String), (i64, serde_json::Value)>>,
        /// apply_record 实际执行次数
        writes: Mutex<usize>,
        /// 在指定阶段注入 DB 故障
        fail_at: Option<&'static str>,
    }

    impl MockStore {
        fn failing_at(stage: &'static str) -> Self {
            Self {
                fail_at: Some(stage),
                ..Default::default()
            }
        }

        /// 预置一行本地数据（模拟库里已有的版本）
        fn seed(&self, table: &str, pk: &str, ts_ms: i64, payload: serde_json::Value) {
            self.rows
                .lock()
                .unwrap()
                .insert((table.to_string(), pk.to_string()), (ts_ms, payload));
        }

        fn row(&self, table: &str, pk: &str) -> Option<(i64, serde_json::Value)> {
            self.rows
                .lock()
                .unwrap()
                .get(&(table.to_string(), pk.to_string()))
                .cloned()
        }

        fn write_count(&self) -> usize {
            *self.writes.lock().unwrap()
        }

        fn logged(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn guard(&self, stage: &str) -> Result<(), SyncError> {
            if self.fail_at == Some(stage) {
                return Err(SyncError::Store(format!("injected DB failure at {stage}")));
            }
            Ok(())
        }
    }

    impl SyncStore for MockStore {
        async fn is_applied(&self, sync_id: &str) -> Result<bool, SyncError> {
            self.guard("is_applied")?;
            Ok(self.log.lock().unwrap().iter().any(|s| s == sync_id))
        }

        async fn current_ts_ms(
            &self,
            table: &str,
            primary_key: &str,
        ) -> Result<Option<i64>, SyncError> {
            self.guard("current_ts_ms")?;
            Ok(self.row(table, primary_key).map(|(ts, _)| ts))
        }

        async fn apply_record(&self, record: &SyncRecord) -> Result<(), SyncError> {
            self.guard("apply_record")?;
            let key = (record.table.clone(), record.primary_key.clone());
            if record.is_delete() {
                self.rows.lock().unwrap().remove(&key);
            } else {
                self.rows
                    .lock()
                    .unwrap()
                    .insert(key, (record.ts_ms, record.payload.clone()));
            }
            self.log.lock().unwrap().push(record.sync_id.clone());
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn record(sync_id: &str, ts_ms: i64, name: &str) -> SyncRecord {
        SyncRecord {
            sync_id: sync_id.to_string(),
            table: "people".to_string(),
            primary_key: PK.to_string(),
            payload: serde_json::json!({ "id": PK, "name": name }),
            ts_ms,
            operation: "upsert".to_string(),
        }
    }

    fn name_of(payload: &serde_json::Value) -> String {
        payload["name"].as_str().unwrap_or_default().to_string()
    }

    /// 1. 首次 sync 写入成功
    #[tokio::test]
    async fn first_sync_writes_row_and_logs_sync_id() {
        let store = MockStore::default();
        let rec = record("sync-1", 1_000, "阿妈");

        let outcome = handle_incoming_sync(&store, &rec).await.expect("should apply");

        assert_eq!(outcome, SyncOutcome::Applied);
        let (ts, payload) = store.row("people", PK).expect("row must exist");
        assert_eq!(ts, 1_000);
        assert_eq!(name_of(&payload), "阿妈");
        // sync_log 落了台账，后续重发才能命中幂等
        assert_eq!(store.logged(), vec!["sync-1".to_string()]);
        assert_eq!(store.write_count(), 1);
    }

    /// 2. 同 sync_id 重发 → skipped（幂等去重），不产生第二次写
    #[tokio::test]
    async fn duplicate_sync_id_is_skipped_without_second_write() {
        let store = MockStore::default();
        let first = record("sync-dup", 1_000, "阿妈");
        assert_eq!(
            handle_incoming_sync(&store, &first).await.unwrap(),
            SyncOutcome::Applied
        );

        // 同 sync_id 但内容/时间戳都更"新"：仍必须被幂等拦住
        let resend = record("sync-dup", 9_999, "被重放的脏数据");
        let outcome = handle_incoming_sync(&store, &resend).await.unwrap();

        assert_eq!(outcome, SyncOutcome::SkippedDuplicate);
        assert_eq!(store.write_count(), 1, "重发不应二次写库");
        let (ts, payload) = store.row("people", PK).unwrap();
        assert_eq!(ts, 1_000, "时间戳保持首次写入的值");
        assert_eq!(name_of(&payload), "阿妈", "内容不被重放覆盖");
    }

    /// 3. 更早 timestamp 不覆盖（LWW 判负）
    #[tokio::test]
    async fn older_timestamp_does_not_overwrite() {
        let store = MockStore::default();
        store.seed(
            "people",
            PK,
            2_000,
            serde_json::json!({ "id": PK, "name": "本地较新" }),
        );

        let stale = record("sync-old", 1_000, "对端较旧");
        let outcome = handle_incoming_sync(&store, &stale).await.unwrap();

        assert_eq!(outcome, SyncOutcome::SkippedStale);
        assert_eq!(store.write_count(), 0, "旧数据不应写库");
        let (ts, payload) = store.row("people", PK).unwrap();
        assert_eq!(ts, 2_000);
        assert_eq!(name_of(&payload), "本地较新");

        // 时间戳相等同样判负（防同刻互相覆盖的写放大）
        let tie = record("sync-tie", 2_000, "同刻");
        assert_eq!(
            handle_incoming_sync(&store, &tie).await.unwrap(),
            SyncOutcome::SkippedStale
        );
        assert_eq!(store.write_count(), 0);
    }

    /// 4. 更晚 timestamp 覆盖
    #[tokio::test]
    async fn newer_timestamp_overwrites() {
        let store = MockStore::default();
        store.seed(
            "people",
            PK,
            1_000,
            serde_json::json!({ "id": PK, "name": "本地较旧" }),
        );

        let fresh = record("sync-new", 2_000, "对端较新");
        let outcome = handle_incoming_sync(&store, &fresh).await.unwrap();

        assert_eq!(outcome, SyncOutcome::Applied);
        assert_eq!(store.write_count(), 1);
        let (ts, payload) = store.row("people", PK).unwrap();
        assert_eq!(ts, 2_000);
        assert_eq!(name_of(&payload), "对端较新");
        assert_eq!(store.logged(), vec!["sync-new".to_string()]);
    }

    /// 5. DB 错误时返 Err（且不留下半截状态）
    #[tokio::test]
    async fn store_failure_returns_err() {
        // 写阶段失败
        let store = MockStore::failing_at("apply_record");
        let rec = record("sync-boom", 1_000, "写不进去");
        let err = handle_incoming_sync(&store, &rec).await.unwrap_err();
        assert!(
            matches!(err, SyncError::Store(ref m) if m.contains("apply_record")),
            "expected Store error, got: {err:?}"
        );
        assert!(store.row("people", PK).is_none(), "失败不应留下行");
        assert!(store.logged().is_empty(), "失败不应写 sync_log");

        // 幂等查询阶段失败也必须冒泡，不能被当成"没见过 → 直接写"
        let store = MockStore::failing_at("is_applied");
        let err = handle_incoming_sync(&store, &rec).await.unwrap_err();
        assert!(matches!(err, SyncError::Store(_)));
        assert_eq!(store.write_count(), 0);

        // 读当前版本阶段失败同理，不能退化成"库里没有 → 直接覆盖"
        let store = MockStore::failing_at("current_ts_ms");
        let err = handle_incoming_sync(&store, &rec).await.unwrap_err();
        assert!(matches!(err, SyncError::Store(_)));
        assert_eq!(store.write_count(), 0);
    }

    /// 6. 表名白名单：表名会被拼进 SQL，必须挡住注入
    #[tokio::test]
    async fn non_whitelisted_table_is_rejected_before_any_query() {
        let store = MockStore::default();
        let mut rec = record("sync-inject", 1_000, "x");
        rec.table = "people; DROP TABLE people --".to_string();

        let err = handle_incoming_sync(&store, &rec).await.unwrap_err();
        assert!(matches!(err, SyncError::UnsupportedTable(_)), "got {err:?}");
        assert_eq!(store.write_count(), 0);
        assert!(store.logged().is_empty());

        for t in SYNCABLE_TABLES {
            assert!(is_syncable_table(t), "{t} should be syncable");
        }
        assert!(!is_syncable_table("sync_log"), "台账表本身不可被同步写入");
    }

    /// 7. delete 操作删行，并且照样写台账（保持幂等）
    #[tokio::test]
    async fn delete_operation_removes_row_and_stays_idempotent() {
        let store = MockStore::default();
        store.seed(
            "people",
            PK,
            1_000,
            serde_json::json!({ "id": PK, "name": "待删" }),
        );

        let mut del = record("sync-del", 2_000, "");
        del.operation = "delete".to_string();
        del.payload = serde_json::Value::Null;

        assert_eq!(
            handle_incoming_sync(&store, &del).await.unwrap(),
            SyncOutcome::Applied
        );
        assert!(store.row("people", PK).is_none(), "行应被删除");

        // 重放同一条 delete：命中幂等，不再动库
        assert_eq!(
            handle_incoming_sync(&store, &del).await.unwrap(),
            SyncOutcome::SkippedDuplicate
        );
        assert_eq!(store.write_count(), 1);
    }

    /// 8. 线格式互通：API 形状 与 SyncMessage 形状都能反序列化成 SyncRecord
    #[test]
    fn sync_record_accepts_both_api_and_gossipsub_shapes() {
        // HTTP 契约形状
        let api: SyncRecord = serde_json::from_value(serde_json::json!({
            "sync_id": "s1",
            "table": "tasks",
            "primary_key": PK,
            "payload": { "id": PK, "title": "买菜" },
            "ts_ms": 1234
        }))
        .expect("api shape should parse");
        assert_eq!(api.table, "tasks");
        assert_eq!(api.primary_key, PK);
        assert_eq!(api.ts_ms, 1234);
        assert_eq!(api.operation, "upsert", "operation 缺省应为 upsert");

        // gossipsub 线格式（record_id / data / timestamp）
        let msg = SyncMessage::new(
            "peer-1".into(),
            "did:key:zmock".into(),
            "events",
            "update",
            PK,
            serde_json::json!({ "id": PK, "title": "家庭会议" }),
        );
        let wire: SyncRecord =
            serde_json::from_value(serde_json::to_value(&msg).unwrap()).expect("wire shape parses");
        assert_eq!(wire.table, "events");
        assert_eq!(wire.primary_key, PK);
        assert_eq!(wire.ts_ms, msg.timestamp);

        // From<&SyncMessage> 与 serde alias 两条路结果一致
        let converted = SyncRecord::from(&msg);
        assert_eq!(converted.sync_id, wire.sync_id);
        assert_eq!(converted.primary_key, wire.primary_key);
        assert_eq!(converted.ts_ms, wire.ts_ms);
        assert!(!converted.is_delete());
    }

    /// 9. 非法记录：主键为空 / payload 不是 object → InvalidRecord
    #[tokio::test]
    async fn invalid_records_are_rejected() {
        let store = MockStore::default();

        let mut no_pk = record("sync-nopk", 1_000, "x");
        no_pk.primary_key = "   ".to_string();
        assert!(matches!(
            handle_incoming_sync(&store, &no_pk).await.unwrap_err(),
            SyncError::InvalidRecord(_)
        ));

        let mut bad_payload = record("sync-badpayload", 1_000, "x");
        bad_payload.payload = serde_json::json!("just a string");
        assert!(matches!(
            handle_incoming_sync(&store, &bad_payload).await.unwrap_err(),
            SyncError::InvalidRecord(_)
        ));

        assert_eq!(store.write_count(), 0);
    }
}
