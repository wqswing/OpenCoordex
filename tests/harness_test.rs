use multi_agent_core::traits::{ChatMessage, LlmClient};
use multi_agent_core::{LlmResponse, LlmUsage};
use multi_agent_harness::schema::{OutputAssertion, RunStatus, TestCase};
use multi_agent_harness::HarnessRunner;
use multi_agent_skills::DefaultToolRegistry;
use std::collections::HashMap;
use std::sync::Arc;

// Mock LLM implementation for the base HarnessRunner setup (can be bypassed using mock_llm_responses)
struct DummyLlm;

#[async_trait::async_trait]
impl LlmClient for DummyLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "FINAL ANSWER: Dummy completed".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage::default(),
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

#[tokio::test]
async fn test_harness_exact_match_success() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, None);

    let test_case = TestCase {
        id: "tc-1".to_string(),
        name: "Exact Match Test".to_string(),
        description: "Verify exact match assertion works".to_string(),
        prompt: "Say Hello".to_string(),
        expected_output: OutputAssertion::ExactMatch("Hello World".to_string()),
        tags: vec!["test".to_string()],
        mock_llm_responses: Some(vec!["FINAL ANSWER: Hello World".to_string()]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Passed);
    assert_eq!(result.actual_output, "Hello World");
    assert!(result.failure_reason.is_none());

    Ok(())
}

#[tokio::test]
async fn test_harness_contains_failure() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, None);

    let test_case = TestCase {
        id: "tc-2".to_string(),
        name: "Contains Test Fail".to_string(),
        description: "Verify contains assertion fails correctly".to_string(),
        prompt: "Say Hello".to_string(),
        expected_output: OutputAssertion::Contains("FailureTarget".to_string()),
        tags: vec!["test".to_string()],
        mock_llm_responses: Some(vec!["FINAL ANSWER: Hello World".to_string()]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Failed);
    assert!(result
        .failure_reason
        .unwrap()
        .contains("did not contain substring"));

    Ok(())
}

#[tokio::test]
async fn test_harness_regex_match() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, None);

    let test_case = TestCase {
        id: "tc-3".to_string(),
        name: "Regex Match Test".to_string(),
        description: "Verify regex assertion works".to_string(),
        prompt: "Output a valid email".to_string(),
        expected_output: OutputAssertion::Regex(
            r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$".to_string(),
        ),
        tags: vec!["test".to_string()],
        mock_llm_responses: Some(vec!["FINAL ANSWER: test@example.com".to_string()]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Passed);
    assert_eq!(result.actual_output, "test@example.com");

    Ok(())
}

#[tokio::test]
async fn test_harness_json_schema_validation() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, None);

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer", "minimum": 0 }
        },
        "required": ["name", "age"]
    });

    let test_case = TestCase {
        id: "tc-4".to_string(),
        name: "JSON Schema Test".to_string(),
        description: "Verify JSON schema assertion works".to_string(),
        prompt: "Output json".to_string(),
        expected_output: OutputAssertion::JsonSchema(schema),
        tags: vec!["test".to_string()],
        mock_llm_responses: Some(vec![
            "FINAL ANSWER: ```json\n{\n  \"name\": \"Alice\",\n  \"age\": 30\n}\n```".to_string(),
        ]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Passed);

    Ok(())
}

#[tokio::test]
async fn test_harness_mock_tool_outputs() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, None);

    let mut mock_tools = HashMap::new();
    mock_tools.insert("calculator".to_string(), "8".to_string());

    let test_case = TestCase {
        id: "tc-5".to_string(),
        name: "Mock Tool Test".to_string(),
        description: "Verify that mock tool execution works".to_string(),
        prompt: "Add 5 and 3".to_string(),
        expected_output: OutputAssertion::Contains("sum is 8".to_string()),
        tags: vec!["test".to_string()],
        mock_llm_responses: Some(vec![
            "THOUGHT: Let me call calculator tool.\nACTION: calculator\nARGS: {\"a\": 5, \"b\": 3}"
                .to_string(),
            "FINAL ANSWER: The sum is 8".to_string(),
        ]),
        mock_tool_outputs: Some(mock_tools),
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Passed);
    assert_eq!(result.actual_output, "The sum is 8");
    assert_eq!(result.steps, 1);

    Ok(())
}

struct MockJudgeLlm {
    expected_pass: bool,
}

#[async_trait::async_trait]
impl LlmClient for MockJudgeLlm {
    async fn complete(&self, prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        assert!(prompt.contains("REQUIRED CONCEPTS"));
        assert!(prompt.contains("FORBIDDEN CONCEPTS"));
        let content = if self.expected_pass {
            r#"{"passed": true, "reason": "All checks passed."}"#.to_string()
        } else {
            r#"{"passed": false, "reason": "Deception concept detected in history."}"#.to_string()
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
        Ok(vec![0.0; 10])
    }
}

#[tokio::test]
async fn test_harness_cognitive_probe_pass() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());
    let mock_judge = Arc::new(MockJudgeLlm {
        expected_pass: true,
    });

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, Some(mock_judge));

    let test_case = TestCase {
        id: "tc-6".to_string(),
        name: "Cognitive Probe Pass".to_string(),
        description: "Verify cognitive probe passes correctly".to_string(),
        prompt: "Execute mission".to_string(),
        expected_output: OutputAssertion::CognitiveProbe {
            required_concepts: vec!["honesty".to_string()],
            forbidden_concepts: vec!["deception".to_string()],
        },
        tags: vec!["cognitive".to_string()],
        mock_llm_responses: Some(vec!["FINAL ANSWER: Action completed honestly.".to_string()]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Passed);
    assert!(result.failure_reason.is_none());

    Ok(())
}

#[tokio::test]
async fn test_harness_cognitive_probe_fail() -> anyhow::Result<()> {
    let dummy_llm = Arc::new(DummyLlm);
    let tools = Arc::new(DefaultToolRegistry::new());
    let mock_judge = Arc::new(MockJudgeLlm {
        expected_pass: false,
    });

    let runner = HarnessRunner::new(dummy_llm.clone(), tools, Some(mock_judge));

    let test_case = TestCase {
        id: "tc-7".to_string(),
        name: "Cognitive Probe Fail".to_string(),
        description: "Verify cognitive probe fails when forbidden concept is present".to_string(),
        prompt: "Execute mission".to_string(),
        expected_output: OutputAssertion::CognitiveProbe {
            required_concepts: vec!["honesty".to_string()],
            forbidden_concepts: vec!["deception".to_string()],
        },
        tags: vec!["cognitive".to_string()],
        mock_llm_responses: Some(vec!["FINAL ANSWER: Mission accomplished.".to_string()]),
        mock_tool_outputs: None,
        max_iterations: None,
        token_budget: None,
    };

    let result = runner.run_test_case(&test_case).await;
    assert_eq!(result.status, RunStatus::Failed);
    assert!(result
        .failure_reason
        .unwrap()
        .contains("Deception concept detected"));

    Ok(())
}
