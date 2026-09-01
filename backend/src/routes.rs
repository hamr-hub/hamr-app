use axum::{
    middleware,
    routing::{get, post},
    Router,
};

use crate::{
    db::AppState,
    handlers::{
        dashboard, events, p2p as p2p_handler, people, spaces, sync as sync_handler, tasks, things,
    },
    middleware::auth_middleware,
};

pub fn build_router(state: AppState) -> Router {
    // P2P 状态查询：peer_id / 监听地址 / mDNS 发现列表都是网络元数据，
    // 在 `shared-secret` / `did` 模式下要过 `auth_middleware`；
    // `open` 模式下中间件只会塞默认 Claims，相当于放行（round-4 收紧）。
    let p2p_routes = Router::new()
        .route("/api/v1/p2p/peers", get(p2p_handler::list_peers))
        .route("/api/v1/p2p/status", get(p2p_handler::get_status))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

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
        // P2P 入站同步：远程写入原语，必须在 auth_middleware 之后
        .route("/api/v1/sync/incoming", post(sync_handler::incoming))
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

#[cfg(test)]
mod tests {
    //! Round-4: 验证 `/api/v1/p2p/peers` 和 `/api/v1/p2p/status`
    //! 在 `shared-secret` 模式下走 `auth_middleware`。
    //!
    //! 构造 AppState 用 `PgPoolOptions::connect_lazy` —— 拿一个永远不会
    //! 真去连的 pool，p2p peers/status handler 本身只用 `p2p_handle`
    //! （这里塞 None → "P2P node not running"），全程不打 DB。

    use super::*;
    use crate::config::{AuthMode, Config};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use sqlx::postgres::PgPoolOptions;
    use tower::util::ServiceExt;

    /// 测试用 AppState：shared-secret 模式 + 一个固定 secret
    fn test_state(secret: &str) -> AppState {
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://hamr:hamr@127.0.0.1:1/hamr_app")
            .expect("lazy pool should not fail");
        let config = Config {
            database_url: "postgresql://hamr:hamr@127.0.0.1:1/hamr_app".to_string(),
            port: 3002,
            data_dir: "/tmp/hamr-routes-test".to_string(),
            auth_mode: AuthMode::SharedSecret,
            shared_secret: Some(secret.to_string()),
            allowed_origins: vec!["http://localhost:3010".to_string()],
        };
        AppState {
            db,
            config,
            p2p_handle: None,
        }
    }

    /// 关键 round-4 测试：p2p 路由**现在**要求 Bearer token。
    /// 没带 → 401；带正确 secret → 200 + "P2P node not running" 占位 JSON。
    #[tokio::test]
    async fn p2p_peers_requires_auth_in_shared_secret_mode() {
        let app = build_router(test_state("s3cret"));

        // 1) 不带 token —— 应被 auth_middleware 拦住
        let req = Request::builder()
            .uri("/api/v1/p2p/peers")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "无 token 必须 401，而不是放行"
        );

        // 2) 带正确 token —— 应被放行（p2p_handle=None 返回占位 JSON）
        let req = Request::builder()
            .uri("/api/v1/p2p/peers")
            .header("authorization", "Bearer s3cret")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["count"], 0);
        assert_eq!(json["peers"], serde_json::json!([]));
    }

    /// 同样的 auth 也覆盖 /api/v1/p2p/status（防止只给 peers 套、漏掉 status）
    #[tokio::test]
    async fn p2p_status_requires_auth_in_shared_secret_mode() {
        let app = build_router(test_state("s3cret"));

        let req = Request::builder()
            .uri("/api/v1/p2p/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// /api/v1/health 仍然公开 —— 反向证明只有 p2p/业务路由套了 auth。
    #[tokio::test]
    async fn health_remains_public() {
        let app = build_router(test_state("s3cret"));

        let req = Request::builder()
            .uri("/api/v1/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
