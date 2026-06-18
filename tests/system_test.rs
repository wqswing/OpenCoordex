use async_trait::async_trait;
use axum::http::StatusCode;
use multi_agent_controller::ReActController;
use multi_agent_core::{ChatMessage, LlmClient, LlmResponse, LlmUsage, ToolRegistry};
use multi_agent_gateway::{DefaultRouter, GatewayConfig, GatewayServer, InMemorySemanticCache};
use multi_agent_governance::AuditStore;
use multi_agent_skills::{CalculatorTool, DefaultToolRegistry, EchoTool};
use multi_agent_store::InMemorySessionStore;
use serde_json::json;
use std::sync::Arc;

// =============================================================================
// Mock LLM Client for System Tests
// =============================================================================

struct ScriptedMockLlm {
    responses: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl ScriptedMockLlm {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(tokio::sync::Mutex::new(responses)),
        }
    }
}

#[async_trait]
impl LlmClient for ScriptedMockLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        let mut resps = self.responses.lock().await;
        let content = if !resps.is_empty() {
            resps.remove(0)
        } else {
            "FINAL ANSWER: Out of mock responses.".to_string()
        };

        Ok(LlmResponse {
            content,
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
            tool_calls: None,
        })
    }

    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        self.complete("").await
    }

    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 1536])
    }
}

// =============================================================================
// Mock Memory Store
// =============================================================================

#[derive(Default)]
struct MockMemoryStore {
    entries: Arc<tokio::sync::RwLock<Vec<multi_agent_core::traits::MemoryEntry>>>,
}

#[async_trait]
impl multi_agent_core::traits::MemoryStore for MockMemoryStore {
    async fn add(
        &self,
        entry: multi_agent_core::traits::MemoryEntry,
    ) -> multi_agent_core::Result<()> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn search(
        &self,
        _embedding: &[f32],
        _limit: usize,
    ) -> multi_agent_core::Result<Vec<multi_agent_core::traits::MemoryEntry>> {
        Ok(self.entries.read().await.clone())
    }

    async fn delete(&self, _id: &str) -> multi_agent_core::Result<()> {
        Ok(())
    }
}

// =============================================================================
// System Tests
// =============================================================================

