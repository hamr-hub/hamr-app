mod config;
mod db;
mod did;
mod errors;
mod handlers;
mod middleware;
mod models;
mod p2p;
mod routes;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub use config::Config;
pub use db::AppState;
pub use did::DeviceIdentity;
pub use p2p::{P2PHandle, SyncMessage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let config = Config::from_env()?;

    // ── 加载/创建设备 DID 身份 ────────────────────────────────
    let identity_path = Path::new(&config.data_dir).join("identity.json");
    let identity = DeviceIdentity::load_or_create(&identity_path).await?;
    tracing::info!("Device DID: {}", identity.did);

    // ── 初始化 SQLite 数据库 ──────────────────────────────────
    let state = AppState::new(&config.database_url).await?;
    state.run_migrations().await?;

    // ── 启动 P2P 节点 ─────────────────────────────────────────
    let state = match p2p::start_p2p_node(&config.data_dir).await {
        Ok(handle) => {
            tracing::info!("P2P node started: peer_id={}", handle.peer_id);
            state.with_p2p(handle)
        }
        Err(e) => {
            tracing::warn!("P2P node failed to start (single-device mode): {}", e);
            state
        }
    };

    let state = Arc::new(state);

    // ── CORS ──────────────────────────────────────────────────
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // ── HTTP 路由 ─────────────────────────────────────────────
    let app = routes::build_router((*state).clone())
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("HamR App Server (P2P Local) listening on {}", addr);
    tracing::info!("API: http://{}/api/v1/health", addr);
    tracing::info!("P2P peers: http://{}/api/v1/p2p/peers", addr);
    tracing::info!("P2P status: http://{}/api/v1/p2p/status", addr);
    tracing::info!("Device DID: {}", identity.did);

    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;

    Ok(())
}
