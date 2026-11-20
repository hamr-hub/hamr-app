use axum::{
    extract::State,
    http::header::CONTENT_TYPE,
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};

use crate::{
    db::AppState,
    handlers::{
        agent, dashboard, events, p2p as p2p_handler, people, spaces, sync as sync_handler, tasks,
        things,
    },
    middleware::auth_middleware,
};

/// Round-5: Prometheus 抓取端点。
///
/// 公开（不走 auth / 限流）：抓取端通常是内网 Prometheus，家庭 P2P 拓扑里也没有
/// 登录用户；指标只含聚合计数，不含 PII。内容类型必须是 Prometheus text format。
async fn prometheus_metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus(),
    )
}

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

    // Agent 路由先用与业务数据相同的鉴权，再检查运行时是否已初始化。
    // 运行时不可用时整个子路由返回 503，包括用于就绪检查的 /status。
    let agent_routes = Router::new()
        .route("/api/v1/agent/chat", post(agent::chat))
        .route("/api/v1/agent/status", get(agent::status))
        .route("/api/v1/agent/sessions", get(agent::list_sessions))
        .route(
            "/api/v1/agent/sessions/:id/messages",
            get(agent::session_messages),
        )
        .route(
            "/api/v1/agent/sessions/:id",
            axum::routing::delete(agent::delete_session),
        )
        .route("/api/v1/agent/memories", get(agent::list_memories))
        .route("/api/v1/agent/memories", post(agent::add_memory))
        .route("/api/v1/agent/reminders", get(agent::list_reminders))
        .route("/api/v1/agent/proactive/scan", post(agent::proactive_scan))
        .route(
            "/api/v1/agent/proactive/flush",
            post(agent::proactive_flush),
        )
        // 家庭 Wiki：知识层。模型只读，人可读可重建。
        .route("/api/v1/agent/wiki", get(agent::wiki_index))
        .route("/api/v1/agent/wiki/rebuild", post(agent::wiki_rebuild))
        .route("/api/v1/agent/wiki/search", get(agent::wiki_search))
        .route("/api/v1/agent/wiki/page", get(agent::wiki_page))
        .route("/api/v1/agent/wiki/changelog", get(agent::wiki_changelog))
        // 场景包：版本化、不可变、每个家庭单独开关。
        .route("/api/v1/agent/skills", get(agent::list_skills))
        .route("/api/v1/agent/skills/available", get(agent::scan_skills))
        .route("/api/v1/agent/skills/install", post(agent::install_skill))
        .route("/api/v1/agent/skills/enabled", post(agent::toggle_skill))
        // 审批写操作：确认走 HTTP，不给模型 confirm 工具。
        .route("/api/v1/agent/write-ops", get(agent::list_write_ops))
        .route(
            "/api/v1/agent/write-ops/:id/confirm",
            post(agent::confirm_write_op),
        )
        .route(
            "/api/v1/agent/write-ops/:id/reject",
            post(agent::reject_write_op),
        )
        // 模型体检：判断当前模型会不会调工具。
        .route("/api/v1/agent/probe", post(agent::probe))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            agent::require_runtime,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/api/v1/health", get(health))
        // 与 /health 同级公开，路径采用 Prometheus 社区惯例 `/metrics`
        .route("/metrics", get(prometheus_metrics))
        .merge(p2p_routes)
        .merge(data_routes)
        .merge(agent_routes)
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
    use crate::agent::{provider::StubProvider, store::MemStore, AgentRuntime};
    use crate::config::{AuthMode, Config};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    const FAMILY_ID: &str = "00000000-0000-0000-0000-000000000001";
    const OPERATION_ID: &str = "00000000-0000-0000-0000-000000000002";

    fn authenticated_request(method: &str, uri: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", "Bearer s3cret")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    }

    fn start_test_runtime(state: &mut AppState) {
        state.config.agent.enabled = true;
        state.agent = Some(Arc::new(AgentRuntime::new(
            state.config.agent.clone(),
            Arc::new(StubProvider::new(Vec::new())),
            Arc::new(MemStore::new()),
            state.db.clone(),
        )));
    }

    /// 测试用 AppState：shared-secret 模式 + 一个固定 secret
    fn test_state(secret: &str) -> AppState {
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://hamr:hamr@127.0.0.1:1/hamr_app")
            .expect("lazy pool should not fail");
        let config = Config {
            database_url: "postgresql://hamr:hamr@127.0.0.1:1/hamr_app".to_string(),
            port: 3002,
            local_only: false,
            data_dir: "/tmp/hamr-routes-test".to_string(),
            auth_mode: AuthMode::SharedSecret,
            shared_secret: Some(secret.to_string()),
            allowed_origins: vec!["http://localhost:3010".to_string()],
            agent: crate::agent::AgentConfig::disabled(),
        };
        AppState {
            db,
            config,
            p2p_handle: None,
            metrics: std::sync::Arc::new(crate::metrics::AppMetrics::new()),
            agent: None,
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

    /// /metrics 公开可抓、返回 Prometheus text format，且含关键指标族。
    /// 反向钉住 round-5 接线：metrics 必须是不需 Bearer token 的抓取端点。
    #[tokio::test]
    async fn metrics_endpoint_is_public_prometheus_text() {
        let app = build_router(test_state("s3cret"));

        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("text/plain") && ct.contains("version=0.0.4")),
            "content-type must be Prometheus text exposition format, got {:?}",
            resp.headers().get("content-type")
        );

        let body = to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        // HELP/TYPE 头 + 三个核心指标族必须在
        assert!(text.contains("# TYPE hamr_p2p_node_up gauge"));
        assert!(text.contains("hamr_p2p_uptime_seconds "));
        assert!(text.contains("# TYPE hamr_p2p_sync_total counter"));
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

    /// Agent 对话端点必须鉴权 —— 它能读到的家庭数据不比直接调 API 少。
    #[tokio::test]
    async fn agent_chat_requires_auth() {
        let app = build_router(test_state("s3cret"));

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/agent/chat")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"family_id":"00000000-0000-0000-0000-000000000001","message":"hi"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "Agent 端点无 token 必须 401"
        );
    }

    #[tokio::test]
    async fn agent_routes_return_503_when_disabled() {
        let state = test_state("s3cret");
        state.db.close().await;
        let app = build_router(state);
        let family_query = format!("family_id={FAMILY_ID}");
        let family_body = format!(r#"{{"family_id":"{FAMILY_ID}"}}"#);
        let chat_body = format!(r#"{{"family_id":"{FAMILY_ID}","message":"hi"}}"#);
        let skill_body = format!(
            r#"{{"family_id":"{FAMILY_ID}","skill_id":"sample","version":"1.0.0","enabled":true}}"#
        );
        for (method, path, body) in [
            ("GET", "status".into(), ""),
            ("POST", "chat".into(), chat_body.as_str()),
            ("GET", format!("wiki?{family_query}"), ""),
            ("POST", "wiki/rebuild".into(), family_body.as_str()),
            ("GET", format!("skills?{family_query}"), ""),
            ("GET", "skills/available".into(), ""),
            ("POST", "skills/install".into(), skill_body.as_str()),
            ("POST", "skills/enabled".into(), skill_body.as_str()),
            ("GET", format!("write-ops?{family_query}"), ""),
            (
                "POST",
                format!("write-ops/{OPERATION_ID}/confirm"),
                family_body.as_str(),
            ),
            (
                "POST",
                format!("write-ops/{OPERATION_ID}/reject"),
                family_body.as_str(),
            ),
        ] {
            let uri = format!("/api/v1/agent/{path}");
            let req = authenticated_request(method, &uri, Body::from(body.to_owned()));
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn agent_status_reports_readiness_of_initialized_runtime() {
        let mut state = test_state("s3cret");
        state.config.agent.enabled = true;
        let status_request = || authenticated_request("GET", "/api/v1/agent/status", Body::empty());
        let resp = build_router(state.clone())
            .oneshot(status_request())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        start_test_runtime(&mut state);
        let resp = build_router(state).oneshot(status_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["enabled"], true);
        assert_eq!(json["provider"], "local");
        assert_eq!(json["model"], "stub");
    }

    #[tokio::test]
    async fn disabled_agent_leaves_business_routes_available() {
        let app = build_router(test_state("s3cret"));
        // 业务 handler 先校验 family_id，此请求无需连接数据库。
        let req = authenticated_request("GET", "/api/v1/people", Body::empty());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "family_id required");
    }

    #[tokio::test]
    async fn agent_confirmation_checks_write_flag_before_store_access() {
        for (allow_write, action, expected_status) in [
            (false, "confirm", StatusCode::FORBIDDEN),
            (true, "confirm", StatusCode::UNPROCESSABLE_ENTITY),
            (false, "reject", StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            let mut state = test_state("s3cret");
            // 访问审批存储时，关闭的连接池立即报错，无需运行数据库。
            state.db.close().await;
            state.config.agent.allow_write = allow_write;
            start_test_runtime(&mut state);
            let uri = format!("/api/v1/agent/write-ops/{OPERATION_ID}/{action}");
            let body = Body::from(format!(r#"{{"family_id":"{FAMILY_ID}"}}"#));
            let req = authenticated_request("POST", &uri, body);
            let resp = build_router(state).oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                expected_status,
                "{action} with allow_write={allow_write}"
            );
            if expected_status == StatusCode::UNPROCESSABLE_ENTITY {
                let body = to_bytes(resp.into_body(), 4096).await.unwrap();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert!(json["error"].as_str().unwrap().contains("closed pool"));
            }
        }
    }

    /// Agent 的会话 / 记忆端点同样在鉴权之后。
    #[tokio::test]
    async fn agent_sessions_requires_auth() {
        let app = build_router(test_state("s3cret"));

        let req = Request::builder()
            .uri(format!("/api/v1/agent/sessions?family_id={FAMILY_ID}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
