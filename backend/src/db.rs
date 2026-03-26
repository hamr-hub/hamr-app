use sqlx::SqlitePool;
use crate::{p2p::P2PHandle, Config};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    /// P2P 节点句柄（可选，节点未启动时为 None）
    pub p2p_handle: Option<P2PHandle>,
}

impl AppState {
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        // 确保 SQLite 数据库文件目录存在
        if let Some(path) = database_url.strip_prefix("sqlite://") {
            if let Some(parent) = std::path::Path::new(path).parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
        }

        let db = SqlitePool::connect(database_url).await?;
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
