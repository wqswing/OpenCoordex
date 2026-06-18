//! Cost-Aware Model Routing for Budget Optimization.
//!
//! Provides pricing information and intelligent routing to minimize costs
//! while maintaining quality requirements.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pricing information for a model (per 1K tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Model identifier (e.g., "openai:gpt-4o").
    pub model_id: String,
    /// Cost per 1K input tokens in USD.
    pub input_cost_per_1k: f64,
    /// Cost per 1K output tokens in USD.
    pub output_cost_per_1k: f64,
    /// Relative quality score (1-10).
    pub quality_score: u8,
    /// Average latency in ms.
    pub avg_latency_ms: u32,
}

impl ModelPricing {
    /// Create new pricing info.
    pub fn new(model_id: impl Into<String>, input: f64, output: f64) -> Self {
        Self {
            model_id: model_id.into(),
            input_cost_per_1k: input,
            output_cost_per_1k: output,
            quality_score: 5,
            avg_latency_ms: 1000,
        }
    }

    /// Set quality score.
    pub fn with_quality(mut self, score: u8) -> Self {
        self.quality_score = score.min(10);
        self
    }

    /// Estimate cost for a request.
    pub fn estimate_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let input_cost = (input_tokens as f64 / 1000.0) * self.input_cost_per_1k;
        let output_cost = (output_tokens as f64 / 1000.0) * self.output_cost_per_1k;
        input_cost + output_cost
    }
}

/// Registry of model pricing information.
pub struct PricingRegistry {
    models: HashMap<String, ModelPricing>,
}

impl PricingRegistry {
    /// Create a new pricing registry.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    /// Create with default OpenAI/Anthropic pricing.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();

        // OpenAI models (2024 pricing estimates)
        registry.register(ModelPricing::new("openai:gpt-4o-mini", 0.15, 0.60).with_quality(7));
        registry.register(ModelPricing::new("openai:gpt-4o", 5.00, 15.00).with_quality(9));
        registry.register(ModelPricing::new("openai:gpt-4-turbo", 10.00, 30.00).with_quality(9));

        // Anthropic models
        registry.register(
            ModelPricing::new("anthropic:claude-3-haiku-20240307", 0.25, 1.25).with_quality(7),
        );
        registry.register(
            ModelPricing::new("anthropic:claude-3-5-sonnet-20241022", 3.00, 15.00).with_quality(10),
        );

        registry
    }

    /// Register a model's pricing.
    pub fn register(&mut self, pricing: ModelPricing) {
        self.models.insert(pricing.model_id.clone(), pricing);
    }

    /// Get pricing for a model.
    pub fn get(&self, model_id: &str) -> Option<&ModelPricing> {
        self.models.get(model_id)
    }

    /// Get all models sorted by cost (cheapest first).
    pub fn sorted_by_cost(&self) -> Vec<&ModelPricing> {
        let mut models: Vec<_> = self.models.values().collect();
        models.sort_by(|a, b| {
            let cost_a = a.input_cost_per_1k + a.output_cost_per_1k;
            let cost_b = b.input_cost_per_1k + b.output_cost_per_1k;
            cost_a.partial_cmp(&cost_b).unwrap()
        });
        models
    }

    /// Get all models sorted by quality (best first).
    pub fn sorted_by_quality(&self) -> Vec<&ModelPricing> {
        let mut models: Vec<_> = self.models.values().collect();
        models.sort_by_key(|b| std::cmp::Reverse(b.quality_score));
        models
    }

    /// Find cheapest model meeting minimum quality.
    pub fn cheapest_with_quality(&self, min_quality: u8) -> Option<&ModelPricing> {
        self.sorted_by_cost()
            .into_iter()
            .find(|m| m.quality_score >= min_quality)
    }
}

impl Default for PricingRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Cost tracking for a session.
#[derive(Debug, Clone, Default)]
pub struct SessionCostTracker {
    /// Total input tokens used.
    pub total_input_tokens: u64,
    /// Total output tokens used.
    pub total_output_tokens: u64,
    /// Accumulated cost in USD.
    pub accumulated_cost: f64,
    /// Budget limit in USD.
    pub budget_limit: Option<f64>,
}

impl SessionCostTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a budget limit.
    pub fn with_budget(mut self, limit: f64) -> Self {
        self.budget_limit = Some(limit);
        self
    }

    /// Record usage.
    pub fn record(&mut self, pricing: &ModelPricing, input_tokens: u64, output_tokens: u64) {
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;
        self.accumulated_cost += pricing.estimate_cost(input_tokens, output_tokens);
    }

    /// Check if budget is exceeded.
    pub fn is_over_budget(&self) -> bool {
        self.budget_limit
            .is_some_and(|limit| self.accumulated_cost >= limit)
    }

    /// Get remaining budget.
    pub fn remaining_budget(&self) -> Option<f64> {
        self.budget_limit
            .map(|limit| (limit - self.accumulated_cost).max(0.0))
    }

    /// Get usage percentage.
    pub fn usage_percentage(&self) -> Option<f64> {
        self.budget_limit
            .map(|limit| (self.accumulated_cost / limit) * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_pricing() {
        let pricing = ModelPricing::new("test:model", 1.0, 2.0).with_quality(8);

        // 1000 input + 500 output = $1 + $1 = $2
        let cost = pricing.estimate_cost(1000, 500);
        assert!((cost - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_pricing_registry() {
        let registry = PricingRegistry::with_defaults();

        let gpt4o = registry.get("openai:gpt-4o").unwrap();
        assert_eq!(gpt4o.quality_score, 9);

        // Cheapest should be gpt-4o-mini
        let cheapest = registry.sorted_by_cost();
        assert_eq!(cheapest[0].model_id, "openai:gpt-4o-mini");
    }

    #[test]
    fn test_cheapest_with_quality() {
        let registry = PricingRegistry::with_defaults();

        // Min quality 8 should give us gpt-4o (first quality 9+ that's cheap)
        let model = registry.cheapest_with_quality(9).unwrap();
        assert!(model.quality_score >= 9);
    }

    #[test]
    fn test_session_cost_tracker() {
        // Pricing: $1/1K input, $2/1K output
        // 500 input = $0.50, 250 output = $0.50, total = $1.00 per record
        let mut tracker = SessionCostTracker::new().with_budget(1.5);
        let pricing = ModelPricing::new("test", 1.0, 2.0);

        // First record: $1.00 (under $1.50 budget)
        tracker.record(&pricing, 500, 250);
        assert!(!tracker.is_over_budget());
        assert!((tracker.accumulated_cost - 1.0).abs() < 0.001);

        // Second record: $2.00 total (over $1.50 budget)
        tracker.record(&pricing, 500, 250);
        assert!(tracker.is_over_budget());
    }
}
