use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use multi_agent_admin::AdminState;
use multi_agent_governance::{
    network::NetworkPolicy, AesGcmSecretsManager, InMemoryAuditStore, NoOpRbacConnector,
    SecretsManager,
};
use multi_agent_skills::McpRegistry;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt;

#[tokio::test]
async fn test_admin_provider_crud_with_encryption() {
    let audit_store = Arc::new(InMemoryAuditStore::new());
    let rbac = Arc::new(NoOpRbacConnector);
    let mcp_registry = Arc::new(McpRegistry::new());
    let secrets = Arc::new(AesGcmSecretsManager::new(None));
    let providers = Arc::new(RwLock::new(Vec::new()));

    let state = Arc::new(AdminState {
        audit_store,
        rbac,
        metrics: None,
        mcp_registry,
        providers,
        provider_store: None,
        secrets: secrets.clone(),
        privacy_controller: None,
        artifact_store: None,
        session_store: None,
        app_config: multi_agent_core::config::AppConfig::default(),
        network_policy: Arc::new(RwLock::new(NetworkPolicy::default())),
        llm_client: None,
        tool_registry: None,
    });

    let app = multi_agent_admin::admin_router(state);

    // 1. Add a provider
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers")
                .header("Content-Type", "application/json")
                .header("Authorization", "Bearer admin")
                .body(Body::from(
                    json!({
                        "vendor": "openai",
                        "model_id": "gpt-4",
                        "base_url": "https://api.openai.com/v1",
                        "api_key": "sk-test-key",
                        "capabilities": ["text"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let provider_id = json["id"]
        .as_str()
        .expect("Provider ID not found")
        .to_string();
    let api_key_id = format!("api_key:{}", provider_id);

    // api_key should NOT be in the response
    assert!(json["api_key"].is_null());

    // 2. Verify key is encrypted in secrets manager
    let retrieved_key = secrets.retrieve(&api_key_id).await.unwrap();
    assert_eq!(retrieved_key, Some("sk-test-key".to_string()));

    // 3. List providers
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/providers")
                .header("Authorization", "Bearer admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert!(list.as_array().unwrap().len() > 0);

    // 4. Delete provider
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&format!("/api/providers/{}", provider_id))
                .header("Authorization", "Bearer admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // 5. Verify secret is deleted
    let retrieved_key_after_delete = secrets.retrieve(&api_key_id).await.unwrap();
    assert!(retrieved_key_after_delete.is_none());
}

struct TestLlm;
#[async_trait::async_trait]
impl multi_agent_core::traits::LlmClient for TestLlm {
    async fn complete(
        &self,
        _prompt: &str,
    ) -> multi_agent_core::Result<multi_agent_core::LlmResponse> {
        Ok(multi_agent_core::LlmResponse {
            content: "FINAL ANSWER: Done".to_string(),
            finish_reason: "stop".to_string(),
            usage: multi_agent_core::LlmUsage::default(),
            tool_calls: None,
        })
    }
    async fn chat(
        &self,
        _messages: &[multi_agent_core::traits::ChatMessage],
    ) -> multi_agent_core::Result<multi_agent_core::LlmResponse> {
        self.complete("").await
    }
    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 10])
    }
}

#[tokio::test]
async fn test_admin_harness_endpoints() {
    let audit_store = Arc::new(InMemoryAuditStore::new());
    let rbac = Arc::new(NoOpRbacConnector);
    let mcp_registry = Arc::new(McpRegistry::new());
    let secrets = Arc::new(AesGcmSecretsManager::new(None));
    let providers = Arc::new(RwLock::new(Vec::new()));

    let state = Arc::new(AdminState {
        audit_store,
        rbac,
        metrics: None,
        mcp_registry,
        providers,
        provider_store: None,
        secrets,
        privacy_controller: None,
        artifact_store: None,
        session_store: None,
        app_config: multi_agent_core::config::AppConfig::default(),
        network_policy: Arc::new(RwLock::new(NetworkPolicy::default())),
        llm_client: Some(Arc::new(TestLlm)),
        tool_registry: Some(Arc::new(multi_agent_skills::DefaultToolRegistry::new())),
    });

    let app = multi_agent_admin::admin_router(state);

    // 1. Get suites
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/harness/suites")
                .header("Authorization", "Bearer admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let suites: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(suites.as_array().unwrap()[0]["id"], "diagnostic-suite");

    // 2. Run suite
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/harness/run")
                .header("Authorization", "Bearer admin")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({ "suite_id": "diagnostic-suite" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["suite_id"], "diagnostic-suite");
    assert_eq!(result["total_cases"], 3);
}
