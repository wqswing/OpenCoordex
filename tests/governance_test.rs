use async_trait::async_trait;
use multi_agent_controller::{ReActConfig, ReActController};
use multi_agent_core::{
    traits::{
        ApprovalGate, ChatMessage, Controller, LlmClient, LlmResponse, LlmUsage, ToolRegistry,
    },
    types::{ApprovalRequest, ApprovalResponse, ToolRiskLevel},
    Error,
};
use multi_agent_skills::DefaultToolRegistry;
use multi_agent_store::InMemorySessionStore;
use std::sync::Arc;

// =============================================================================
// Mocks
// =============================================================================

struct MockLlm {
    responses: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockLlm {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(tokio::sync::Mutex::new(responses)),
        }
    }
}

#[async_trait]
impl LlmClient for MockLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        let mut resps = self.responses.lock().await;
        let content = if !resps.is_empty() {
            resps.remove(0)
        } else {
            "FINAL ANSWER: Done".to_string()
        };

        Ok(LlmResponse {
            content,
            finish_reason: "stop".to_string(),
            usage: LlmUsage {
                prompt_tokens: 10,
                completion_tokens: 10,
                total_tokens: 20,
            },
            tool_calls: None,
        })
    }

    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        self.complete("").await
    }

    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 10])
    }
}

struct DenyGate;

#[async_trait]
impl ApprovalGate for DenyGate {
    async fn request_approval(
        &self,
        _req: &ApprovalRequest,
    ) -> multi_agent_core::Result<ApprovalResponse> {
        Ok(ApprovalResponse::Denied {
            reason: "Computer says no".to_string(),
            reason_code: "TEST_DENIED".to_string(),
            approver_id: Some("system".to_string()),
            approver_role: Some("admin".to_string()),
        })
    }

    fn threshold(&self) -> ToolRiskLevel {
        ToolRiskLevel::Medium
    }
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn test_budget_exceeded() -> anyhow::Result<()> {
    // 1. Setup Controller with VERY low budget (30 tokens)
    // Each mock LLM call uses 20 tokens.
    // Call 1: 20 tokens (OK)
    // Call 2: 20 tokens (+ 20 = 40, Exceeded)

    let config = ReActConfig {
        default_budget: 30,
        max_iterations: 5,
        ..Default::default()
    };

    let responses = vec!["THOUGHT: Step 1".to_string(), "THOUGHT: Step 2".to_string()];
    let llm = Arc::new(MockLlm::new(responses));

    let controller = Arc::new(
        ReActController::builder()
            .with_config(config)
            .with_llm(llm)
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    // 2. Execute
    let result = controller
        .execute(
            multi_agent_core::types::UserIntent::ComplexMission {
                goal: "Do work".to_string(),
                context_summary: "".to_string(),
                visual_refs: vec![],
                user_id: None,
            },
            "test-trace".to_string(),
        )
        .await;

    // 3. Verify Error
    match result {
        Err(Error::BudgetExceeded { used, limit }) => {
            assert!(used >= limit, "Used {} should be >= limit {}", used, limit);
        }
        _ => panic!("Expected BudgetExceeded error, got {:?}", result),
    }

    Ok(())
}

struct HighRiskTool;
#[async_trait]
impl multi_agent_core::traits::Tool for HighRiskTool {
    fn name(&self) -> &str {
        "high_risk_tool"
    }
    fn description(&self) -> &str {
        "Dangerous"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
    ) -> multi_agent_core::Result<multi_agent_core::types::ToolOutput> {
        Ok(multi_agent_core::types::ToolOutput::text(
            "Done".to_string(),
        ))
    }
    fn risk_level(&self) -> ToolRiskLevel {
        ToolRiskLevel::High
    }
}

#[tokio::test]
async fn test_deadlock_breaker() -> anyhow::Result<()> {
    // 1. Setup Controller with DenyGate and HighRiskTool
    // The agent will try to call the tool, get denied, observe denial, try again...
    // Should break after 3 attempts.

    let responses = vec![
        "ACTION: high_risk_tool\nARGS: {}".to_string(),
        "ACTION: high_risk_tool\nARGS: {}".to_string(),
        "ACTION: high_risk_tool\nARGS: {}".to_string(),
        "ACTION: high_risk_tool\nARGS: {}".to_string(), // Should not be reached
    ];

    let llm = Arc::new(MockLlm::new(responses));
    let tools = Arc::new(DefaultToolRegistry::new());
    tools.register(Box::new(HighRiskTool)).await?;

    let controller = Arc::new(
        ReActController::builder()
            .with_llm(llm)
            .with_tools(tools)
            .with_approval_gate(Arc::new(DenyGate))
            .with_session_store(Arc::new(InMemorySessionStore::new()))
            .build(),
    );

    // 2. Execute
    let result = controller
        .execute(
            multi_agent_core::types::UserIntent::ComplexMission {
                goal: "Do dangerous work".to_string(),
                context_summary: "".to_string(),
                visual_refs: vec![],
                user_id: None,
            },
            "test-trace".to_string(),
        )
        .await;

    // 3. Verify Error
    match result {
        Err(Error::Controller(msg)) => {
            assert!(
                msg.contains("Deadlock"),
                "Expected Deadlock error, got: {}",
                msg
            );
        }
        _ => panic!("Expected Controller Deadlock error, got {:?}", result),
    }

    Ok(())
}
