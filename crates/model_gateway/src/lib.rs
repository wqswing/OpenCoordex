#![deny(unused)]
//! L-M Model Gateway for OpenCoordex.
//!
//! This crate provides:
//! - Model selection and load balancing
//! - Provider health tracking and circuit breaker
//! - Fallback and retry logic
//! - Rig LLM client adapter

pub mod config;
pub mod pricing;
pub mod providers;
pub mod rig_client;
pub mod routing_client;
pub mod selector;

pub use pricing::{ModelPricing, PricingRegistry, SessionCostTracker};
pub use providers::{MockLlmClient, ProviderRegistry};
pub use rig_client::{create_default_client, RigConfig, RigLlmClient, RigProvider};
pub use routing_client::TieredRoutingLlmClient;
pub use selector::AdaptiveModelSelector;

use config::ProviderConfig;
use secrecy::Secret;

/// Create an LLM client from configuration with optional explicit API keys.
pub fn create_client_from_config(
    config: &ProviderConfig,
    openai_key: Option<Secret<String>>,
    anthropic_key: Option<Secret<String>>,
) -> multi_agent_core::Result<RigLlmClient> {
    // Simple strategy: Use the first provider/model found in the config
    // In the future, we could have a "default" flag or selection logic.
    let openai_key = openai_key.or_else(|| std::env::var("OPENAI_API_KEY").ok().map(Secret::new));
    let anthropic_key =
        anthropic_key.or_else(|| std::env::var("ANTHROPIC_API_KEY").ok().map(Secret::new));

    for provider in &config.providers {
        match provider.name.to_lowercase().as_str() {
            "openai" => {
                if openai_key.is_none() {
                    continue;
                }
                if let Some(model) = provider.models.first() {
                    let mut rig_cfg = RigConfig::openai(&model.id);
                    if let Some(key) = openai_key.clone() {
                        rig_cfg = rig_cfg.with_api_key(key);
                    }
                    return Ok(RigLlmClient::new(rig_cfg));
                }
            }
            "anthropic" => {
                if anthropic_key.is_none() {
                    continue;
                }
                if let Some(model) = provider.models.first() {
                    let mut rig_cfg = RigConfig::anthropic(&model.id);
                    if let Some(key) = anthropic_key.clone() {
                        rig_cfg = rig_cfg.with_api_key(key);
                    }
                    return Ok(RigLlmClient::new(rig_cfg));
                }
            }
            _ => continue,
        }
    }

    Err(multi_agent_core::Error::ModelProvider(
        "No supported provider found in config".to_string(),
    ))
}
