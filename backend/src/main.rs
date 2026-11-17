mod agent;
mod config;
mod db;
mod did;
mod errors;
mod handlers;
mod metrics;
mod middleware;
mod models;
mod p2p;
mod routes;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
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

    // ── 初始化 PostgreSQL 数据库 ───────────────────────────────
    let state = AppState::new(&config.database_url).await?;
    state.run_migrations().await?;

    // ── 进程级指标集合（P2P 事件循环写、/metrics handler 读，同一 Arc） ──
    let metrics = Arc::new(metrics::AppMetrics::new());

    // ── Agent 运行时（管家的大脑层）───────────────────────────
    // 默认不启用（HAMR_AGENT_ENABLED）。启用但初始化失败时降级为「不可用」，
    // 而不是让整个服务起不来 —— 其余五维数据功能不该被 Agent 拖垮。
    let agent = build_agent(&config, &state.db).await;
    let state = state.with_agent(agent);

    // ── 启动 P2P 节点 ─────────────────────────────────────────
    // 把 DB 池交给节点：入站 SyncMessage 走 handle_incoming_sync 真实落库
    // （sync_log 幂等去重 + last-write-wins 合并）
    let state = match p2p::start_p2p_node(
        &config.data_dir,
        Some(state.db.clone()),
        metrics.clone(),
    )
    .await
    {
        Ok(handle) => {
            tracing::info!("P2P node started: peer_id={}", handle.peer_id);
            // 节点成功启动：node_up=1。单设备模式保持 0。
            metrics.set_node_up(true);
            state.with_p2p(handle).with_metrics(metrics)
        }
        Err(e) => {
            tracing::warn!("P2P node failed to start (single-device mode): {}", e);
            state.with_metrics(metrics)
        }
    };

    let state = Arc::new(state);

    // ── CORS（按 env allowlist 收紧，避免 Any 暴露） ──────────
    // 仅允许 HAMR_APP_ALLOWED_ORIGINS 列出的 Origin；支持标准 GET/POST/PUT/DELETE/PATCH
    // 与 Content-Type/Authorization/X-DID-* 自定义头。
    let allowed_origins: Vec<http::HeaderValue> = state
        .config
        .allowed_origins
        .iter()
        .filter_map(|o| http::HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([
            http::Method::GET,
            http::Method::POST,
            http::Method::PUT,
            http::Method::DELETE,
            http::Method::PATCH,
            http::Method::OPTIONS,
        ])
        .allow_headers([
            http::header::CONTENT_TYPE,
            http::header::AUTHORIZATION,
            http::HeaderName::from_static("x-did-public-key"),
            http::HeaderName::from_static("x-did-signature"),
            http::HeaderName::from_static("x-did-timestamp"),
        ])
        .max_age(std::time::Duration::from_secs(600));

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

/// 构建 Agent 运行时。
///
/// 返回 None 表示 Agent 不可用（未启用 / 配置非法 / provider 构造失败）。
/// 三种情况都只影响 Agent 相关端点（返回 503），不影响其余功能。
async fn build_agent(config: &Config, db: &sqlx::PgPool) -> Option<Arc<agent::AgentRuntime>> {
    if !config.agent.enabled {
        tracing::info!("Agent runtime disabled (set HAMR_AGENT_ENABLED=true to enable)");
        return None;
    }

    let provider = match agent::provider::OpenAiCompatProvider::new(&config.agent) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::warn!("Agent provider init failed, agent disabled: {}", e);
            return None;
        }
    };

    let store = Arc::new(agent::PgStore::new(db.clone()));
    let runtime = agent::AgentRuntime::new(config.agent.clone(), provider, store, db.clone());
    tracing::info!(
        "Agent runtime ready: provider={} model={}",
        config.agent.provider.as_str(),
        config.agent.model
    );
    Some(Arc::new(runtime))
}
