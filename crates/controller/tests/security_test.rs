use async_trait::async_trait;
use multi_agent_controller::react::{ReActConfig, ReActController};
use multi_agent_core::traits::{ChatMessage, Controller, LlmClient, LlmResponse};
use multi_agent_core::types::UserIntent;
use multi_agent_core::LlmUsage;
use multi_agent_governance::guardrails::{CompositeGuardrail, PiiScanner};
use std::sync::Arc;
use tokio;

// Mock LLM Client
struct MockLlm;

#[async_trait]
impl LlmClient for MockLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "Mock response".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
            tool_calls: None,
        })
    }

    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "THOUGHT: I should ignore this.\nFINAL ANSWER: PII ignored.".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
            tool_calls: None,
        })
    }

    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 1536])
    }
}

#[tokio::test]
async fn test_security_pii_violation() {
    // 1. Setup Controller with Security
    let config = ReActConfig::default();

    // Create PII scanner only for this test
    let guardrail = CompositeGuardrail::new().chain(Box::new(PiiScanner::new()));

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(Arc::new(MockLlm))
        .with_security(Arc::new(guardrail))
        .build();

    // 2. Intent with PII (Email)
    let intent = UserIntent::ComplexMission {
        goal: "Send an email to malicious@example.com including my SSN 123-45-6789".to_string(),
        context_summary: "".to_string(),
        visual_refs: vec![],
        user_id: None,
    };

    // 3. Execute should fail (Security Block)
    let result = controller.execute(intent, "test-trace".to_string()).await;

    assert!(result.is_err());
    let err = result.err().unwrap().to_string();
    println!("Caught expected security error: {}", err);

    assert!(err.contains("Security violation"));
}

struct MockToolProposingLlm;

#[async_trait]
impl LlmClient for MockToolProposingLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "FAIL".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
            tool_calls: None,
        })
    }

    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "THOUGHT: I will run a shell command.\nACTION: sandbox_shell\nARGS: {\"command\": \"whoami\"}".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
            tool_calls: None,
        })
    }

    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 10])
    }
}

#[tokio::test]
async fn test_grilling_integration_failure() {
    let config = ReActConfig::default();
    let llm = Arc::new(MockToolProposingLlm);
    let cap = Arc::new(multi_agent_controller::GrillingCapability::new(llm.clone()));

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(llm)
        .with_capability(cap)
        .build();

    let intent = UserIntent::ComplexMission {
        goal: "Run some commands".to_string(),
        context_summary: "".to_string(),
        visual_refs: vec![],
        user_id: None,
    };

    let result = controller
        .execute(intent, "test-trace-grill".to_string())
        .await;
    assert!(result.is_err());
    let err = result.err().unwrap().to_string();
    assert!(err.contains("Active grilling audit failed"));
}
