use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use crate::metrics::AppMetrics;
use crate::{p2p::P2PHandle, Config};

#[derive(Clone)]
pub struct AppState {
    /// PostgreSQL 连接池
    ///
    /// 方言必须与 `migrations/*.sql`、`docker-compose.yml` 以及 handler 里的
    /// `$1` 占位符 / `RETURNING *` / `TEXT[]` 保持一致（round-1 已把 sqlx
    /// feature 切到 postgres，此处同步收口，否则 crate 无法编译）。
    pub db: PgPool,
    pub config: Config,
    /// P2P 节点句柄（可选，节点未启动时为 None）
    pub p2p_handle: Option<P2PHandle>,
    /// Round-5：进程级 Prometheus 指标集合。P2P 事件循环单向写，`/metrics`
    /// handler 只读原子量、不走 channel，节点卡死时抓取端仍能出数。
    pub metrics: Arc<AppMetrics>,
}

impl AppState {
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        let db = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        let config = Config::from_env()?;
        Ok(Self {
            db,
            config,
            p2p_handle: None,
            metrics: Arc::new(AppMetrics::new()),
        })
    }

    pub async fn run_migrations(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.db).await?;
        Ok(())
    }

    /// 注入 P2P 节点句柄
    pub fn with_p2p(mut self, handle: P2PHandle) -> Self {
        self.p2p_handle = Some(handle);
        self
    }

    /// 注入与 P2P 节点共享的指标句柄（main 里先建 Arc 再分别交给节点和 state）
    pub fn with_metrics(mut self, metrics: Arc<AppMetrics>) -> Self {
        self.metrics = metrics;
        self
    }
}
