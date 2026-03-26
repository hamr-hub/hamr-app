use axum::{middleware, routing::get, Router};

use crate::{
    db::AppState,
    handlers::{dashboard, events, p2p as p2p_handler, people, spaces, tasks, things},
    middleware::auth_middleware,
};

pub fn build_router(state: AppState) -> Router {
    // P2P 状态查询不需要认证（公开元数据）
    let p2p_routes = Router::new()
        .route("/api/v1/p2p/peers", get(p2p_handler::list_peers))
        .route("/api/v1/p2p/status", get(p2p_handler::get_status));

    // 业务数据路由（附带 auth_middleware 注入 Claims）
    let data_routes = Router::new()
        .route("/api/v1/dashboard", get(dashboard::get_stats))
        .route("/api/v1/people", get(people::list).post(people::create))
        .route(
            "/api/v1/people/:id",
            get(people::get).put(people::update).delete(people::delete),
        )
        .route("/api/v1/events", get(events::list).post(events::create))
        .route(
            "/api/v1/events/:id",
            get(events::get).put(events::update).delete(events::delete),
        )
        .route("/api/v1/tasks", get(tasks::list).post(tasks::create))
        .route(
            "/api/v1/tasks/:id",
            get(tasks::get).put(tasks::update).delete(tasks::delete),
        )
        .route("/api/v1/things", get(things::list).post(things::create))
        .route(
            "/api/v1/things/:id",
            get(things::get).put(things::update).delete(things::delete),
        )
        .route("/api/v1/spaces", get(spaces::list).post(spaces::create))
        .route(
            "/api/v1/spaces/:id",
            get(spaces::get).put(spaces::update).delete(spaces::delete),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/api/v1/health", get(health))
        .merge(p2p_routes)
        .merge(data_routes)
        .with_state(state)
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "hamr-app",
        "version": "0.2.0",
        "arch": "p2p-local"
    }))
}
