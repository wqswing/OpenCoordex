use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use multi_agent_admin::AdminState;
use multi_agent_gateway::{DefaultRouter, GatewayConfig, GatewayServer, InMemorySemanticCache};
use multi_agent_governance::{setup_metrics_recorder, NoOpRbacConnector, SqliteAuditStore};
use std::sync::Arc;
use tower::ServiceExt; // for oneshot

#[tokio::test]
async fn test_v0_8_features_integration() {
    // 1. Setup Environment
    // Use a unique file for audit logic to avoid collision
    let audit_file = format!("test_audit_v0_8_{}.log", uuid::Uuid::new_v4());
    let _ = std::fs::remove_file(&audit_file); // Clean up previous run

    // Initialize Governance
    let audit_store = Arc::new(SqliteAuditStore::new(&audit_file).unwrap());
    let rbac = Arc::new(NoOpRbacConnector);
    // Metrics setup might fail if already initialized in another test, so handle error gracefully or assume it works once globally
    let metrics_handle = setup_metrics_recorder().ok();

    let admin_state = Arc::new(AdminState {
        audit_store: audit_store.clone(),
        rbac: rbac.clone(),
        metrics: metrics_handle.clone(),
        mcp_registry: Arc::new(multi_agent_skills::McpRegistry::new()),
        providers: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        provider_store: None,
        secrets: Arc::new(multi_agent_governance::AesGcmSecretsManager::new(None)),
        privacy_controller: None,
        artifact_store: None,
        session_store: None,
        app_config: multi_agent_core::config::AppConfig::default(),
        network_policy: Arc::new(tokio::sync::RwLock::new(
            multi_agent_governance::network::NetworkPolicy::default(),
        )),
        llm_client: None,
        tool_registry: None,
    });

    // Initialize Gateway
    let config = GatewayConfig {
        host: "127.0.0.1".to_string(),
        port: 0, // Random port
        enable_cors: false,
        enable_tracing: false,
        allowed_origins: vec![],
        tls: Default::default(),
    };

    // Mocks for Gateway deps
    let router = Arc::new(DefaultRouter::new());
    let llm_client = Arc::new(multi_agent_model_gateway::MockLlmClient::new("dummy"));
    let cache = Arc::new(InMemorySemanticCache::new(llm_client));
    let routing_policy_store =
        Arc::new(multi_agent_gateway::routing_policy::RoutingPolicyStore::new());

    let server = GatewayServer::new(config, router, cache)
        .with_admin(admin_state)
        .with_metrics(metrics_handle.expect("Metrics handler must be available for this test"))
        .with_routing_policy_store(routing_policy_store);

    let app = server.build_router();

    // 2. Test Cases

    // Case A: Public Health Check
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Case B: Admin Config (Unauthorized - No Token)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/admin/config")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Case C: Admin Config (Forbidden - User Token)
    // NoOpRbacConnector returns "user" role for default token
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/admin/config")
                .header("Authorization", "Bearer somerandomtoken")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Case D: Admin Config (Authorized - Admin Token)
    // NoOpRbacConnector returns "admin" role for "admin" token
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/admin/config")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Case E: Metrics Endpoint (Authorized)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/admin/metrics")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // We can't easily check the body string in oneshot without reading the stream,
    // but OK status implies authentication worked.

    // Case F: Verify Audit Log persistence
    // We need to trigger an audit event first.
    // Currently Admin API reads logs, but maybe modifying config writes logs?
    // Actually, the current Admin API implementation is read-only for now,
    // but let's verify we can Write to the audit store manually and Read it back via API.

    use multi_agent_governance::{AuditEntry, AuditStore};
    let entry = AuditEntry {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        user_id: "test_admin".to_string(),
        action: "TEST_ACTION".to_string(),
        resource: "test_resource".to_string(),
        outcome: multi_agent_governance::AuditOutcome::Success,
        metadata: Some(serde_json::json!({"foo": "bar"})),
        previous_hash: None,
        hash: None,
    };
    audit_store.log(entry).await.unwrap();

    // Now query via API
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/admin/audit?action=TEST_ACTION")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Case G: Publish versioned routing policy
    let publish_payload = serde_json::json!({
        "version": "1.0.0",
        "name": "default-routing",
        "channel": "canary",
        "rules": [
            {
                "id": "channel-support",
                "scope": "channel",
                "scope_value": "support",
                "target": {
                    "type": "fast_action",
                    "payload": { "tool_name": "search" }
                },
                "priority": 1
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/routing/publish")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::from(publish_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Case H: Simulate routing policy
    let simulate_payload = serde_json::json!({
        "scenarios": [
            {
                "channel": "support",
                "account": "a1",
                "peer": "u1"
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/routing/simulate")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::from(simulate_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"][0]["matched_rule_id"], "channel-support");

    // Case I: Promote canary policy to stable
    let promote_payload = serde_json::json!({
        "version": "1.0.0"
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/routing/promote")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::from(promote_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["promoted"], true);
    assert_eq!(json["active_stable"]["version"], "1.0.0");

    // Case J: Publish a newer canary and then rollback canary to previous version
    let publish_payload_2 = serde_json::json!({
        "version": "1.1.0",
        "name": "default-routing-next",
        "channel": "canary",
        "rules": [
            {
                "id": "channel-support-v2",
                "scope": "channel",
                "scope_value": "support",
                "target": {
                    "type": "fast_action",
                    "payload": { "tool_name": "calculator" }
                },
                "priority": 2
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/routing/publish")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::from(publish_payload_2.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let rollback_payload = serde_json::json!({
        "version": "1.0.0"
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/routing/rollback")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::from(rollback_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Case K: Query policy state for stable/canary channels
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/routing/policies")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["active_stable"]["version"], "1.0.0");
    assert_eq!(json["active_canary"]["version"], "1.0.0");
    assert!(json["history"].as_array().unwrap().len() >= 2);

    // Case L: Query routing audit view by action
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/routing/audits?action=ROUTING_POLICY_PUBLISH")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["count"].as_u64().unwrap() >= 2);

    // Case M: Query routing audit view by version and channel (publish metadata)
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/routing/audits?action=ROUTING_POLICY_PUBLISH&version=1.0.0&channel=canary")
                .header("Authorization", "Bearer admin")
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["count"].as_u64().unwrap() >= 1);

    // 3. Cleanup
    let _ = std::fs::remove_file(audit_file);
}
