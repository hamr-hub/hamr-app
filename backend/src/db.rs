use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
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
}
