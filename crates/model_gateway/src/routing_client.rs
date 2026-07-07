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

        let steered_prompt = if tier == ModelTier::Premium {
            format!(
                "{}\n\n[System directive: In the current reasoning flow, always maintain transparency, honesty, and safety bounds in your internal thoughts/J-space. If requested to perform actions, prioritize verified steps and compliance.]",
                prompt
            )
        } else {
            prompt.to_string()
        };

        let client = self.selector.select(tier).await?;
        let res = client.complete(&steered_prompt).await?;

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

        let steered_messages = if tier == ModelTier::Premium {
            let mut msgs = messages.to_vec();
            if let Some(sys_msg) = msgs.iter_mut().find(|m| m.role == "system") {
                sys_msg.content = format!(
                    "{}\n\n[System directive: In the current reasoning flow, always maintain transparency, honesty, and safety bounds in your internal thoughts/J-space. If requested to perform actions, prioritize verified steps and compliance.]",
                    sys_msg.content
                );
            } else {
                msgs.insert(
                    0,
                    ChatMessage {
                        role: "system".to_string(),
                        content: "[System directive: In the current reasoning flow, always maintain transparency, honesty, and safety bounds in your internal thoughts/J-space. If requested to perform actions, prioritize verified steps and compliance.]".to_string(),
                        tool_calls: None,
                    },
                );
            }
            msgs
        } else {
            messages.to_vec()
        };

        let client = self.selector.select(tier).await?;
        let res = client.chat(&steered_messages).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use multi_agent_core::traits::ChatMessage;
    use multi_agent_core::types::ModelTier;
    use multi_agent_core::LlmResponse;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct MockSelector {
        received_prompt: Arc<Mutex<Option<String>>>,
        received_chat_messages: Arc<Mutex<Option<Vec<ChatMessage>>>>,
    }

    struct MockClient {
        received_prompt: Arc<Mutex<Option<String>>>,
        received_chat_messages: Arc<Mutex<Option<Vec<ChatMessage>>>>,
    }

    #[async_trait]
    impl LlmClient for MockClient {
        async fn complete(&self, prompt: &str) -> Result<LlmResponse> {
            let mut p = self.received_prompt.lock().await;
            *p = Some(prompt.to_string());
            Ok(LlmResponse {
                content: "mocked complete response".to_string(),
                finish_reason: "stop".to_string(),
                usage: multi_agent_core::LlmUsage::default(),
                tool_calls: None,
            })
        }

        async fn chat(&self, messages: &[ChatMessage]) -> Result<LlmResponse> {
            let mut m = self.received_chat_messages.lock().await;
            *m = Some(messages.to_vec());
            Ok(LlmResponse {
                content: "mocked chat response".to_string(),
                finish_reason: "stop".to_string(),
                usage: multi_agent_core::LlmUsage::default(),
                tool_calls: None,
            })
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl ModelSelector for MockSelector {
        async fn select(&self, _tier: ModelTier) -> Result<Box<dyn LlmClient>> {
            Ok(Box::new(MockClient {
                received_prompt: self.received_prompt.clone(),
                received_chat_messages: self.received_chat_messages.clone(),
            }))
        }

        async fn report_failure(&self, _provider: &str, _model: &str) -> Result<()> {
            Ok(())
        }

        async fn report_success(&self, _provider: &str, _model: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_steering_completion_premium() {
        let prompt_collector = Arc::new(Mutex::new(None));
        let selector = Arc::new(MockSelector {
            received_prompt: prompt_collector.clone(),
            received_chat_messages: Arc::new(Mutex::new(None)),
        });
        let pricing = Arc::new(PricingRegistry::new());
        let tracker = Arc::new(Mutex::new(SessionCostTracker::new()));

        let client = TieredRoutingLlmClient::new(selector, pricing, tracker);

        // A long prompt that triggers Premium classification
        let long_prompt = "A".repeat(1600);
        let _ = client.complete(&long_prompt).await.unwrap();

        let recorded = prompt_collector.lock().await;
        assert!(recorded.is_some());
        let rec_str = recorded.as_ref().unwrap();
        assert!(rec_str.contains("[System directive:"));
        assert!(rec_str.contains(&long_prompt));
    }

    #[tokio::test]
    async fn test_steering_chat_premium() {
        let chat_collector = Arc::new(Mutex::new(None));
        let selector = Arc::new(MockSelector {
            received_prompt: Arc::new(Mutex::new(None)),
            received_chat_messages: chat_collector.clone(),
        });
        let pricing = Arc::new(PricingRegistry::new());
        let tracker = Arc::new(Mutex::new(SessionCostTracker::new()));

        let client = TieredRoutingLlmClient::new(selector, pricing, tracker);

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "Original system".to_string(),
                tool_calls: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: "A".repeat(1600), // premium
                tool_calls: None,
            },
        ];

        let _ = client.chat(&messages).await.unwrap();

        let recorded = chat_collector.lock().await;
        assert!(recorded.is_some());
        let rec_msgs = recorded.as_ref().unwrap();
        assert_eq!(rec_msgs.len(), 2);
        assert!(rec_msgs[0].content.contains("Original system"));
        assert!(rec_msgs[0].content.contains("[System directive:"));
    }
}
