use sqlx::PgPool;
use crate::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Config,
}

impl AppState {
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        let db = PgPool::connect(database_url).await?;
        let config = Config::from_env()?;
        Ok(Self { db, config })
    }

    pub async fn run_migrations(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.db).await?;
        Ok(())
    }
}
