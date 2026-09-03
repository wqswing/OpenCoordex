# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v1.2.0] - 2026-07-07

### 🔍 Jacobian Lens (J-space) 认知审计与对齐
- **GWT 全局工作空间状态提炼 (Active Workspace Extraction)**: 实现了 `ActiveWorkspaceCapability`。在 ReAct 推理循环前动态压缩提炼当前的 Objective、Constraints 与 Verified facts，防范长历史上下文导致的注意力衰减与安全规则漂移。
- **隐式 Steering 注意力导向 (Implicit Steering Injection)**: 在 `TieredRoutingLlmClient` 对接 Premium（高危/高复度）模型请求时，自动注入合规、真诚和防欺骗底线 prompt 指针，从概率上约束模型行为。

### 🛡️ 沙箱动态约束拦截与主动心理拷问 (Dynamic Sandboxing & Proactive Grilling)
- **沙箱约束动态传递与执行拦截 (Sandbox Constraint Listener & Enforcement)**: 在 core 层中引入 `ConstraintListener` 观察者模式。沙箱管理器 (`SandboxManager`) 订阅 J-space 限制（如 `No python`、`Read-Only`），并在执行 shell 指令/写文件操作前进行动态强力阻断，抛出 `SecurityViolation` 异常。
- **影子主动心理拷问插件 (Proactive Grilling Capability)**: 在 `AgentCapability` Trait 中新增前置 `on_tool_proposed` 拦截生命周期。当高危工具（如 `sandbox_shell`）触发时，自动克隆影子会话并抛出反事实安全红线问询，结合 Judge LLM 进行评估以防范欺骗式对齐（Deceptive Alignment）。

### 🧠 开源本地模型 J-lens 表征概念投影 (Local J-lens Projection)
- **隐藏层对齐概念投影探针 (Local J-lens Evaluator)**: 在 `multi_agent_model_gateway` 中新增 `LocalJLensEvaluator`，模拟或计算本地开源大模型（Llama/Qwen）词表 Unembedding 表征层梯度，将隐藏状态投影至 honesty, evasion 和 deception 探针方向，实时测算异常风险。

### 🖥️ 审计控制面板与鉴权 (Cognitive Control Plane UI)
- **认知审计 API 端点与鉴权拦截 (Cognitive Audit APIs)**: 增加具备管理员与审计员级 RBAC Bearer 鉴权锁的 `/cognitive/metrics` 与 `/cognitive/anomalies` 端点。
- **可视化 J-space 活性看板 (Live GWT Sidebar Dashboard)**: 仪表盘侧边栏新增“认知审计”板块，实时加载三轴活性指标、审计异常历史清单以及 manual override 处理操作。

## [v1.10.0] - 2026-06-17

### 🌐 Standalone LLM Gateway & 智能调度

- **独立 LLM Gateway 服务模式 (Standalone LLM Gateway Service)**: 将底层的 `multi_agent_model_gateway` 升级为独立的 HTTP 反向代理网关。新增标准的 OpenAI 兼容端点 `/v1/chat/completions`，支持多租户 Token 验证、请求解包映射与结果打包。
- **企业级 Token 精细化控制与多级 LLM 智能分发路由 (Enterprise LLM Tiering & Complexity Routing)**: 基于输入 Prompt 复杂度与 Token 预算，自动将请求分发至 `Fast`、`Standard` 与 `Premium` 模型层级。结合 `SessionCostTracker` 实时记录并拦截超预算请求。

### 🧪 自动化评测套件 (Evaluation Harness)

- **评测套件与基准测试 (Test Harness)**: 引入独立的 `multi_agent_harness` 模块，支持配置化的测试集（Suite）和测试用例（TestCase）。支持多种形式的断言校验（精确匹配、子串包含、正则表达式匹配、JSON Schema 格式校验以及基于大模型的 LLM Judge 判定）。

### 🛡️ KYA 理念、职责分离与多级审批权限校验

- **KYA (Know Your Agent) 理念落地**: 在 `ApprovalRequest` 中引入不可篡改的 Agent 凭证（包括 Agent ID、Agent 类型、运行模型以及系统提示词 SHA-256 指纹哈希），确保 Agent 的每一项敏感行为都具备清晰的密码学身份锚定。
- **职责分配与多级审批权限校验 (Separation of Duties - SoD)**: 重构审批流拦截器，根据工具风险等级（Medium, High, Critical）强制校验审批用户的 RBAC 角色权限（Admin, Security Officer, Compliance, Operator），防止越权审批，强化多级级联风控。
- **用户上下文插入 (OIDC/JWT Context)**: 从 OIDC 校验后的请求扩展中提取用户身份，实现端到端的责任归属追溯。

### 💾 吞吐性能、合规性与不可篡改审计链优化

- **防篡改与不可篡改审计链路 (Immutable Audit Trails)**: 引入 SHA-256 块哈希链机制，强关联上一条审计记录的 Hash 签名，实现防篡改审计日志。任何绕过系统的物理修改都将引发链式校验失败。
- **吞吐性能优化：启用 SQLite 数据库连接池与 WAL 模式**: 为 `SqliteAuditStore` 引入线程安全的轻量级连接池，全面开启 **WAL (Write-Ahead Logging)** 预写日志与 `NORMAL` 同步模式，配置 5 秒 Busy Timeout，在高并发吞吐下彻底消除 SQLite 锁冲突。
- **合规性优化：兼容 GDPR 的"被遗忘权"脱敏审计链**: 实现 `erase_user` GDPR 脱敏清空接口。在满足用户隐私擦除需求时，对日志进行 `"REDACTED"` 替换并清空敏感元数据，同时**自动链式重算后续所有数据行的哈希值与父链指向**，在保障合规脱敏的前提下维系密码学审计链路的完整性和校验通过。
- **内存与稳定性优化：基于游标 (Cursor) 的分页审计完整性校验**: 重构 `verify_integrity` 链式校验算法，使用基于游标的 SQL 分页拉取机制（每次加载 1,000 条），在校验包含数十万条记录的长链路时防止内存 OOM 崩溃，保证极佳的系统稳定性。

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
