use anyhow::Result;
use secrecy::ExposeSecret;
use std::io;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

use multi_agent_controller::ReActController;
use multi_agent_core::traits::{ArtifactStore, SessionStore, ToolRegistry};
use multi_agent_core::types::ToolRiskLevel;
use multi_agent_gateway::research::ResearchOrchestrator;
use multi_agent_gateway::{DefaultRouter, GatewayConfig, GatewayServer, InMemorySemanticCache};
use multi_agent_governance::approval::ChannelApprovalGate;
use multi_agent_skills::{
    load_mcp_config, CalculatorTool, CompositeToolRegistry, DefaultToolRegistry, EchoTool,
    McpRegistry,
};
use multi_agent_store::{
    knowledge::InMemoryKnowledgeStore, InMemorySessionStore, InMemoryStore, RedisSessionStore,
    S3ArtifactStore, TieredStore,
};

/// A writer that broadcasts log lines to a channel.
struct ChannelWriter {
    tx: broadcast::Sender<String>,
}

impl io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = String::from_utf8_lossy(buf).to_string();
        // Ignore send errors - if no one is listening, we drop logs
        let _ = self.tx.send(s);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Event emitter that broadcasts to the logs channel.
struct ChannelEventEmitter {
    tx: broadcast::Sender<String>,
}

#[async_trait::async_trait]
impl multi_agent_core::traits::EventEmitter for ChannelEventEmitter {
    async fn emit(&self, event: multi_agent_core::events::EventEnvelope) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = self.tx.send(json);
        }
    }
}

/// Composite event emitter for multiple subscribers.
struct CompositeEventEmitter {
    emitters: Vec<Arc<dyn multi_agent_core::traits::EventEmitter>>,
}

#[async_trait::async_trait]
impl multi_agent_core::traits::EventEmitter for CompositeEventEmitter {
    async fn emit(&self, event: multi_agent_core::events::EventEnvelope) {
        for emitter in &self.emitters {
            emitter.emit(event.clone()).await;
        }
    }
}

