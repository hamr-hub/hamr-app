/// p2p.rs — HamR P2P 同步模块
///
/// 实现三层功能：
///   P1: mDNS 设备发现（局域网自动发现）
///   P2: 节点管理（Ed25519 身份持久化、设备列表、/p2p/peers /p2p/status API 数据）
///   P3: 数据同步框架（SyncMessage 广播，last-write-wins）
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use libp2p::{
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
}

impl P2PNode {
    /// 创建 P2P 节点
    ///
    /// `data_dir`：身份文件存储目录（如 `~/.hamr`）
    pub async fn new(data_dir: &str) -> Result<Self> {
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
                                    // TODO: 调用数据库应用 LWW 合并逻辑（Phase2）
                                    // 目前仅记录日志，留钩子
                                    Self::handle_incoming_sync(sync_msg);
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

    /// P3: 处理收到的同步消息（LWW 合并钩子）
    fn handle_incoming_sync(msg: SyncMessage) {
        // TODO: 接入 AppState.db，执行 LWW 合并写入
        // 当前仅打印，供后续扩展
        tracing::debug!(
            "[P2P][LWW] Incoming sync: table={} op={} record={} ts={}",
            msg.table,
            msg.operation,
            msg.record_id,
            msg.timestamp
        );
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
pub async fn start_p2p_node(data_dir: &str) -> Result<P2PHandle> {
    let node = P2PNode::new(data_dir).await?;
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
