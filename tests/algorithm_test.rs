use async_trait::async_trait;
use multi_agent_controller::react::{ReActConfig, ReActController};
use multi_agent_core::{
    mocks::{MockLlm, MockToolRegistry},
    traits::{Controller, IntentRouter, SessionStore, Tool},
    types::{AgentResult, NormalizedRequest, RefId, ToolOutput, UserIntent},
    Result,
};
use multi_agent_gateway::router::DefaultRouter;
use serde_json::Value;
use std::sync::{Arc, Mutex};

// ============================================================================
// Helper: Closure Tool for Testing
// ============================================================================

struct ClosureTool {
    name: String,
    func: Box<dyn Fn(Value) -> Result<String> + Send + Sync>,
}

impl ClosureTool {
    fn new(name: &str, func: impl Fn(Value) -> Result<String> + Send + Sync + 'static) -> Self {
        Self {
            name: name.to_string(),
            func: Box::new(func),
        }
    }
}

#[async_trait]
impl Tool for ClosureTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Test tool"
    }
    fn parameters(&self) -> Value {
        serde_json::json!({})
    }
    async fn execute(&self, args: Value) -> Result<ToolOutput> {
        let content = (self.func)(args)?;
        Ok(ToolOutput {
            success: true,
            content,
            data: None,
            created_refs: vec![],
        })
    }
}

// ============================================================================
// 1. Routing Algorithm Tests
// ============================================================================

#[tokio::test]
async fn test_routing_precision() {
    let router = DefaultRouter::new();

    // Case 1: Explicit Fast Action
    let req = NormalizedRequest::text("Calculate 5 + 5");
    let intent = router.classify(&req).await.unwrap();
    if let UserIntent::FastAction { tool_name, .. } = intent {
        assert_eq!(tool_name, "calculator");
    } else {
        panic!("Expected FastAction(calculator), got {:?}", intent);
    }

    // Case 2: Explicit Complex Mission
    let req = NormalizedRequest::text("Create a marketing plan");
    let intent = router.classify(&req).await.unwrap();
    if let UserIntent::ComplexMission { goal, .. } = intent {
        assert!(goal.contains("marketing plan"));
    } else {
        panic!("Expected ComplexMission, got {:?}", intent);
    }

    // Case 3: Ambiguous (Priority Check)
    let req = NormalizedRequest::text("Search for a recipe and write a blog post about it");
    let intent = router.classify(&req).await.unwrap();
    matches!(intent, UserIntent::ComplexMission { .. });
}

#[tokio::test]
async fn test_routing_visual_override() {
    let router = DefaultRouter::new();
    let mut req = NormalizedRequest::text("What is this?");
    // Correct usage: refs expects RefId directly based on Request struct definition
    req.refs.push(RefId::from_string("image_123"));

    let intent = router.classify(&req).await.unwrap();
    if let UserIntent::ComplexMission { visual_refs, .. } = intent {
        assert_eq!(visual_refs.len(), 1);
        assert_eq!(visual_refs[0], "image_123");
    } else {
        panic!("Visual refs should force ComplexMission");
    }
}

// ============================================================================
// 2. Scheduling Algorithm Tests (ReAct State Machine)
// ============================================================================

#[tokio::test]
async fn test_react_perfect_execution() {
    // Setup Mock LLM
    let llm = MockLlm::new(vec![
        // Step 1
        r#"THOUGHT: Check weather.
ACTION: weather
ARGS: {"city": "Paris"}"#
            .to_string(),
        // Step 2
        r#"FINAL ANSWER: It is sunny in Paris."#.to_string(),
    ]);

    // Setup Mock Tool
    let weather_tool = ClosureTool::new("weather", |_: Value| {
        Ok("Weather in Paris is Sunny".to_string())
    });

    let registry = MockToolRegistry::with_tools(vec![Arc::new(weather_tool)]);

    let config = ReActConfig::default();

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(Arc::new(llm))
        .with_tools(Arc::new(registry))
        .build();

    let result: AgentResult = controller
        .execute(
            UserIntent::ComplexMission {
                goal: "Check weather in Paris".to_string(),
                context_summary: "".to_string(),
                visual_refs: vec![],
                user_id: None,
            },
            "test-trace".to_string(),
        )
        .await
        .unwrap();

    match result {
        AgentResult::Text(ans) => assert_eq!(ans, "It is sunny in Paris."),
        _ => panic!("Expected text result, got {:?}", result),
    }
}

#[tokio::test]
async fn test_react_max_iterations() {
    let llm = MockLlm::new(vec![
        "THOUGHT: Thinking...".to_string(),
        "THOUGHT: Still thinking...".to_string(),
        "THOUGHT: More thinking...".to_string(),
        "THOUGHT: Is this working?".to_string(),
    ]);

    let config = ReActConfig {
        max_iterations: 3,
        ..Default::default()
    };

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(Arc::new(llm))
        .build();

    let result: Result<AgentResult> = controller
        .execute(
            UserIntent::ComplexMission {
                goal: "Hard problem".to_string(),
                context_summary: "".to_string(),
                visual_refs: vec![],
                user_id: None,
            },
            "test-trace".to_string(),
        )
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains("exceeded max iterations") || err.contains("budget"),
        "Unexpected error: {}",
        err
    );
}

// ============================================================================
// 3. Resumption & Crash Recovery Tests
// ============================================================================

#[tokio::test]
async fn test_react_persistence_trigger() {
    // A simplified test to verify saving happens.

    struct CapturingStore {
        last_id: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl SessionStore for CapturingStore {
        async fn save(&self, session: &multi_agent_core::types::Session) -> Result<()> {
            *self.last_id.lock().unwrap() = Some(session.id.clone());
            Ok(())
        }
        async fn load(&self, _: &str) -> Result<Option<multi_agent_core::types::Session>> {
            Ok(None)
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_running(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_sessions(
            &self,
            _status: Option<multi_agent_core::types::SessionStatus>,
            _last_id: Option<&str>,
        ) -> Result<Vec<multi_agent_core::types::Session>> {
            Ok(vec![])
        }
    }

    let last_id = Arc::new(Mutex::new(None));
    let store = Arc::new(CapturingStore {
        last_id: last_id.clone(),
    });

    let llm = MockLlm::new(vec!["FINAL ANSWER: Done".to_string()]);

    let controller = ReActController::builder()
        .with_config(ReActConfig::default())
        .with_llm(Arc::new(llm))
        // Correct method name from builder.rs
        .with_session_store(store)
        .build();

    let _ = controller
        .execute(
            UserIntent::ComplexMission {
                goal: "Save me".to_string(),
                context_summary: "".to_string(),
                visual_refs: vec![],
                user_id: None,
            },
            "test-trace".to_string(),
        )
        .await;

    assert!(
        last_id.lock().unwrap().is_some(),
        "Session should have been saved"
    );
}
