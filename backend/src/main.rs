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
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub use config::Config;
pub use db::AppState;
pub use did::DeviceIdentity;
pub use p2p::{P2PNode, SyncMessage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let config = Config::from_env()?;
    
    // 加载或创建设备身份（DID）
    let identity_path = Path::new(&config.data_dir).join("identity.json");
    let identity = DeviceIdentity::load_or_create(&identity_path).await?;
    tracing::info!("Device DID: {}", identity.did);
    
    // 初始化数据库
    let state = AppState::new(&config.database_url).await?;
    state.run_migrations().await?;
    
    // 将设备身份添加到应用状态
    let state = Arc::new(state);
    let state_clone = state.clone();
    
    // 创建P2P同步通道
    let (tx, rx) = mpsc::channel::<SyncMessage>(100);
    
    // 在后台启动P2P网络
    let p2p_handle = tokio::spawn(async move {
        match P2PNode::new().await {
            Ok(mut node) => {
                tracing::info!("P2P node started with peer_id: {}", node.peer_id);
                if let Err(e) = node.start(rx).await {
                    tracing::error!("P2P node error: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("Failed to create P2P node: {}", e);
            }
        }
    });
    
    // 将P2P发送通道添加到应用状态
    // TODO: 需要修改AppState以包含p2p_sender
    
    // CORS配置
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // 构建HTTP API路由
    let app = routes::build_router(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("🚀 HamR App Server (P2P Local) listening on {}", addr);
    tracing::info!("📚 API docs: http://{}/api", addr);
    tracing::info!("🔒 Device ID: {}", identity.did);
    
    // 启动HTTP服务器
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    
    // 等待P2P节点结束
    p2p_handle.await?;
    
    Ok(())
}
