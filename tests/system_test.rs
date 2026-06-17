use async_trait::async_trait;
use axum::http::StatusCode;
use multi_agent_controller::ReActController;
use multi_agent_core::{ChatMessage, LlmClient, LlmResponse, LlmUsage, ToolRegistry};
use multi_agent_gateway::{DefaultRouter, GatewayConfig, GatewayServer, InMemorySemanticCache};
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