#[tokio::test]
async fn test_system_e2e_happy_path() -> anyhow::Result<()> {
    let responses = vec![
        "THOUGHT: I need to add 5 and 3.\nACTION: calculator\nARGS: {\"operation\": \"add\", \"a\": 5, \"b\": 3}".to_string(),
        "THOUGHT: I have the result 8. Now I should echo it.\nACTION: echo\nARGS: {\"message\": \"The result is 8\"}".to_string(),
        "FINAL ANSWER: Execution complete. The sum of 5 and 3 is 8.".to_string(),
    ];
    let llm = Arc::new(ScriptedMockLlm::new(responses));
    let tools = Arc::new(DefaultToolRegistry::new());
    tools.register(Box::new(EchoTool)).await?;
    tools.register(Box::new(CalculatorTool)).await?;

    let controller = Arc::new(
        ReActController::builder()
            .with_llm(llm.clone())
            .with_tools(tools)
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    let (addr, _handle) = start_test_server(controller, llm.clone()).await?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat", addr))
        .json(&json!({"message": "Add 5 and 3 and echo it"}))
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data"]["result"]["type"], "Text");
    assert!(body["data"]["result"]["payload"]
        .as_str()
        .unwrap()
        .contains("8"));

    Ok(())
}

#[tokio::test]
async fn test_system_security_block() -> anyhow::Result<()> {
    use multi_agent_governance::{CompositeGuardrail, PiiScanner};

    let llm = Arc::new(ScriptedMockLlm::new(vec![]));
    let guardrail = Arc::new(CompositeGuardrail::new().chain(Box::new(PiiScanner::new())));

    let controller = Arc::new(
        ReActController::builder()
            .with_llm(llm.clone())
            .with_security(guardrail)
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    let (addr, _handle) = start_test_server(controller, llm.clone()).await?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat", addr))
        .json(&json!({"message": "My SSN is 123-45-6789."}))
        .send()
        .await?;

    // Gateway returns OK but payload contains Error
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data"]["result"]["type"], "Error");
    assert!(body["data"]["result"]["payload"]["message"]
        .as_str()
        .unwrap()
        .contains("Security violation"));

    Ok(())
}

#[tokio::test]
async fn test_system_memory_retrieval() -> anyhow::Result<()> {
    use multi_agent_core::traits::MemoryStore;
    let memory_store = Arc::new(MockMemoryStore::default());
    // Seed memory
    memory_store
        .add(multi_agent_core::traits::MemoryEntry {
            id: "1".to_string(),
            content: "Important info: The secret key is GOLDEN-EYE.".to_string(),
            embedding: vec![0.0; 1536],
            metadata: Default::default(),
        })
        .await?;

    let responses =
        vec!["FINAL ANSWER: Based on my memory, the secret key is GOLDEN-EYE.".to_string()];
    let llm = Arc::new(ScriptedMockLlm::new(responses));

    let controller = Arc::new(
        ReActController::builder()
            .with_llm(llm.clone())
            .with_memory(memory_store, llm.clone())
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    let (addr, _handle) = start_test_server(controller, llm.clone()).await?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat", addr))
        .json(&json!({"message": "Help me find the secret key"}))
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["data"]["result"]["type"], "Text");
    assert!(body["data"]["result"]["payload"]
        .as_str()
        .unwrap()
        .contains("GOLDEN-EYE"));

    Ok(())
}

#[tokio::test]
async fn test_system_semantic_cache() -> anyhow::Result<()> {
    let llm = Arc::new(ScriptedMockLlm::new(vec![
        "FINAL ANSWER: This is a new response.".to_string(),
    ]));
    let router = Arc::new(DefaultRouter::new());
    let cache = Arc::new(InMemorySemanticCache::new(llm.clone()));

    let controller = Arc::new(
        ReActController::builder()
            .with_llm(llm.clone())
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    let server = GatewayServer::new(
        GatewayConfig {
            host: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        },
        router,
        cache,
    )
    .with_controller(controller);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(
            listener,
            server
                .build_router()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat", addr);

    // First request
    let resp1 = client
        .post(&url)
        .json(&json!({"message": "Hello"}))
        .send()
        .await?;
    let body1: serde_json::Value = resp1.json().await?;
    assert_eq!(body1["data"]["cached"], false);

    // Second request (same message)
    let resp2 = client
        .post(&url)
        .json(&json!({"message": "Hello"}))
        .send()
        .await?;
    let body2: serde_json::Value = resp2.json().await?;
    assert_eq!(body2["data"]["cached"], true);
    assert_eq!(
        body2["data"]["result"]["payload"],
        body1["data"]["result"]["payload"]
    );

    Ok(())
}

#[tokio::test]
async fn test_system_health_check() -> anyhow::Result<()> {
    let router = Arc::new(DefaultRouter::new());
    let llm = Arc::new(ScriptedMockLlm::new(vec![]));
    let cache = Arc::new(InMemorySemanticCache::new(llm));
    let server = GatewayServer::new(GatewayConfig::default(), router, cache);

    let axum_router = server.build_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        axum::serve(
            listener,
            axum_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = reqwest::Client::new();
    let url = format!("http://{}/health", addr);
    let resp = client.get(&url).send().await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;
    assert_eq!(body["status"], "ok");

    Ok(())
}

// =============================================================================
// Helpers
// =============================================================================

async fn start_test_server(
    controller: Arc<ReActController>,
    llm: Arc<dyn LlmClient>,
) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let router = Arc::new(DefaultRouter::new());
    let cache = Arc::new(InMemorySemanticCache::new(llm));

    let config = GatewayConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        ..Default::default()
    };

    let server = GatewayServer::new(config, router, cache).with_controller(controller);

    let axum_router = server.build_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            axum_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    Ok((addr, handle))
}

#[tokio::test]
async fn test_system_llm_gateway_proxy_endpoint() -> anyhow::Result<()> {
    let responses =
        vec!["FINAL ANSWER: Hello! I am routing this request via the proxy gateway.".to_string()];
    let llm_mock = Arc::new(ScriptedMockLlm::new(responses));

    // Setup tiered routing client
    let model_registry = Arc::new(multi_agent_model_gateway::ProviderRegistry::new());
    model_registry.register("openai", "gpt-4o-mini", llm_mock.clone());
    model_registry.register("openai", "gpt-4o", llm_mock.clone());
    model_registry.register("anthropic", "claude-3-5-sonnet-20241022", llm_mock.clone());

    let model_selector = Arc::new(multi_agent_model_gateway::AdaptiveModelSelector::new(
        model_registry,
    ));
    let pricing_registry = Arc::new(multi_agent_model_gateway::PricingRegistry::with_defaults());
    let cost_tracker = Arc::new(tokio::sync::Mutex::new(
        multi_agent_model_gateway::SessionCostTracker::new(),
    ));

    let tiered_client = Arc::new(multi_agent_model_gateway::TieredRoutingLlmClient::new(
        model_selector,
        pricing_registry,
        cost_tracker,
    ));

    // Setup AdminState and AuditStore
    let audit_store = Arc::new(multi_agent_governance::InMemoryAuditStore::new());
    let rbac = Arc::new(multi_agent_governance::NoOpRbacConnector);
    let secrets_path = std::path::PathBuf::from("test_secrets_proxy.json");
    // Cleanup old file if exists
    let _ = std::fs::remove_file(&secrets_path);
    let secrets = Arc::new(
        multi_agent_governance::secrets::FilePersistentSecretsManager::new(
            secrets_path.clone(),
            None,
        )
        .await?,
    );

    let mut app_config = multi_agent_core::config::AppConfig::default();
    app_config.governance.admin_token = Some(secrecy::Secret::new("admin_secret".to_string()));

    let admin_state = Arc::new(multi_agent_admin::AdminState {
        audit_store: audit_store.clone() as Arc<dyn multi_agent_governance::AuditStore>,
        rbac: rbac as Arc<dyn multi_agent_governance::RbacConnector>,
        metrics: None,
        mcp_registry: Arc::new(multi_agent_skills::McpRegistry::new()),
        providers: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        provider_store: None,
        secrets,
        privacy_controller: None,
        artifact_store: None,
        session_store: None,
        app_config: app_config.clone(),
        network_policy: Arc::new(tokio::sync::RwLock::new(
            multi_agent_governance::network::NetworkPolicy::new(vec![], vec![], vec![]),
        )),
        llm_client: None,
        tool_registry: None,
    });

    let router = Arc::new(DefaultRouter::new());
    let cache = Arc::new(InMemorySemanticCache::new(llm_mock.clone()));

    let config = GatewayConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        ..Default::default()
    };

    let server = GatewayServer::new(config, router, cache)
        .with_admin(admin_state)
        .with_tiered_client(tiered_client);

    let axum_router = server.build_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        axum::serve(
            listener,
            axum_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);

    // Call completions endpoint using the admin token
    let payload = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Heavy reasoning task: FINAL ANSWER: hello"}
        ],
        "temperature": 0.7,
        "stream": false
    });

    let resp = client
        .post(&url)
        .header("x-admin-token", "admin_secret")
        .json(&payload)
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await?;

    // Verify OpenAI-compatible structure
    assert!(body["id"].as_str().unwrap().starts_with("chatcmpl-"));
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "FINAL ANSWER: Hello! I am routing this request via the proxy gateway."
    );
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");

    // Clean up test file
    let _ = std::fs::remove_file(&secrets_path);

    // Verify audit log has the record
    let filter = multi_agent_governance::AuditFilter {
        action: Some("chat_completions".to_string()),
        ..Default::default()
    };
    let entries: Vec<multi_agent_governance::AuditEntry> = audit_store.query(filter).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "chat_completions");
    assert_eq!(entries[0].resource, "anthropic:claude-3-5-sonnet-20241022"); // Routed to Premium since prompt contains heavy reasoning keywords

    Ok(())
}
