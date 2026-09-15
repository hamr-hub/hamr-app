//! metrics.rs — Round-5：Prometheus `/metrics` 暴露 + P2P 健康告警
//!
//! ## 为什么不引入 prometheus / metrics-exporter crate
//!
//! 本轮只需要「若干计数器 + 若干 gauge → text exposition 格式」。
//! Prometheus 的 text format 是稳定的行协议（`0.0.4`），手写渲染 ~40 行，
//! 换来的是：零新依赖、零 Cargo.lock churn、离线可编译、无 registry 全局
//! 单例（`prometheus` crate 的默认 registry 是进程级 static，单测并发注册
//! 同名 collector 会 panic —— 本项目 33 个单测全并发跑，这是真实雷）。
//!
//! ## 数据流：为什么用 Arc<AppMetrics> 而不是走 P2PCommand
//!
//! R4 的 `/p2p/status` 是 `oneshot` 穿过 swarm 事件循环取快照。这个模式
//! **不能**用在 `/metrics` 上：swarm 循环一旦被慢 I/O 卡住（或 panic 掉），
//! 抓取端会挂在 `rx.await` 上直到超时 —— 监控系统恰恰要在这种时候能出数。
//!
//! 所以指标存在一份 `Arc<AppMetrics>` 原子集合里：P2P 节点单向写，HTTP
//! handler 只读原子量、**不做任何 channel I/O**。副作用是 `/metrics` 在
//! 节点卡死时仍能返回（`hamr_p2p_node_up` 会停在 1 但 gauge 不再更新，
//! 配合 `hamr_p2p_uptime_seconds` 仍然自增即可在告警侧识别）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::p2p::{NodeStatus, SyncError, SyncOutcome};

// ─────────────────────────────────────────────
// 指标集合
// ─────────────────────────────────────────────

/// P2P 相关的计数器与 gauge。
///
/// 全部 `AtomicU64` + `Ordering::Relaxed`：指标是统计量，不参与任何
/// 正确性判定，不需要跨线程 happens-before，Relaxed 足够且最省。
#[derive(Debug, Default)]
pub struct P2PCounters {
    // ── counters（只增） ──────────────────────────────────────
    /// 入站 gossipsub 消息总数（含之后被限流/解码失败的）
    pub messages_received_total: AtomicU64,
    /// 入站消息 JSON 解码失败数
    pub decode_errors_total: AtomicU64,
    /// 被 rate limiter 主动丢弃的入站 sync 数
    pub rate_limited_drops_total: AtomicU64,
    /// LWW 落库成功
    pub sync_applied_total: AtomicU64,
    /// LWW 判负跳过（重复 sync_id 或 stale timestamp）
    pub sync_skipped_total: AtomicU64,
    /// 落库失败（表名非法 / 记录不合法 / DB 故障）
    pub sync_error_total: AtomicU64,
    /// 本地向外广播成功数
    pub publish_total: AtomicU64,
    /// 本地向外广播失败数（不含 InsufficientPeers —— 单设备模式属正常）
    pub publish_errors_total: AtomicU64,
    /// 触发过的健康告警次数
    pub health_alerts_total: AtomicU64,

    // ── gauges（可增可减） ────────────────────────────────────
    /// 当前已连接 peer 数
    pub connected_peers: AtomicU64,
    /// 当前已知（含离线）peer 数
    pub known_peers: AtomicU64,
    /// rate limiter 当前持有的桶数
    pub rate_limiter_buckets: AtomicU64,
    /// P2P 节点是否在跑（1/0）
    pub node_up: AtomicU64,
}

/// 进程级指标句柄，挂在 `AppState` 上，全程 `Arc` 共享。
#[derive(Debug)]
pub struct AppMetrics {
    pub p2p: P2PCounters,
    /// 进程启动时刻。uptime 在 render 时现算，**不依赖 swarm 循环打点** ——
    /// 否则空闲局域网里（mDNS query_interval=20s）uptime 会跳着走。
    started_at: Instant,
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self {
            p2p: P2PCounters::default(),
            started_at: Instant::now(),
        }
    }
}

