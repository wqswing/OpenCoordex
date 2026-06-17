# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v1.10.0] - 2026-06-17

### 🧪 Evaluation Harness & LLM Routing
- **Test Harness**: Introduced `multi_agent_harness` supporting test case assertions (exact match, contains, regex, json-schema, LLM judge), suites execution, and mock controller loops.
- **Complexity-Based LLM Routing**: Implemented `TieredRoutingLlmClient` dynamically routing requests to `Fast`, `Standard`, or `Premium` models, integrated with `SessionCostTracker` for cost calculation.

### 🛡️ Enterprise KYA & Access Control
- **KYA Attestation**: Integrated KYA properties (agent ID, agent type, model name, and system prompt SHA-256 fingerprint hash) inside `ApprovalRequest`.
- **Separation of Duties (SoD)**: Enforced role-based clearances (Admin, Security Officer, Compliance, Operator) checking against tool risk levels on `ChannelApprovalGate` before executions.
- **OIDC/JWT Authentication Context**: Extracted auth details from REST and WebSocket endpoints to pass context to the gate.

### 💾 Cryptographic Audit Trail Optimizations
- **Connection Pooling & WAL**: Optimized `SqliteAuditStore` with thread-safe connection pooling, SQLite WAL mode, and a 5-second busy timeout.
- **GDPR Redaction & Re-Chaining**: Re-implemented `erase_user` to redact records while recompute hash chains to maintain audit cryptographic validity.
- **Cursor Paginated Verification**: Re-implemented integrity verification using a 1,000-row paginated cursor to avoid memory OOMs on large databases.

## [Unreleased] - 2026-02-18

### Branding
- Product/project name standardized to **OpenCoordex** for external positioning.
- Historical internal crate namespace (`multi_agent_*`) kept for compatibility.

### ✅ Release Closure (P0/P1/P2 convergence)
- **Configuration reliability**:
  - Fixed `config/default.toml` required schema coverage for runtime startup.
  - Added required governance/safety/encryption/tls default sections to avoid boot-time decode failures.
- **Quality gates**:
  - Resolved rustfmt/clippy blockers in governance modules.
  - Revalidated release gate sequence: `fmt -> clippy -> test -> smoke`.
- **Gateway and orchestration hardening**:
  - Typed gateway contract alignment (request/response/event/error-code stability).
  - Side-effect idempotency key support retained as release baseline.
  - Lane-based scheduling (`session lane + global lane`) retained as controller baseline.
- **Enterprise routing strategy (P2-first block)**:
  - Explicit routing policy dimensions: `channel/account/peer`.
  - Added simulation and publish admin APIs for safe strategy rollout.
- **Memory writeback loop (P1)**:
  - Session memory writeback loop maintained (`YYYY-MM-DD.md` + `MEMORY.md`).
  - Pre-compaction flush integrated into governance/capability lifecycle.
- **Release docs**:
  - Updated `README.md` and `ARCHITECTURE.md` to reflect current production architecture and operations endpoints.

## [v1.0.5] - 2026-02-17

### 🛡️ Egress & Policy Hardening (P0/P1)
- **Egress Convergence**:
    - Centralized all network egress logic in `crates/governance` -> `fetch_with_policy`.
    - Enforced strict allow/deny list checks, IP filtering, and SSRF protection for all Research Agent requests.
    - Added redirect safety (dropping bodies on GET redirects) and max response size limits.
- **Policy-Driven Approval**:
    - Replaced hardcoded approval triggers with dynamic risk scoring from `policy.yaml`.
    - Integrated `PolicyEngine` into `ResearchOrchestrator` to evaluate plan risk before execution.
- **Authentication & Access**:
    - **Console Auth**: Added `x-admin-token` header and cookie support for admin API access.
    - **Configurable External Access**: New `admin_allow_external_access` config to safely expose admin endpoints (e.g., K8s Ingress).
- **Robustness**:
    - Fixed `std::fs` panic in policy saving by ensuring directory existence.

## [v1.0.4] - 2026-02-17

### 🚀 Major Features
- **Nexus Premium UI**: Complete dashboard overhaul with glassmorphism, neon accents, and improved layout.
- **Enhanced Governance**:
    - **Approval Timeline**: Visual tracking of human-in-the-loop decisions.
    - **Risk Scoring**: Real-time risk level indicators for sensitive agent actions.
- **Production Hardening**:
    - **Nonce-based Approval**: End-to-end replay protection for binary decisions.
    - **Encrypted Secrets Migration**: Automated migration and AES-256 encryption for provider keys.
    - **Egress Monitoring**: Real-time HTTP audit logging in the Research Agent.

## [v0.8.0] - 2026-01-18

### 🚀 Major Features
- **Enterprise Governance Layer**: 
    - Introduced a new `governance` crate managing Security, Audit, and Quotas.
    - Implemented `RbacConnector` trait with `OidcRbacConnector` (Keycloak/Auth0 RS256) and `NoOp` implementations.
    - Implemented `SecretsManager` trait with `AesGcmSecretsManager` (AES-256-GCM encryption).
    - Implemented `AuditStore` trait with `FileAuditStore` (JSON Lines persistence).

- **Admin Management Dashboard**:
    - Web-based UI at `/` serving static assets via `rust-embed`.
    - Features: Configuration inspector, Real-time Metrics view, and Audit Log explorer.
    - Protected by Bearer Token Authentication (verified against RBAC).

- **Observability**:
    - Integrated `metrics` and `metrics-exporter-prometheus` for real-time telemetry.
    - Admin API `/admin/metrics` endpoint exposes global system metrics.

### 🛡️ Security Hardening
- **Encryption at Rest**: All sensitive configuration secrets are now encrypted in memory and transit.
- **Audit Trails**: Critical system actions (config changes, access) are persistently logged.
- **Identity Integration**: Added support for external OIDC Identity Providers.

### 🔧 Improvements
- **Performance**: Upgraded `ArtifactStore` and `SessionStore` to use `DashMap` for high-concurrency read/write operations.
- **Testing**: Added comprehensive integration test suite (`tests/integration_v0_8.rs`) verifying the entire security and management pipeline.

## [v0.7.0] - 2024-12-XX

### 🚀 Major Features
- **Vector Database Integration**:
    - Added `qdrant-client` support for production-grade RAG workloads.
    - Implemented `QdrantMemoryStore` for persistent vector embeddings.

- **Advanced LLM Capabilities**:
    - Native support for **OpenAI Function Calling**, enabling structured and reliable tool execution.
    - Refactored `Controller` to use a unified `ActionParser` supporting both text-based (ReAct) and structured (JSON) outputs.

- **Architectural Refactoring**:
    - **Modular Traits**: Split monolithic core traits into domain-specific modules (`gateway`, `controller`, `skills`, `store`, `governance`, `llm`) to strictly enforce the 6-layer architecture.
    - **Mock Infrastructure**: Introduced `crates/core/src/mocks.rs` providing reusable mocks (`MockLlm`, `MockToolRegistry`, `RecordTool`) for all layers.

### 📦 Dependencies
- Upgraded `redis` crate to v0.27.
- Migrated to latest `aws-config` and `aws-sdk-s3`.
- Removed deprecated async connection methods.

## [v0.6.0] - 2024-11-XX
- Initial implementation of the 6-Layer Architecture.
- Basic ReAct Agent implementation.
- In-memory storage and naive semantic cache.
