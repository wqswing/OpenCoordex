use crate::pricing::{PricingRegistry, SessionCostTracker};
use async_trait::async_trait;
use multi_agent_core::{
    traits::{ChatMessage, LlmClient, LlmResponse, ModelSelector},
    types::ModelTier,
    Result,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Proxy LLM client that dynamically routes prompts to different model tiers
/// (Fast, Standard, Premium) based on complexity and records pricing metrics.
pub struct TieredRoutingLlmClient {
    selector: Arc<dyn ModelSelector>,
    pricing: Arc<PricingRegistry>,
    tracker: Arc<Mutex<SessionCostTracker>>,
}

impl TieredRoutingLlmClient {
    pub fn new(
        selector: Arc<dyn ModelSelector>,
        pricing: Arc<PricingRegistry>,
        tracker: Arc<Mutex<SessionCostTracker>>,
    ) -> Self {
        Self {
            selector,
            pricing,
            tracker,
        }
    }

    /// Classify the complexity tier based on prompt content
    pub fn classify_prompt(&self, prompt: &str) -> ModelTier {
        let trimmed = prompt.trim();

        // 1. Premium criteria: heavy reasoning, planning or evaluation
        if trimmed.contains("FINAL ANSWER:")
            || trimmed.contains("EVALUATION CRITERIA")
            || trimmed.contains("You are an objective AI evaluator")
            || trimmed.contains("marketing plan")
            || trimmed.len() > 1500
        {
            return ModelTier::Premium;
        }

        // 2. Fast criteria: simple ReAct intermediate steps, tool definitions, calculator calls
        if trimmed.contains("THOUGHT:")
            || trimmed.contains("ACTION:")
            || trimmed.contains("calculator")
            || trimmed.contains("weather")
            || trimmed.len() < 100
        {
            return ModelTier::Fast;
        }

        // Default to Standard tier
        ModelTier::Standard
    }

    /// Helper to classify complexity across messages
    pub fn classify_messages(&self, messages: &[ChatMessage]) -> ModelTier {
        if let Some(last_msg) = messages.last() {
            self.classify_prompt(&last_msg.content)
        } else {
            ModelTier::Standard
        }
    }

    /// Access the underlying cost tracker
    pub fn cost_tracker(&self) -> &Arc<Mutex<SessionCostTracker>> {
        &self.tracker
    }
}

#[async_trait]
impl LlmClient for TieredRoutingLlmClient {
    async fn complete(&self, prompt: &str) -> Result<LlmResponse> {
        let tier = self.classify_prompt(prompt);
        tracing::debug!(tier = ?tier, "Tiered routing decision for completion");

        let client = self.selector.select(tier).await?;
        let res = client.complete(prompt).await?;

        let model_id = match tier {
            ModelTier::Fast => "openai:gpt-4o-mini",
            ModelTier::Standard => "openai:gpt-4o",
            ModelTier::Premium => "anthropic:claude-3-5-sonnet-20241022",
        };

        if let Some(pricing) = self.pricing.get(model_id) {
            let mut tracker = self.tracker.lock().await;
            tracker.record(
                pricing,
                res.usage.prompt_tokens,
                res.usage.completion_tokens,
            );
            tracing::debug!(
                accumulated_cost = tracker.accumulated_cost,
                "Recorded LLM cost for {}",
                model_id
            );
        }

        Ok(res)
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<LlmResponse> {
        let tier = self.classify_messages(messages);
        tracing::debug!(tier = ?tier, "Tiered routing decision for chat");

        let client = self.selector.select(tier).await?;
        let res = client.chat(messages).await?;

        let model_id = match tier {
            ModelTier::Fast => "openai:gpt-4o-mini",
            ModelTier::Standard => "openai:gpt-4o",
            ModelTier::Premium => "anthropic:claude-3-5-sonnet-20241022",
        };

        if let Some(pricing) = self.pricing.get(model_id) {
            let mut tracker = self.tracker.lock().await;
            tracker.record(
                pricing,
                res.usage.prompt_tokens,
                res.usage.completion_tokens,
            );
            tracing::debug!(
                accumulated_cost = tracker.accumulated_cost,
                "Recorded LLM cost for {}",
                model_id
            );
        }

        Ok(res)
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let client = self.selector.select(ModelTier::Fast).await?;
        client.embed(text).await
    }
}
