#![deny(unused)]
//! OpenCoordex - Multi-Agent AI Platform
//!
//! A layered, Rust-based multi-agent architecture supporting multi-modal ingestion,
//! intelligent routing, ReAct-based orchestration, and production-grade resilience.

use multi_agent_sandbox::SandboxEngine;
use std::sync::Arc;

use multi_agent_controller::ReActController;
use multi_agent_core::traits::{ArtifactStore, SessionStore, ToolRegistry};
use multi_agent_gateway::{DefaultRouter, GatewayConfig, GatewayServer, InMemorySemanticCache};
use multi_agent_skills::{CalculatorTool, DefaultToolRegistry, EchoTool};
use multi_agent_store::{
    knowledge::SqliteKnowledgeStore, InMemorySessionStore, InMemoryStore, RedisSessionStore,
    S3ArtifactStore, TieredStore,
};
use secrecy::ExposeSecret;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    // Load configuration
    let app_config = multi_agent_core::config::AppConfig::load().map_err(|e| {
        eprintln!("Failed to load configuration: {}", e);
        e
    })?;

    // Initialize tracing
    let rust_log = std::env::var("RUST_LOG").ok(); // Still allow env override for RUST_LOG as it's common
    let otel_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
    multi_agent_governance::configure_tracing(
        rust_log.as_deref(),
        otel_endpoint.as_deref(),
        app_config.governance.json_logs,
    )?;

    tracing::info!("Starting OpenCoordex v{}", env!("CARGO_PKG_VERSION"));

    // =========================================================================
    // Initialize L3: Artifact Store
    // =========================================================================
    let (store_raw, store): (
        Arc<dyn multi_agent_core::traits::Erasable>,
        Arc<dyn ArtifactStore>,
    ) = if let Some(bucket) = &app_config.store.s3_bucket {
        let endpoint = app_config.store.s3_endpoint.as_deref();
        tracing::info!(bucket = %bucket, endpoint = ?endpoint, "Initializing S3 Artifact Store (Tiered)");

        let s3 = Arc::new(S3ArtifactStore::new(bucket, "", endpoint).await);
        let hot = Arc::new(InMemoryStore::new());
        let tiered = Arc::new(TieredStore::new(hot).with_cold(s3));
        (
            tiered.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
            tiered as Arc<dyn ArtifactStore>,
        )
    } else {
        tracing::info!("Initializing In-Memory Artifact Store");
        let memory = Arc::new(InMemoryStore::new());
        (
            memory.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
            memory as Arc<dyn ArtifactStore>,
        )
    };

    // Data-at-rest Encryption
    let store = if app_config.store.encryption.enabled {
        if let Some(key) = &app_config.store.encryption.master_key {
            tracing::info!("🔒 Artifact Store Encryption ENABLED");
            Arc::new(
                multi_agent_governance::EncryptedArtifactStore::new(store, key.expose_secret())
                    .map_err(|e| {
                        multi_agent_core::Error::governance(format!(
                            "Encryption init failed: {}",
                            e
                        ))
                    })?,
            )
        } else {
            tracing::warn!(
                "Encryption enabled but no master key provided - falling back to plaintext"
            );
            store
        }
    } else {
        store
    };

    // Secrets manager for encrypting API keys
    let secrets_path = std::path::PathBuf::from("secrets.json");
    let master_key_bytes = if let Some(key) = &app_config.store.encryption.master_key {
        use secrecy::ExposeSecret;
        let key_str = key.expose_secret();
        let mut key_bytes = [0u8; 32];
        // naive padding/truncation for demo
        let bytes = key_str.as_bytes();
        let len = bytes.len().min(32);
        key_bytes[0..len].copy_from_slice(&bytes[0..len]);
        Some(key_bytes)
    } else {
        None
    };

    let secrets_manager: Arc<dyn multi_agent_governance::SecretsManager> = Arc::new(
        multi_agent_governance::secrets::FilePersistentSecretsManager::new(
            secrets_path,
            master_key_bytes,
        )
        .await?,
    );

    // M11.2: Secrets Migration
    // Check for legacy onboarding.json and migrate to SecretsManager
    let legacy_path = std::path::PathBuf::from(".sovereign_claw/onboarding.json");
    if legacy_path.exists() {
        tracing::info!("Found legacy onboarding.json - migrating secrets...");
        match tokio::fs::read_to_string(&legacy_path).await {
            Ok(content) => {
                let json: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();
                let mut migrated = false;

                if let Some(key) = json.get("openai_key").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        if let Err(e) = secrets_manager.store("openai_api_key", key).await {
                            tracing::error!("Failed to migrate OpenAI key: {}", e);
                        } else {
                            migrated = true;
                        }
                    }
                }

                if let Some(key) = json.get("anthropic_key").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        if let Err(e) = secrets_manager.store("anthropic_api_key", key).await {
                            tracing::error!("Failed to migrate Anthropic key: {}", e);
                        } else {
                            migrated = true;
                        }
                    }
                }

                if migrated {
                    let new_path = legacy_path.with_extension("json.migrated");
                    if let Err(e) = tokio::fs::rename(&legacy_path, &new_path).await {
                        tracing::error!("Failed to rename legacy onboarding file: {}", e);
                    } else {
                        tracing::info!(
                            "Secrets migrated successfully. Renamed legacy file to {:?}",
                            new_path
                        );
                    }
                }
            }
            Err(e) => tracing::error!("Failed to read legacy onboarding file: {}", e),
        }
    }

    // Initialize Session Store
    let (session_store_raw, session_store): (
        Arc<dyn multi_agent_core::traits::Erasable>,
        Arc<dyn SessionStore>,
    ) = if let Some(redis_url) = &app_config.store.redis_url {
        tracing::info!(url = %redis_url, "Initializing Redis Session Store");
        let redis = Arc::new(RedisSessionStore::new(
            redis_url,
            "opencoordex:session",
            3600 * 24,
        )?);
        (
            redis.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
            redis as Arc<dyn SessionStore>,
        )
    } else {
        tracing::info!("Initializing In-Memory Session Store");
        let memory = Arc::new(InMemorySessionStore::new());
        (
            memory.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
            memory as Arc<dyn SessionStore>,
        )
    };

    // =========================================================================
    // Initialize L2: Skills & Tools
    // =========================================================================
    let tools = Arc::new(DefaultToolRegistry::new());

    // Register built-in tools
    tools.register(Box::new(EchoTool)).await?;
    tools.register(Box::new(CalculatorTool)).await?;

    // =========================================================================
    // Initialize Sandbox (Sovereign Execution Plane)
    // =========================================================================
    let sandbox_manager = match multi_agent_sandbox::DockerSandbox::new() {
        Ok(engine) => {
            let engine = std::sync::Arc::new(engine);
            if engine.is_available().await {
                let config = multi_agent_sandbox::SandboxConfig::default();
                let manager = Arc::new(multi_agent_sandbox::SandboxManager::new(engine, config));

                // Register sandbox tools
                tools
                    .register(Box::new(multi_agent_sandbox::SandboxShellTool::new(
                        manager.clone(),
                    )))
                    .await?;
                tools
                    .register(Box::new(multi_agent_sandbox::SandboxWriteFileTool::new(
                        manager.clone(),
                    )))
                    .await?;
                tools
                    .register(Box::new(multi_agent_sandbox::SandboxReadFileTool::new(
                        manager.clone(),
                    )))
                    .await?;
                tools
                    .register(Box::new(multi_agent_sandbox::SandboxListFilesTool::new(
                        manager.clone(),
                    )))
                    .await?;

                tracing::info!("🐳 Sovereign Sandbox initialized (Docker available)");
                Some(manager)
            } else {
                tracing::warn!("Docker daemon not reachable — sandbox tools disabled");
                None
            }
        }
        Err(e) => {
            tracing::warn!("Docker not available ({}). Sandbox tools disabled.", e);
            None
        }
    };

    // Network Policy setup
    // Load from network_policy.json if exists, else AppConfig
    let policy_path = std::path::PathBuf::from("network_policy.json");
    let initial_policy = if policy_path.exists() {
        tracing::info!("Loading network policy from network_policy.json");
        let content = tokio::fs::read_to_string(&policy_path).await?;
        serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::error!(
                "Failed to parse network_policy.json: {}. Using default config.",
                e
            );
            multi_agent_governance::network::NetworkPolicy::new(
                app_config.governance.allow_domains.clone(),
                app_config.governance.deny_domains.clone(),
                vec![80, 443],
            )
        })
    } else {
        multi_agent_governance::network::NetworkPolicy::new(
            app_config.governance.allow_domains.clone(),
            app_config.governance.deny_domains.clone(),
            vec![80, 443],
        )
    };

    let network_policy = Arc::new(tokio::sync::RwLock::new(initial_policy));

    // Register Network tools
    tools
        .register(Box::new(multi_agent_skills::network::FetchTool::new(
            network_policy.clone(),
            app_config.safety.clone(),
        )) as Box<dyn multi_agent_core::traits::Tool>)
        .await?;

    if let Some(sm) = &sandbox_manager {
        tools
            .register(Box::new(multi_agent_skills::network::DownloadTool::new(
                network_policy.clone(),
                app_config.safety.clone(),
                sm.clone(),
            )) as Box<dyn multi_agent_core::traits::Tool>)
            .await?;
    }

    let _sandbox_manager = sandbox_manager; // keep alive

    tracing::info!(tools_count = tools.len(), "L2 Skills registry initialized");

    // =========================================================================
    // Initialize LLM Client for embeddings & Routing
    // =========================================================================
    use multi_agent_core::traits::LlmClient;

    let llm_client: Arc<dyn LlmClient> = {
        let providers_path = std::path::Path::new("providers.json");
        if providers_path.exists() {
            tracing::info!("Loading LLM config from providers.json");
            match multi_agent_model_gateway::config::ProviderConfig::load(providers_path).await {
                Ok(cfg) => {
                    let client_result = {
                        let openai_key =
                            if let Some(k) = app_config.model_gateway.openai_api_key.clone() {
                                Some(k)
                            } else {
                                secrets_manager
                                    .retrieve("openai_api_key")
                                    .await?
                                    .map(secrecy::Secret::new)
                            };
                        let anthropic_key =
                            if let Some(k) = app_config.model_gateway.anthropic_api_key.clone() {
                                Some(k)
                            } else {
                                secrets_manager
                                    .retrieve("anthropic_api_key")
                                    .await?
                                    .map(secrecy::Secret::new)
                            };

                        multi_agent_model_gateway::create_client_from_config(
                            &cfg,
                            openai_key,
                            anthropic_key,
                        )
                    };
                    match client_result {
                        Ok(client) => Arc::new(client),
                        Err(e) => {
                            tracing::warn!(
                                "Failed to create client from config: {}. Fallback to env vars.",
                                e
                            );
                            match multi_agent_model_gateway::create_default_client() {
                                Ok(client) => Arc::new(client),
                                Err(_) => {
                                    Arc::new(multi_agent_model_gateway::MockLlmClient::new("dummy"))
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to parse providers.json: {}. Fallback to env vars.",
                        e
                    );
                    match multi_agent_model_gateway::create_default_client() {
                        Ok(client) => Arc::new(client),
                        Err(_) => Arc::new(multi_agent_model_gateway::MockLlmClient::new("dummy")),
                    }
                }
            }
        } else {
            tracing::info!("No providers.json found. Using environment variables.");
            match multi_agent_model_gateway::create_default_client() {
                Ok(client) => Arc::new(client),
                Err(e) => {
                    tracing::warn!("Failed to create default LLM client: {}. Semantic cache will fallback to exact match.", e);
                    Arc::new(multi_agent_model_gateway::MockLlmClient::new("dummy"))
                }
            }
        }
    };

    // Initialize Model Registry & Adaptive Model Selector
    let model_registry = Arc::new(multi_agent_model_gateway::ProviderRegistry::new());
    model_registry.register("openai", "gpt-4o-mini", llm_client.clone());
    model_registry.register("openai", "gpt-4o", llm_client.clone());
    model_registry.register("anthropic", "claude-3-5-sonnet-20241022", llm_client.clone());

    let model_selector = Arc::new(multi_agent_model_gateway::AdaptiveModelSelector::new(model_registry));
    let pricing_registry = Arc::new(multi_agent_model_gateway::PricingRegistry::with_defaults());
    let cost_tracker = Arc::new(tokio::sync::Mutex::new(multi_agent_model_gateway::SessionCostTracker::new()));

    let tiered_client = Arc::new(multi_agent_model_gateway::TieredRoutingLlmClient::new(
        model_selector,
        pricing_registry,
        cost_tracker,
    ));

    // =========================================================================
    // Initialize L1: Controller
    // =========================================================================
    let controller = Arc::new(
        ReActController::builder()
            .with_store(store.clone())
            .with_session_store(session_store.clone())
            .with_llm(tiered_client.clone() as Arc<dyn LlmClient>)
            .with_capability(Arc::new(
                multi_agent_controller::MemoryWritebackCapability::from_env(),
            ))
            .with_compressor(Arc::new(
                multi_agent_controller::context::TruncationCompressor::new(),
            ))
            .build(),
    );
    tracing::info!("L1 Controller initialized with dynamic Tiered LLM Routing");

    // =========================================================================
    // Initialize L0: Gateway
    // =========================================================================
    let approval_gate = Arc::new(multi_agent_governance::approval::ChannelApprovalGate::new(
        multi_agent_core::types::ToolRiskLevel::High,
    ));

    let routing_policy_store = Arc::new(
        match multi_agent_gateway::routing_policy::RoutingPolicyStore::new_persistent(
            ".sovereign_claw/routing/policies.json",
        ) {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to load persistent routing policy store; using in-memory store"
                );
                multi_agent_gateway::routing_policy::RoutingPolicyStore::new()
            }
        },
    );
    let router = Arc::new(
        DefaultRouter::new()
            .with_llm_classifier(tiered_client.clone() as Arc<dyn LlmClient>, tools.clone() as Arc<dyn ToolRegistry>)
            .with_routing_policy_store(routing_policy_store.clone()),
    );

    let cache = Arc::new(InMemorySemanticCache::new(tiered_client.clone() as Arc<dyn LlmClient>));

    let gateway_config = GatewayConfig {
        host: app_config.server.host.clone(),
        port: app_config.server.port,
        enable_cors: true,
        enable_tracing: true,
        allowed_origins: app_config.gateway.allowed_origins.clone(),
        tls: app_config.gateway.tls.clone(),
    };

    let (logs_tx, _logs_rx) = tokio::sync::broadcast::channel(100);

    let server = GatewayServer::new(gateway_config.clone(), router, cache)
        .with_controller(controller)
        .with_logs_channel(logs_tx.clone())
        .with_approval_gate(approval_gate.clone())
        .with_routing_policy_store(routing_policy_store.clone());

    tracing::info!(
        host = %gateway_config.host,
        port = gateway_config.port,
        "L0 Gateway initialized"
    );

    // =========================================================================
    // Print startup banner
    // =========================================================================
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!(
        "║                    OpenCoordex v{}                       ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Enterprise Open Multi-Agent Platform                         ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Endpoints:                                                   ║");
    println!("║    GET  /health      - Health check                          ║");
    println!("║    POST /v1/chat     - Chat with the agent                   ║");
    println!("║    POST /v1/intent   - Classify intent only                  ║");
    println!("║    POST /v1/research - Expert Research Agent (M10)           ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!(
        "║  Server: http://{}:{}                              ║",
        gateway_config.host, gateway_config.port
    );
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // Initialize L4: Observability (Metrics) & Governance
    // =========================================================================
    let metrics_handle = multi_agent_governance::setup_metrics_recorder()?;

    // Initialize Governance Components
    if let Some(parent) = std::path::Path::new(&app_config.governance.audit_log_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            multi_agent_core::Error::storage(format!(
                "Failed to create audit log directory '{}': {}",
                parent.display(),
                e
            ))
        })?;
    }
    let audit_store = Arc::new(multi_agent_governance::SqliteAuditStore::new(
        &app_config.governance.audit_log_path,
    )?);

    // RBAC: Check environment for production mode
    let is_production = app_config.governance.multiagent_env.to_lowercase() == "production";

    let rbac: Arc<dyn multi_agent_governance::RbacConnector> = if is_production {
        // Production: Require OIDC configuration
        let oidc_issuer = app_config.governance.oidc_issuer.as_ref()
            .expect("OIDC_ISSUER is required in production mode. Set governance.multiagent_env=development to disable.");
        tracing::info!(issuer = %oidc_issuer, "Initializing OIDC RBAC connector for production");
        Arc::new(multi_agent_governance::rbac::OidcRbacConnector::new(
            oidc_issuer,
        ))
    } else {
        tracing::warn!("Using NoOpRbacConnector - NOT SUITABLE FOR PRODUCTION");
        Arc::new(multi_agent_governance::NoOpRbacConnector)
    };

    // Initialize MCP Registry
    let mcp_registry = Arc::new(multi_agent_skills::McpRegistry::new());
    mcp_registry.register_defaults(); // Register built-in defaults

    // Initialize Redis components if configured
    let redis_url = app_config.store.redis_url.as_ref();

    let (provider_store, rate_limiter) = if let Some(url) = redis_url {
        tracing::info!("Initializing Redis backends at {}", url);

        let provider_store = match multi_agent_store::RedisProviderStore::new(url, "providers") {
            Ok(store) => Some(Arc::new(store) as Arc<dyn multi_agent_core::traits::ProviderStore>),
            Err(e) => {
                tracing::error!("Failed to initialize RedisProviderStore: {}", e);
                None
            }
        };

        let rate_limiter = match multi_agent_store::RedisRateLimiter::new(url) {
            Ok(limiter) => {
                Some(Arc::new(limiter) as Arc<dyn multi_agent_core::traits::DistributedRateLimiter>)
            }
            Err(e) => {
                tracing::error!("Failed to initialize RedisRateLimiter: {}", e);
                None
            }
        };

        (provider_store, rate_limiter)
    } else {
        tracing::info!("REDIS_URL not set - using in-memory stores");
        (None, None)
    };

    // Initialize Knowledge Store (M10.3)
    let knowledge_db_path = app_config
        .governance
        .audit_log_path
        .replace("audit.db", "knowledge.db");
    let knowledge_store_raw = Arc::new(SqliteKnowledgeStore::new(knowledge_db_path)?);
    let knowledge_store: Arc<dyn multi_agent_core::traits::KnowledgeStore> =
        knowledge_store_raw.clone();

    // Initialize Privacy Controller (M10.4)
    let erasable_stores: Vec<Arc<dyn multi_agent_core::traits::Erasable>> = vec![
        store_raw,
        session_store_raw,
        knowledge_store_raw.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
        audit_store.clone() as Arc<dyn multi_agent_core::traits::Erasable>,
    ];
    let privacy_controller = Arc::new(multi_agent_governance::PrivacyController::new(
        erasable_stores,
        Arc::new(multi_agent_core::traits::NoOpEventEmitter),
    ));

    let admin_state = Arc::new(multi_agent_admin::AdminState {
        audit_store,
        rbac,
        metrics: Some(metrics_handle.clone()),
        mcp_registry: mcp_registry.clone(),
        providers: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        provider_store,
        secrets: secrets_manager,
        artifact_store: Some(store.clone()),
        session_store: Some(session_store.clone()),
        privacy_controller: Some(privacy_controller),
        app_config: app_config.clone(),
        network_policy: network_policy.clone(),
        llm_client: Some(tiered_client.clone() as Arc<dyn LlmClient>),
        tool_registry: Some(tools.clone() as Arc<dyn ToolRegistry>),
    });

    // Initialize Research Orchestrator (M10.1, M10.5)
    let research_orchestrator = Arc::new(multi_agent_gateway::research::ResearchOrchestrator::new(
        admin_state.clone(),
        approval_gate.clone(),
        network_policy.clone(),
        None,
        app_config.safety.clone(),
        store.clone(),
        knowledge_store.clone(),
        Some(logs_tx.clone()),
    ));

    // =========================================================================
    // Start the server
    // =========================================================================
    let mut server = server
        .with_metrics(metrics_handle)
        .with_admin(admin_state)
        .with_research_orchestrator(research_orchestrator);

    if let Some(limiter) = rate_limiter {
        server = server.with_rate_limiter(limiter);
    }

    server.run().await?;

    Ok(())
}
