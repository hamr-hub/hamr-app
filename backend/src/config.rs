use anyhow::Context;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub data_dir: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        // data_dir 默认 ~/.hamr
        let data_dir = std::env::var("HAMR_DATA_DIR").unwrap_or_else(|_| {
            dirs_next::home_dir()
                .map(|h| h.join(".hamr").to_string_lossy().to_string())
                .unwrap_or_else(|| "/tmp/hamr".to_string())
        });

        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| format!("sqlite://{}/hamr.db", data_dir)),
            port: std::env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3002),
            data_dir,
        })
    }
}
