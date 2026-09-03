#![deny(unused)]
//! L4 Governance for OpenCoordex.
//!
//! This crate provides:
//! - Budget control (token limits)
//! - Security proxy (request validation)
//! - Distributed tracing
//! - RBAC connector for enterprise IAM
//! - Audit logging
//! - Encrypted secrets management

pub mod approval;
pub mod audit;
pub mod budget;
pub mod guardrails;
pub mod metrics;
pub mod network;
pub mod policy;
pub mod privacy;
pub mod rbac;
pub mod secrets;
pub mod security;
pub mod storage_encryption;
pub mod tracing_layer;

pub use approval::{AutoApproveGate, ChannelApprovalGate};
pub use audit::{
    AuditEntry, AuditFilter, AuditOutcome, AuditStore, InMemoryAuditStore, SqliteAuditStore,
};
pub use budget::TokenBudgetController;
pub use guardrails::{
    CognitiveIntentGuardrail, CompositeGuardrail, Guardrail, GuardrailResult, PiiScanner,
    PromptInjectionDetector, ViolationType,
};
pub use metrics::{setup_metrics_recorder, track_request, track_tokens};
pub use policy::{PolicyDecision, PolicyEngine, PolicyFile, PolicyRule, RuleAction, RuleMatch};
pub use privacy::{DeletionReport, PrivacyController};
pub use rbac::{NoOpRbacConnector, RbacConnector, StaticTokenRbacConnector, UserRoles};
pub use secrets::{AesGcmSecretsManager, EncryptedSecret, SecretsManager};
pub use security::DefaultSecurityProxy;
pub use storage_encryption::EncryptedArtifactStore;
pub use tracing_layer::configure_tracing;