#[allow(clippy::type_complexity)]
pub async fn start_server() -> Result<()> {
    // Load configuration
    let app_config = multi_agent_core::config::AppConfig::load()?;

    // =========================================================================
    // Initialize Tracing (Structure Logs + Broadcast)
    // =========================================================================

    // Create broadcast channel for logs (capacity 1000 lines)
    let (tx, _rx) = broadcast::channel(1000);
    let tx_for_logs = tx.clone();

    // Configure tracing to write JSON logs to the channel
    let make_writer = move || ChannelWriter {
        tx: tx_for_logs.clone(),
    };

    let rust_log = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "info,opencoordex=debug,multi_agent=debug".into());
    let env_filter = tracing_subscriber::EnvFilter::new(rust_log);

    if let Err(e) = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_writer(make_writer)
                .json()
                .flatten_event(true),
        )
        .try_init()
    {
        eprintln!("Tracing init warning: {}", e);
    }

    tracing::info!("Starting OpenCoordex Backend (Tauri Embedded)");

    // =========================================================================
    // Initialize L3: Artifact Store
    // =========================================================================
    use multi_agent_governance::privacy::PrivacyController;
    use multi_agent_store::retention::{Erasable, Prunable};

    // =========================================================================
    // Initialize L3: Artifact Store
    // =========================================================================
    let (store, artifacts_erasables, artifacts_prunables): (
        Arc<dyn ArtifactStore>,
        Vec<Arc<dyn Erasable>>,
        Vec<Arc<dyn Prunable>>,
    ) = if let Some(bucket) = &app_config.store.s3_bucket {
        let endpoint = app_config.store.s3_endpoint.as_deref();
        tracing::info!(bucket = %bucket, endpoint = ?endpoint, "Initializing S3 Artifact Store (Tiered)");

        let s3 = Arc::new(S3ArtifactStore::new(bucket, "", endpoint).await);
        let hot = Arc::new(InMemoryStore::new());
        let tiered = Arc::new(TieredStore::new(hot.clone()).with_cold(s3.clone()));

        let erasables: Vec<Arc<dyn Erasable>> = vec![s3.clone(), hot.clone()];
        let prunables: Vec<Arc<dyn Prunable>> = vec![s3, hot];

        (tiered, erasables, prunables)
    } else {
        tracing::info!("Initializing In-Memory Artifact Store");
        let mem = Arc::new(InMemoryStore::new());
        (mem.clone(), vec![mem.clone()], vec![mem])
    };

    // Initialize Session Store
    let (session_store, session_erasable, session_prunable): (
        Arc<dyn SessionStore>,
        Arc<dyn Erasable>,
        Arc<dyn Prunable>,
    ) = if let Some(redis_url) = &app_config.store.redis_url {
        tracing::info!(url = %redis_url, "Initializing Redis Session Store");
        let s = Arc::new(RedisSessionStore::new(
            redis_url,
            "opencoordex:session",
            3600 * 24,
        )?);
        (s.clone(), s.clone(), s)
    } else {
        tracing::info!("Initializing In-Memory Session Store");
        let s = Arc::new(InMemorySessionStore::new());
        (s.clone(), s.clone(), s)
    };

    // =========================================================================
    // Initialize L2: Skills & Tools
    // =========================================================================
    let local_registry = Arc::new(DefaultToolRegistry::new());

    local_registry.register(Box::new(EchoTool)).await?;
    local_registry.register(Box::new(CalculatorTool)).await?;

    // =========================================================================
    // Initialize Sandbox
    // =========================================================================
    let sandbox_manager = match multi_agent_sandbox::DockerSandbox::new() {
        Ok(engine) => {
            let engine = std::sync::Arc::new(engine);
            let config = multi_agent_sandbox::SandboxConfig::default();
            let manager = Arc::new(multi_agent_sandbox::SandboxManager::new(engine, config));

            local_registry
                .register(Box::new(multi_agent_sandbox::SandboxShellTool::new(
                    manager.clone(),
                )))
                .await?;
            local_registry
                .register(Box::new(multi_agent_sandbox::SandboxWriteFileTool::new(
                    manager.clone(),
                )))
                .await?;
            local_registry
                .register(Box::new(multi_agent_sandbox::SandboxReadFileTool::new(
                    manager.clone(),
                )))
                .await?;
            local_registry
                .register(Box::new(multi_agent_sandbox::SandboxListFilesTool::new(
                    manager.clone(),
                )))
                .await?;

            tracing::info!("🐳 Sovereign Sandbox initialized");
            Some(manager)
        }
        Err(e) => {
            tracing::warn!("Docker not available ({}). Sandbox tools disabled.", e);
            None
        }
    };

    // =========================================================================
    // Initialize Airlock Networking
    // =========================================================================
    let policy_path = ".sovereign_claw/policies/network_policy.yaml";
    let network_policy = if std::path::Path::new(policy_path).exists() {
        match std::fs::read_to_string(policy_path) {
            Ok(content) => match serde_yaml::from_str::<
                multi_agent_governance::network::NetworkPolicy,
            >(&content)
            {
                Ok(policy) => {
                    tracing::info!("Loaded network policy from {}", policy_path);
                    Arc::new(tokio::sync::RwLock::new(policy))
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to parse network policy: {}. Using Deny-All default.",
                        e
                    );
                    Arc::new(tokio::sync::RwLock::new(
                        multi_agent_governance::network::NetworkPolicy::default(),
                    ))
                }
            },
            Err(e) => {
                tracing::error!(
                    "Failed to read network policy: {}. Using Deny-All default.",
                    e
                );
                Arc::new(tokio::sync::RwLock::new(
                    multi_agent_governance::network::NetworkPolicy::default(),
                ))
            }
        }
    } else {
        tracing::info!(
            "No network policy found at {}. Using Deny-All default.",
            policy_path
        );
        // Create default file if it doesn't exist
        if let Some(parent) = std::path::Path::new(policy_path).parent() {
            let _ = std::fs::create_dir_all(parent);
            let default_policy = multi_agent_governance::network::NetworkPolicy::default();
            if let Ok(yaml) = serde_yaml::to_string(&default_policy) {
                let _ = std::fs::write(policy_path, yaml);
            }
        }
        Arc::new(tokio::sync::RwLock::new(
            multi_agent_governance::network::NetworkPolicy::default(),
        ))
    };

    local_registry
        .register(Box::new(multi_agent_skills::network::FetchTool::new(
            network_policy.clone(),
            app_config.safety.clone(),
        )))
        .await?;

    if let Some(manager) = sandbox_manager {
        local_registry
            .register(Box::new(multi_agent_skills::network::DownloadTool::new(
                network_policy.clone(),
                app_config.safety.clone(),
                manager,
            )))
            .await?;
    } else {
        tracing::warn!("Sandbox not available. DownloadTool disabled.");
    }

    // Initialize MCP Registry
    let mcp_registry = Arc::new(McpRegistry::new());

    // Load MCP config
    let config_path = std::path::Path::new("mcp_config.toml");
    if config_path.exists() {
        tracing::info!("Loading MCP config from {:?}", config_path);
        if let Err(e) = load_mcp_config(mcp_registry.clone(), config_path).await {
            tracing::warn!("Failed to load MCP config: {}", e);
        }
    } else if let Some(mut home) = dirs::home_dir() {
        home.push(".sovereign_claw");
        home.push("mcp_config.toml");
        if home.exists() {
            tracing::info!("Loading MCP config from {:?}", home);
            if let Err(e) = load_mcp_config(mcp_registry.clone(), &home).await {
                tracing::warn!("Failed to load MCP config: {}", e);
            }
        }
    }

    // Initialize Governance / Admin State
    let audit_log_path = app_config.governance.audit_log_path.clone();
    if let Some(parent) = std::path::Path::new(&audit_log_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let audit_store = Arc::new(multi_agent_governance::SqliteAuditStore::new(
        &audit_log_path,
    )?);

    // Onboarding Keys
    if let Some(key) = &app_config.model_gateway.openai_api_key {
        std::env::set_var("OPENAI_API_KEY", key.expose_secret());
    }
    if let Some(key) = &app_config.model_gateway.anthropic_api_key {
        std::env::set_var("ANTHROPIC_API_KEY", key.expose_secret());
    }

    let admin_token = match &app_config.governance.admin_token {
        Some(t) => t.expose_secret().to_string(),
        None => {
            let token = uuid::Uuid::new_v4().to_string();
            tracing::info!("*************************************************");
            tracing::info!("ADMIN_TOKEN NOT SET. GENERATED RANDOM TOKEN:");
            tracing::info!("  {}", token);
            tracing::info!("*************************************************");
            token
        }
    };
    let rbac = Arc::new(multi_agent_governance::StaticTokenRbacConnector::new(
        admin_token,
    ));
    let secrets = Arc::new(multi_agent_governance::AesGcmSecretsManager::new(None));

    let provider_store = Arc::new(multi_agent_store::FileProviderStore::new(
        ".sovereign_claw/providers.json",
    ));

    // Policy Engine Initialization
    let policy_dir = ".sovereign_claw/policies";
    let default_policy_path = format!("{}/default.yaml", policy_dir);
    if !std::path::Path::new(&default_policy_path).exists() {
        let _ = std::fs::create_dir_all(policy_dir);
        let default_policy = r#"version: "1.0"
name: "Default Security Policy"
rules:
  - id: "block-rm-rf"
    description: "Block recursive force delete"
    match_rule:
      tool_glob: "sandbox_*"
      args_contain: ["rm -rf", "rm -r"]
    action:
      risk: critical
      reason: "Destructive filesystem operation detected"
  - id: "high-risk-fs"
    description: "Elevate risk for sensitive FS operations"
    match_rule:
      tool_glob: "fs_write*"
    action:
      risk: high
      reason: "Filesystem write detected"
thresholds:
  low: 10
  medium: 30
  high: 60
  critical: 90
  approval_required: 50
"#;
        let _ = std::fs::write(&default_policy_path, default_policy);
    }

    let policy_engine = match multi_agent_governance::PolicyEngine::load(&default_policy_path) {
        Ok(engine) => Arc::new(tokio::sync::RwLock::new(engine)),
        Err(e) => {
            tracing::error!("Failed to load policy engine: {}. Using empty policy.", e);
            Arc::new(tokio::sync::RwLock::new(
                multi_agent_governance::PolicyEngine::from_file(
                    multi_agent_governance::PolicyFile {
                        version: "0.0.0".into(),
                        name: "Empty Backup Policy".into(),
                        rules: vec![],
                        thresholds: multi_agent_governance::policy::PolicyThresholds::default(),
                    },
                ),
            ))
        }
    };

    let mut all_erasables = artifacts_erasables;
    all_erasables.push(session_erasable);

    // We haven't created event_emitter yet (it's created later at line 316).
    // We need event_emitter BEFORE creating PrivacyController?
    // PrivacyController needs event_emitter.
    // So create event_emitter earlier or reorder.

    // Reorder:
    // 1. Logs channel writer (done)
    // 2. Audit persistence (done earlier)
    // 3. Composite Event Emitter

    // Hash-chained Audit Log (Moving up)
    let audit_log_path_secure = app_config.governance.audit_log_storage_path.clone();
    if let Some(parent) = std::path::Path::new(&audit_log_path_secure).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let audit_subscriber = Arc::new(crate::audit_log::AuditSubscriber::new(
        &audit_log_path_secure,
    )?);
    let ws_emitter = Arc::new(ChannelEventEmitter { tx: tx.clone() });

    let event_emitter = Arc::new(CompositeEventEmitter {
        emitters: vec![ws_emitter.clone(), audit_subscriber.clone()],
    });

    let privacy_controller = Arc::new(PrivacyController::new(all_erasables, event_emitter.clone()));

    let admin_state = Arc::new(multi_agent_admin::AdminState {
        audit_store,
        rbac,
        metrics: None, // metrics recorder handles this globally
        mcp_registry: mcp_registry.clone(),
        providers: Arc::new(tokio::sync::RwLock::new(vec![])),
        provider_store: Some(provider_store),
        secrets,
        privacy_controller: Some(privacy_controller),
        artifact_store: Some(store.clone()),
        session_store: Some(session_store.clone()),
        app_config: app_config.clone(),
        network_policy: network_policy.clone(),
        llm_client: None,
        tool_registry: None,
    });

    // Composite Registry
    let mut composite_tools = CompositeToolRegistry::new();
    composite_tools.add_registry(local_registry.clone());
    composite_tools.add_registry(mcp_registry.clone());
    let tools = Arc::new(composite_tools);

    // Initialize Plugin Manager
    let plugins_dir = if let Some(mut home) = dirs::home_dir() {
        home.push(".sovereign_claw");
        home.push("plugins");
        home
    } else {
        std::path::PathBuf::from("plugins")
    };
    let state_file = plugins_dir.join("state.json");
    let plugin_manager = Arc::new(
        multi_agent_ecosystem::PluginManager::new(plugins_dir, state_file)
            .with_event_emitter(event_emitter.clone()),
    );
    if let Err(e) = plugin_manager.initialize().await {
        tracing::warn!("Failed to initialize plugin manager: {}", e);
    }
    // Sync initially
    plugin_manager.sync_registry(&mcp_registry).await;

    let controller = Arc::new(
        ReActController::builder()
            .with_store(store.clone())
            .with_session_store(session_store.clone())
            .with_tools(tools.clone())
            .with_event_emitter(event_emitter)
            .with_policy_engine(policy_engine.clone())
            .build(),
    );

    // =========================================================================
    // Initialize L0: Gateway
    // =========================================================================
    let router = Arc::new(DefaultRouter::new());

    // Onboarding Check
    let onboarding_completed = app_config.model_gateway.openai_api_key.is_some()
        || app_config.model_gateway.anthropic_api_key.is_some();

    if !onboarding_completed {
        tracing::warn!("⚠️  ONBOARDING REQUIRED: No LLM API keys found in config");
    }

    use multi_agent_core::traits::LlmClient;
    let llm_client: Arc<dyn LlmClient> = match multi_agent_model_gateway::create_default_client() {
        Ok(client) => Arc::new(client),
        Err(e) => {
            tracing::warn!(
                "Failed to create default LLM client: {}. Cache fallback.",
                e
            );
            Arc::new(multi_agent_model_gateway::MockLlmClient::new("dummy"))
        }
    };

    let cache = Arc::new(InMemorySemanticCache::new(llm_client));

    // Secure Defaults: CORS
    let allowed_origins = app_config.gateway.allowed_origins.clone();
    if !cfg!(debug_assertions) && allowed_origins.contains(&"*".to_string()) {
        tracing::error!("FATAL: Wildcard CORS ('*') is forbidden in production builds.");
        return Err(anyhow::anyhow!(
            "Secure Default Violation: Wildcard CORS in production"
        ));
    }

    let gateway_config = GatewayConfig {
        host: app_config.server.host.clone(),
        port: app_config.server.port,
        enable_cors: true,
        enable_tracing: true,
        allowed_origins,
        tls: app_config.gateway.tls.clone(),
    };

    // =========================================================================
    // Initialize Research P0 Components
    // =========================================================================
    let approval_gate = Arc::new(ChannelApprovalGate::new(ToolRiskLevel::Medium));
    let knowledge_store = Arc::new(InMemoryKnowledgeStore::new());

    let research_orchestrator = Arc::new(ResearchOrchestrator::new(
        admin_state.clone(),
        approval_gate.clone(),
        network_policy.clone(),
        Some(policy_engine.clone()),
        app_config.safety.clone(),
        store.clone(),
        knowledge_store.clone(),
        Some(tx.clone()),
    ));

    let server = GatewayServer::new(gateway_config.clone(), router, cache)
        .with_controller(controller)
        .with_admin(admin_state)
        .with_plugin_manager(plugin_manager)
        .with_logs_channel(tx)
        .with_policy_engine(policy_engine)
        .with_approval_gate(approval_gate)
        .with_research_orchestrator(research_orchestrator);

    tracing::info!(
        host = %app_config.server.host,
        port = app_config.server.port,
        "Gateway server initialized"
    );

    println!(
        "✓ OpenCoordex Gateway running on http://{}:{}",
        app_config.server.host, app_config.server.port
    );

    let app = server.build_router();
    let listener = tokio::net::TcpListener::bind(format!(
        "{}:{}",
        app_config.server.host, app_config.server.port
    ))
    .await?;

    tracing::info!("Server listening on {}", listener.local_addr()?);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    // =========================================================================
    // Start Background Retention Pruning
    // =========================================================================
    // use multi_agent_store::retention::Prunable; // Removed duplicate import

    // We need to collect prunables.
    // We have `all_erasables`. Assuming objects implementing Erasable also implement Prunable?
    // No. `S3ArtifactStore`, `InMemoryStore`, `RedisSessionStore` implement both.
    // But `all_erasables` is `Vec<Arc<dyn Erasable>>`. We can't cast back to `Prunable`.
    // We need to collect them separately or cast from original Arc.

    // Simplification: We rely on `artifacts_erasables` and `session_erasable` logic
    // but we need to re-collect as `Prunable`.
    // This is getting verbose in `backend.rs`.
    // For MVP, let's just create a new vector here if we can access the original variables.
    // Variables `s3`, `hot`, `mem`, `s` are inside if/else scopes.
    // We returned them as `Erasable`.

    // Refactoring step 3690 returned `(tiered, erasables)`.
    // We should return `(tiered, erasables, prunables)`.
    // OR just return the concrete types? No, type erasure.

    // Let's assume for now we only support pruning if we refactor initialization.
    // But I don't want to refactor everything again.

    // Workaround:
    // We can't easily get `Prunable` from `Erasable`.
    // But we can implement `Prunable` for `Erasable` wrapper? No.

    // I will refactor the initialization blocks to return `prunables` as well.
    // It is messy but necessary.

    // Actually, I can just use a `BackgroundWorker` or `LifecycleManager`?

    // Let's postpone generic pruning for a moment and look at what I have.
    // `session_erasable` is `Arc<dyn Erasable>`.
    // `artifacts_erasables` is `Vec<Arc<dyn Erasable>>`.

    // If I change the return type of the init blocks to `(..., Vec<Arc<dyn Erasable>>, Vec<Arc<dyn Prunable>>)`.

    // Let's do that.

    let mut all_prunables = artifacts_prunables;
    all_prunables.push(session_prunable);

    tokio::spawn(async move {
        // Default retention: 30 days
        let retention_period = std::time::Duration::from_secs(30 * 24 * 3600);
        // Check every hour
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));

        loop {
            interval.tick().await;
            tracing::info!("Starting background retention pruning...");
            for p in &all_prunables {
                if let Err(e) = p.prune(retention_period).await {
                    tracing::error!("Pruning failed: {}", e);
                }
            }
        }
    });

    Ok(())
}
