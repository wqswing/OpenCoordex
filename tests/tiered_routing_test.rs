use multi_agent_core::{
    traits::{ChatMessage, LlmClient, LlmResponse},
    types::ModelTier,
    LlmUsage,
};
use multi_agent_model_gateway::{
    AdaptiveModelSelector, PricingRegistry, ProviderRegistry, SessionCostTracker,
    TieredRoutingLlmClient,
};
use std::sync::Arc;
use tokio::sync::Mutex;

// Mock LLM client that returns a specific model name/id in its response
struct MockModelClient {
    model_name: String,
}

#[async_trait::async_trait]
impl LlmClient for MockModelClient {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: format!("Response from {}", self.model_name),
            finish_reason: "stop".to_string(),
            usage: LlmUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
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

#[tokio::test]
async fn test_tiered_routing_classification() {
    let registry = Arc::new(ProviderRegistry::new());

    let fast_client = Arc::new(MockModelClient {
        model_name: "openai:gpt-4o-mini".to_string(),
    });
    let std_client = Arc::new(MockModelClient {
        model_name: "openai:gpt-4o".to_string(),
    });
    let premium_client = Arc::new(MockModelClient {
        model_name: "anthropic:claude-3-5-sonnet-20241022".to_string(),
    });

    registry.register("openai", "gpt-4o-mini", fast_client);
    registry.register("openai", "gpt-4o", std_client);
    registry.register("anthropic", "claude-3-5-sonnet-20241022", premium_client);

    let selector = Arc::new(AdaptiveModelSelector::new(registry));
    let pricing = Arc::new(PricingRegistry::with_defaults());
    let tracker = Arc::new(Mutex::new(SessionCostTracker::new()));

    let tiered_client = TieredRoutingLlmClient::new(selector, pricing, tracker.clone());

    // Test prompt classification: Fast
    assert_eq!(
        tiered_client.classify_prompt("THOUGHT: Let's do arithmetic."),
        ModelTier::Fast
    );
    assert_eq!(
        tiered_client.classify_prompt("ACTION: calculator"),
        ModelTier::Fast
    );

    // Test prompt classification: Premium
    assert_eq!(
        tiered_client.classify_prompt("FINAL ANSWER: The result is 10."),
        ModelTier::Premium
    );
    assert_eq!(
        tiered_client.classify_prompt("EVALUATION CRITERIA: ExactMatch"),
        ModelTier::Premium
    );

    // Test prompt classification: Standard (default, length > 100)
    let std_prompt = "Please describe the solar system in detail, including all the major planets, their relative sizes, and their distance from the sun, so we can write a report for school.";
    assert_eq!(
        tiered_client.classify_prompt(std_prompt),
        ModelTier::Standard
    );
}

#[tokio::test]
async fn test_tiered_routing_execution_and_pricing() -> anyhow::Result<()> {
    let registry = Arc::new(ProviderRegistry::new());

    let fast_client = Arc::new(MockModelClient {
        model_name: "openai:gpt-4o-mini".to_string(),
    });
    let std_client = Arc::new(MockModelClient {
        model_name: "openai:gpt-4o".to_string(),
    });
    let premium_client = Arc::new(MockModelClient {
        model_name: "anthropic:claude-3-5-sonnet-20241022".to_string(),
    });

    registry.register("openai", "gpt-4o-mini", fast_client);
    registry.register("openai", "gpt-4o", std_client);
    registry.register("anthropic", "claude-3-5-sonnet-20241022", premium_client);

    let selector = Arc::new(AdaptiveModelSelector::new(registry));
    let pricing = Arc::new(PricingRegistry::with_defaults());
    let tracker = Arc::new(Mutex::new(SessionCostTracker::new()));

    let tiered_client = TieredRoutingLlmClient::new(selector, pricing, tracker.clone());

    // 1. Fast tier call
    let res = tiered_client
        .complete("THOUGHT: Call weather tool.")
        .await?;
    assert_eq!(res.content, "Response from openai:gpt-4o-mini");

    // 2. Standard tier call (length > 100)
    let std_prompt = "Please write a short essay explaining how a refrigerator works, focusing on the compression cycle, the expansion valve, and the heat exchange process.";
    let res = tiered_client.complete(std_prompt).await?;
    assert_eq!(res.content, "Response from openai:gpt-4o");

    // 3. Premium tier call
    let res = tiered_client
        .complete("FINAL ANSWER: Here is the final consolidated report.")
        .await?;
    assert_eq!(
        res.content,
        "Response from anthropic:claude-3-5-sonnet-20241022"
    );

    // 4. Verify Cost Accumulation
    // Pricing Registry defaults (per 1K):
    // - mini: input=0.15, output=0.60
    // - standard (gpt-4o): input=5.00, output=15.00
    // - premium (claude-3-5-sonnet): input=3.00, output=15.00
    // Each mock call uses 100 input and 50 output tokens.
    // mini cost = (100 / 1000) * 0.15 + (50 / 1000) * 0.60 = 0.015 + 0.030 = 0.045
    // std cost  = (100 / 1000) * 5.00 + (50 / 1000) * 15.00 = 0.500 + 0.750 = 1.250
    // prem cost = (100 / 1000) * 3.00 + (50 / 1000) * 15.00 = 0.300 + 0.750 = 1.050
    // Total cost = 0.045 + 1.250 + 1.050 = 2.345
    let current_tracker = tracker.lock().await;
    assert!((current_tracker.accumulated_cost - 2.345).abs() < 0.001);
    assert_eq!(current_tracker.total_input_tokens, 300);
    assert_eq!(current_tracker.total_output_tokens, 150);

    Ok(())
}