impl AppMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// 供测试构造「已经跑了一段时间」的实例
    #[cfg(test)]
    pub fn started_ago(d: Duration) -> Self {
        Self {
            p2p: P2PCounters::default(),
            started_at: Instant::now() - d,
        }
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    // ── 写入口：语义化方法，避免调用方到处写字段名 ────────────

    pub fn inc_messages_received(&self) {
        self.p2p.messages_received_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_decode_errors(&self) {
        self.p2p.decode_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 返回累计丢弃数（含本次），方便调用方直接打进日志
    pub fn inc_rate_limited_drops(&self) -> u64 {
        self.p2p.rate_limited_drops_total.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn rate_limited_drops(&self) -> u64 {
        self.p2p.rate_limited_drops_total.load(Ordering::Relaxed)
    }

    pub fn inc_publish(&self, ok: bool) {
        if ok {
            self.p2p.publish_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.p2p.publish_errors_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 把一条 sync 的处理结果折进 `sync_total{result=...}` 三个桶。
    ///
    /// `SkippedDuplicate` 和 `SkippedStale` 合并成 `skipped`：两者都是
    /// 「按预期没写库」，运维视角同一类；要区分请看 debug 日志。
    pub fn record_sync_outcome(&self, outcome: &Result<SyncOutcome, SyncError>) {
        match outcome {
            Ok(SyncOutcome::Applied) => {
                self.p2p.sync_applied_total.fetch_add(1, Ordering::Relaxed);
            }
            Ok(SyncOutcome::SkippedDuplicate) | Ok(SyncOutcome::SkippedStale) => {
                self.p2p.sync_skipped_total.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.p2p.sync_error_total.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn set_node_up(&self, up: bool) {
        self.p2p.node_up.store(u64::from(up), Ordering::Relaxed);
    }

    pub fn set_peer_gauges(&self, connected: usize, known: usize) {
        self.p2p.connected_peers.store(connected as u64, Ordering::Relaxed);
        self.p2p.known_peers.store(known as u64, Ordering::Relaxed);
    }

    pub fn set_rate_limiter_buckets(&self, buckets: usize) {
        self.p2p
            .rate_limiter_buckets
            .store(buckets as u64, Ordering::Relaxed);
    }

    pub fn inc_health_alerts(&self, n: u64) {
        self.p2p.health_alerts_total.fetch_add(n, Ordering::Relaxed);
    }

    /// 用 `/p2p/status` 的快照回填 gauge（给非 P2P 路径的调用方兜底）
    pub fn sync_from_status(&self, s: &NodeStatus) {
        self.set_peer_gauges(s.connected_peers, s.known_peers);
        self.set_rate_limiter_buckets(s.active_rate_limited_peers);
    }

    // ── 读出口 ────────────────────────────────────────────────

    fn get(&self, f: &AtomicU64) -> u64 {
        f.load(Ordering::Relaxed)
    }

    /// 渲染 Prometheus text exposition format（version=0.0.4）。
    ///
    /// 契约（被单测钉住）：
    /// - 每个指标前有 `# HELP` 和 `# TYPE` 各一行；
    /// - 样本行形如 `name value` 或 `name{label="v"} value`；
    /// - 整个文档以 `\n` 结尾（缺尾换行会被部分抓取端判为截断）。
    pub fn render_prometheus(&self) -> String {
        let p = &self.p2p;
        let mut out = String::with_capacity(2048);

        // 构建信息：version 用 label 带出来，值恒为 1（Prometheus 惯例）
        metric(
            &mut out,
            "hamr_build_info",
            "gauge",
            "Build info of the running hamr-app-server; value is always 1.",
            &[(
                format!("version=\"{}\"", env!("CARGO_PKG_VERSION")),
                1,
            )],
        );

        metric(
            &mut out,
            "hamr_p2p_node_up",
            "gauge",
            "Whether the P2P node event loop was started (1) or the server runs single-device (0).",
            &[(String::new(), self.get(&p.node_up))],
        );

        metric(
            &mut out,
            "hamr_p2p_uptime_seconds",
            "gauge",
            "Seconds since the process started.",
            &[(String::new(), self.uptime_seconds())],
        );

        metric(
            &mut out,
            "hamr_p2p_connected_peers",
            "gauge",
            "Number of currently connected P2P peers.",
            &[(String::new(), self.get(&p.connected_peers))],
        );

        metric(
            &mut out,
            "hamr_p2p_known_peers",
            "gauge",
            "Number of peers discovered via mDNS, including offline ones.",
            &[(String::new(), self.get(&p.known_peers))],
        );

        metric(
            &mut out,
            "hamr_p2p_rate_limiter_buckets",
            "gauge",
            "Number of per-peer token buckets currently held by the inbound rate limiter.",
            &[(String::new(), self.get(&p.rate_limiter_buckets))],
        );

        metric(
            &mut out,
            "hamr_p2p_messages_received_total",
            "counter",
            "Total inbound gossipsub messages received, before rate limiting and decoding.",
            &[(String::new(), self.get(&p.messages_received_total))],
        );

        metric(
            &mut out,
            "hamr_p2p_decode_errors_total",
            "counter",
            "Total inbound messages that failed SyncMessage JSON decoding.",
            &[(String::new(), self.get(&p.decode_errors_total))],
        );

        metric(
            &mut out,
            "hamr_p2p_rate_limited_drops_total",
            "counter",
            "Total inbound sync messages dropped by the per-peer token bucket.",
            &[(String::new(), self.get(&p.rate_limited_drops_total))],
        );

        // 带 label 的 counter：一个 HELP/TYPE 头 + 三个样本行
        metric(
            &mut out,
            "hamr_p2p_sync_total",
            "counter",
            "Inbound sync records by last-write-wins persistence outcome.",
            &[
                (
                    "result=\"applied\"".to_string(),
                    self.get(&p.sync_applied_total),
                ),
                (
                    "result=\"skipped\"".to_string(),
                    self.get(&p.sync_skipped_total),
                ),
                (
                    "result=\"error\"".to_string(),
                    self.get(&p.sync_error_total),
                ),
            ],
        );

        metric(
            &mut out,
            "hamr_p2p_publish_total",
            "counter",
            "Outbound sync broadcasts by result.",
            &[
                ("result=\"ok\"".to_string(), self.get(&p.publish_total)),
                (
                    "result=\"error\"".to_string(),
                    self.get(&p.publish_errors_total),
                ),
            ],
        );

        metric(
            &mut out,
            "hamr_p2p_health_alerts_total",
            "counter",
            "Total P2P health alerts raised (see ERROR logs for details).",
            &[(String::new(), self.get(&p.health_alerts_total))],
        );

        out
    }
}

/// 渲染一个指标族：`# HELP` + `# TYPE` + N 个样本行。
///
/// `samples` 里的 label 串是**已拼好**的 `k="v"` 片段（空串 = 无 label）。
fn metric(out: &mut String, name: &str, kind: &str, help: &str, samples: &[(String, u64)]) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    for (labels, value) in samples {
        out.push_str(name);
        if !labels.is_empty() {
            out.push('{');
            out.push_str(labels);
            out.push('}');
        }
        out.push(' ');
        out.push_str(&value.to_string());
        out.push('\n');
    }
}

// ─────────────────────────────────────────────
// P2P 健康告警
// ─────────────────────────────────────────────

/// 同时被限流的 peer 数超过此值 → 告警。
///
/// 家庭场景正常 peer 数是个位数；50 个 peer 同时在挨限流，只可能是
/// (a) 有人在灌 gossip，或 (b) 我们的 bucket 参数配错了。两种都要人看。
pub const ALERT_MAX_RATE_LIMITED_PEERS: u64 = 50;

/// 丢弃速率超过此值（条/秒）→ 告警。
///
/// bucket 配置是 refill=2/s per peer，稳态下**合法**流量不该产生持续丢弃；
/// 5/s 给了 2~3 个 peer 短时抖动的余量。
pub const ALERT_MAX_DROPS_PER_SEC: f64 = 5.0;

/// 速率统计窗口。太短会被单个 burst 打出假阳性，太长会迟报。
pub const DROP_RATE_WINDOW: Duration = Duration::from_secs(5);

/// 同一类告警的最小间隔 —— 防止 gossip 风暴时把 error 日志刷爆
/// （每条入站消息都会走一次 evaluate）。
pub const ALERT_COOLDOWN: Duration = Duration::from_secs(30);

/// 一条健康告警。结构化而非直接 `error!`，是为了让阈值逻辑可单测 ——
/// 判定与日志副作用分离。
#[derive(Debug, Clone, PartialEq)]
pub enum HealthAlert {
    /// 同时被限流的 peer 过多
    TooManyRateLimitedPeers { peers: u64, threshold: u64 },
    /// 丢弃速率过高
    DropRateTooHigh {
        drops_in_window: u64,
        per_sec: f64,
        threshold: f64,
    },
}

impl HealthAlert {
    /// 供 `tracing::error!` 使用的人读描述
    pub fn message(&self) -> String {
        match self {
            HealthAlert::TooManyRateLimitedPeers { peers, threshold } => format!(
                "{} peers are being rate limited simultaneously (threshold {}) — \
                 possible gossip flood or misconfigured token bucket",
                peers, threshold
            ),
            HealthAlert::DropRateTooHigh {
                drops_in_window,
                per_sec,
                threshold,
            } => format!(
                "inbound sync drop rate {:.2}/s exceeds {:.2}/s ({} drops in the last {}s window) — \
                 a peer is flooding or bucket refill is too low",
                per_sec,
                threshold,
                drops_in_window,
                DROP_RATE_WINDOW.as_secs()
            ),
        }
    }
}

/// 滑窗丢弃速率 + 阈值判定 + 告警节流。
///
/// **不自己读时钟**：`now` 由调用方传入，所以单测可以完全确定性地推进时间，
/// 不需要 `sleep`（R4 的 TokenBucket 测试靠 sleep，慢且偶发 flaky）。
pub struct P2PHealthMonitor {
    window: Duration,
    cooldown: Duration,
    max_rate_limited_peers: u64,
    max_drops_per_sec: f64,
    /// 上个窗口结束时的累计丢弃数
    drops_at_window_start: u64,
    window_started_at: Option<Instant>,
    last_alert_at: Option<Instant>,
}

impl Default for P2PHealthMonitor {
    fn default() -> Self {
        Self {
            window: DROP_RATE_WINDOW,
            cooldown: ALERT_COOLDOWN,
            max_rate_limited_peers: ALERT_MAX_RATE_LIMITED_PEERS,
            max_drops_per_sec: ALERT_MAX_DROPS_PER_SEC,
            drops_at_window_start: 0,
            window_started_at: None,
            last_alert_at: None,
        }
    }
}

impl P2PHealthMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 测试用：自定义窗口与冷却
    #[cfg(test)]
    pub fn with_window_and_cooldown(window: Duration, cooldown: Duration) -> Self {
        Self {
            window,
            cooldown,
            ..Default::default()
        }
    }

    /// 评估当前健康状况。
    ///
    /// - `total_drops`：**累计**丢弃数（单调递增），内部自己差分成速率；
    /// - `rate_limited_peers`：当前 bucket 数（瞬时值）。
    ///
    /// 返回需要告警的项；处于冷却期内返回空 vec（判定仍然照做，只是不报）。
    pub fn evaluate(
        &mut self,
        now: Instant,
        total_drops: u64,
        rate_limited_peers: u64,
    ) -> Vec<HealthAlert> {
        let mut alerts = Vec::new();

        // 瞬时阈值：桶数
        if rate_limited_peers > self.max_rate_limited_peers {
            alerts.push(HealthAlert::TooManyRateLimitedPeers {
                peers: rate_limited_peers,
                threshold: self.max_rate_limited_peers,
            });
        }

        // 滑窗阈值：丢弃速率。窗口没满就先攒着，不做判定。
        let started = *self.window_started_at.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= self.window {
            let delta = total_drops.saturating_sub(self.drops_at_window_start);
            let secs = elapsed.as_secs_f64();
            // secs 恒 >= window > 0，除零不可达；仍然守一下防未来把 window 改成 0
            let per_sec = if secs > 0.0 { delta as f64 / secs } else { 0.0 };
            if per_sec > self.max_drops_per_sec {
                alerts.push(HealthAlert::DropRateTooHigh {
                    drops_in_window: delta,
                    per_sec,
                    threshold: self.max_drops_per_sec,
                });
            }
            // 无论是否告警都翻窗，否则窗口越滚越长、速率被摊薄成 0
            self.window_started_at = Some(now);
            self.drops_at_window_start = total_drops;
        }

        if alerts.is_empty() {
            return alerts;
        }

        // 节流：冷却期内静默（判定已完成，窗口状态也已推进）
        if let Some(last) = self.last_alert_at {
            if now.saturating_duration_since(last) < self.cooldown {
                return Vec::new();
            }
        }
        self.last_alert_at = Some(now);
        alerts
    }
}

// ─────────────────────────────────────────────
// 单测
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Prometheus 渲染 ───────────────────────────────────────

    /// 每个指标族都必须带 HELP + TYPE 头，且样本行数量对得上。
    /// 这是抓取端能不能识别的硬契约。
    #[test]
    fn render_emits_help_and_type_for_every_metric() {
        let m = AppMetrics::new();
        let out = m.render_prometheus();

        let names = [
            "hamr_build_info",
            "hamr_p2p_node_up",
            "hamr_p2p_uptime_seconds",
            "hamr_p2p_connected_peers",
            "hamr_p2p_known_peers",
            "hamr_p2p_rate_limiter_buckets",
            "hamr_p2p_messages_received_total",
            "hamr_p2p_decode_errors_total",
            "hamr_p2p_rate_limited_drops_total",
            "hamr_p2p_sync_total",
            "hamr_p2p_publish_total",
            "hamr_p2p_health_alerts_total",
        ];
        for n in names {
            assert!(
                out.contains(&format!("# HELP {} ", n)),
                "missing HELP for {n}:\n{out}"
            );
            assert!(
                out.contains(&format!("# TYPE {} ", n)),
                "missing TYPE for {n}:\n{out}"
            );
        }

        // 尾换行：缺了会被部分抓取端判成截断响应
        assert!(out.ends_with('\n'), "exposition must end with a newline");
        // 不允许空行（Prometheus 解析器对空行宽容，但我们自己保持干净）
        assert!(
            !out.contains("\n\n"),
            "exposition must not contain blank lines:\n{out}"
        );
    }

    /// 计数器自增要真的反映到输出里（防止渲染读错字段）。
    #[test]
    fn counters_are_reflected_in_exposition() {
        let m = AppMetrics::new();
        m.inc_messages_received();
        m.inc_messages_received();
        m.inc_messages_received();
        m.inc_decode_errors();
        assert_eq!(m.inc_rate_limited_drops(), 1);
        assert_eq!(m.inc_rate_limited_drops(), 2);
        m.inc_publish(true);
        m.inc_publish(false);
        m.inc_health_alerts(2);

        let out = m.render_prometheus();
        assert!(out.contains("\nhamr_p2p_messages_received_total 3\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_decode_errors_total 1\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_rate_limited_drops_total 2\n"), "{out}");
        assert!(out.contains("hamr_p2p_publish_total{result=\"ok\"} 1\n"), "{out}");
        assert!(out.contains("hamr_p2p_publish_total{result=\"error\"} 1\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_health_alerts_total 2\n"), "{out}");
    }

    /// R4 的四个 NodeStatus 量 —— 必须能作为 gauge 出数。
    #[test]
    fn gauges_are_reflected_in_exposition() {
        let m = AppMetrics::new();
        m.set_node_up(true);
        m.set_peer_gauges(2, 5);
        m.set_rate_limiter_buckets(7);

        let out = m.render_prometheus();
        assert!(out.contains("\nhamr_p2p_node_up 1\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_connected_peers 2\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_known_peers 5\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_rate_limiter_buckets 7\n"), "{out}");

        // gauge 要能降回去（不是 counter）
        m.set_peer_gauges(0, 5);
        m.set_node_up(false);
        let out = m.render_prometheus();
        assert!(out.contains("\nhamr_p2p_connected_peers 0\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_node_up 0\n"), "{out}");
    }

    /// `sync_total{result=...}` 三个桶的映射：applied / skipped(x2) / error。
    #[test]
    fn sync_outcomes_map_to_three_result_buckets() {
        let m = AppMetrics::new();
        m.record_sync_outcome(&Ok(SyncOutcome::Applied));
        m.record_sync_outcome(&Ok(SyncOutcome::SkippedDuplicate));
        m.record_sync_outcome(&Ok(SyncOutcome::SkippedStale));
        m.record_sync_outcome(&Err(SyncError::UnsupportedTable("evil".into())));
        m.record_sync_outcome(&Err(SyncError::Store("boom".into())));

        let out = m.render_prometheus();
        assert!(out.contains("hamr_p2p_sync_total{result=\"applied\"} 1\n"), "{out}");
        // duplicate + stale 都折进 skipped
        assert!(out.contains("hamr_p2p_sync_total{result=\"skipped\"} 2\n"), "{out}");
        assert!(out.contains("hamr_p2p_sync_total{result=\"error\"} 2\n"), "{out}");
    }

    /// uptime 现算而不是靠事件循环打点。
    #[test]
    fn uptime_is_computed_from_process_start() {
        let m = AppMetrics::started_ago(Duration::from_secs(42));
        assert!(m.uptime_seconds() >= 42);
        let out = m.render_prometheus();
        assert!(
            out.lines()
                .any(|l| l.starts_with("hamr_p2p_uptime_seconds ")
                    && l.rsplit(' ').next().unwrap().parse::<u64>().unwrap() >= 42),
            "{out}"
        );
    }

    /// `NodeStatus` 快照能回填 gauge（给不经 P2P 循环的调用方兜底）。
    #[test]
    fn sync_from_status_backfills_gauges() {
        let m = AppMetrics::new();
        let status = NodeStatus {
            peer_id: "12D3KooWtest".to_string(),
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/1".to_string()],
            connected_peers: 3,
            known_peers: 9,
            gossipsub_topic: "hamr-family-sync".to_string(),
            uptime_seconds: 100,
            rate_limited_drops: 4,
            active_rate_limited_peers: 6,
        };
        m.sync_from_status(&status);

        let out = m.render_prometheus();
        assert!(out.contains("\nhamr_p2p_connected_peers 3\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_known_peers 9\n"), "{out}");
        assert!(out.contains("\nhamr_p2p_rate_limiter_buckets 6\n"), "{out}");
    }

    // ── 健康告警 ──────────────────────────────────────────────

    /// 阈值以下不告警（防止正常家庭流量天天报错）。
    #[test]
    fn healthy_node_raises_no_alert() {
        let mut mon = P2PHealthMonitor::new();
        let t0 = Instant::now();
        assert!(mon.evaluate(t0, 0, 3).is_empty());
        // 窗口满了，但 5s 内只丢了 2 条（0.4/s）→ 仍然健康
        assert!(mon.evaluate(t0 + Duration::from_secs(6), 2, 3).is_empty());
    }

    /// `active_rate_limited_peers > 50` → 告警。边界值 50 本身不报。
    #[test]
    fn too_many_rate_limited_peers_alerts_above_threshold() {
        let mut mon = P2PHealthMonitor::new();
        let t0 = Instant::now();

        // 边界：等于阈值不报（">" 而不是 ">=")
        assert!(
            mon.evaluate(t0, 0, ALERT_MAX_RATE_LIMITED_PEERS).is_empty(),
            "50 个桶不该告警 —— 阈值是严格大于"
        );

        let alerts = mon.evaluate(t0, 0, ALERT_MAX_RATE_LIMITED_PEERS + 1);
        assert_eq!(
            alerts,
            vec![HealthAlert::TooManyRateLimitedPeers {
                peers: 51,
                threshold: 50
            }]
        );
        assert!(alerts[0].message().contains("rate limited"));
    }

    /// 丢弃速率 > 5/s → 告警；速率按滑窗差分算。
    #[test]
    fn drop_rate_alerts_when_above_five_per_second() {
        let mut mon = P2PHealthMonitor::new();
        let t0 = Instant::now();

        // 开窗
        assert!(mon.evaluate(t0, 0, 1).is_empty());

        // 5s 窗口里丢了 100 条 → 20/s，远超 5/s
        let alerts = mon.evaluate(t0 + Duration::from_secs(5), 100, 1);
        assert_eq!(alerts.len(), 1, "应恰好一条速率告警: {alerts:?}");
        match &alerts[0] {
            HealthAlert::DropRateTooHigh {
                drops_in_window,
                per_sec,
                threshold,
            } => {
                assert_eq!(*drops_in_window, 100);
                assert!((*per_sec - 20.0).abs() < 0.01, "per_sec = {per_sec}");
                assert_eq!(*threshold, ALERT_MAX_DROPS_PER_SEC);
            }
            other => panic!("wrong alert variant: {other:?}"),
        }
    }

    /// 窗口没满不做速率判定 —— 否则一个 burst 会立刻打出假阳性。
    #[test]
    fn drop_rate_not_evaluated_before_window_closes() {
        let mut mon = P2PHealthMonitor::new();
        let t0 = Instant::now();
        assert!(mon.evaluate(t0, 0, 1).is_empty());
        // 才过 1s（窗口 5s），即使已经丢了 1000 条也先不报
        assert!(
            mon.evaluate(t0 + Duration::from_secs(1), 1000, 1).is_empty(),
            "窗口未闭合就报警会被单个 burst 打出假阳性"
        );
    }

    /// 冷却期内不重复刷 error 日志（每条入站消息都会调 evaluate）。
    #[test]
    fn alerts_are_throttled_by_cooldown() {
        let mut mon = P2PHealthMonitor::with_window_and_cooldown(
            Duration::from_secs(1),
            Duration::from_secs(30),
        );
        let t0 = Instant::now();

        // 首次触发
        let first = mon.evaluate(t0, 0, 99);
        assert_eq!(first.len(), 1, "第一次应该报: {first:?}");

        // 冷却期内（+10s < 30s）静默
        assert!(
            mon.evaluate(t0 + Duration::from_secs(10), 0, 99).is_empty(),
            "冷却期内必须静默，否则 gossip 风暴会刷爆日志"
        );

        // 冷却期过后重新放行
        assert_eq!(
            mon.evaluate(t0 + Duration::from_secs(31), 0, 99).len(),
            1,
            "冷却期结束后必须能再报"
        );
    }

    /// 两个阈值同时越线 → 两条告警一起出。
    #[test]
    fn both_thresholds_can_fire_together() {
        let mut mon = P2PHealthMonitor::new();
        let t0 = Instant::now();
        assert!(mon.evaluate(t0, 0, 1).is_empty());

        let alerts = mon.evaluate(t0 + Duration::from_secs(5), 200, 80);
        assert_eq!(alerts.len(), 2, "桶数 + 速率都越线，应各报一条: {alerts:?}");
    }

    /// 窗口翻页后旧的 drop 不再被重复计入（防止告警粘住不灭）。
    #[test]
    fn window_resets_so_old_drops_do_not_re_alert() {
        let mut mon = P2PHealthMonitor::with_window_and_cooldown(
            Duration::from_secs(1),
            // 冷却设为 0，隔离出「窗口翻页」这一个变量
            Duration::from_secs(0),
        );
        let t0 = Instant::now();
        assert!(mon.evaluate(t0, 0, 1).is_empty());

        // 窗口 1：丢了 50 条 → 50/s，告警
        assert_eq!(mon.evaluate(t0 + Duration::from_secs(1), 50, 1).len(), 1);

        // 窗口 2：累计仍是 50（这一窗没有新增）→ 0/s，不该再报
        assert!(
            mon.evaluate(t0 + Duration::from_secs(2), 50, 1).is_empty(),
            "累计值没变说明本窗无新丢弃，告警必须自动消除"
        );
    }
}
