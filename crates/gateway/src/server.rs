//! Axum-based HTTP server for the gateway.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Json, Path, Query, State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::idempotency::{IdempotencyLookup, IdempotencyStore};
use crate::routing_policy::{
    RoutingContext, RoutingPolicyChannel, RoutingPolicyEngine, RoutingPolicyRelease,
    RoutingPolicyStore, RoutingRule,
};
use crate::scheduler::ControllerScheduler;
use multi_agent_core::{
    config::TlsConfig,
    traits::{Controller, IntentRouter, SemanticCache},
    types::{
        AgentResult, ApiEnvelope, ApiErrorBody, ApiErrorCode, ApprovalResponse, NormalizedRequest,
        RequestContent, RequestMetadata, UserIntent, GATEWAY_CONTRACT_VERSION,
    },
    Result,
};
use multi_agent_governance::approval::ChannelApprovalGate;
use multi_agent_governance::{AuditEntry, AuditFilter, AuditOutcome};

/// Gateway configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Host to bind to.
    pub host: String,
    /// Port to bind to.
    pub port: u16,
    /// Enable CORS.
    pub enable_cors: bool,
    /// Enable request tracing.
    pub enable_tracing: bool,
    /// Allowed CORS origins.
    pub allowed_origins: Vec<String>,
    /// TLS Configuration.
    pub tls: TlsConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            enable_cors: true,
            enable_tracing: true,
            allowed_origins: vec![
                "http://localhost:3000".to_string(),
                "http://127.0.0.1:3000".to_string(),
            ],
            tls: TlsConfig {
                enabled: false,
                cert_path: None,
                key_path: None,
                ca_path: None,
            },
        }
    }
}

/// Shared application state.
pub struct AppState {
    /// Intent router.
    pub router: Arc<dyn IntentRouter>,
    /// Semantic cache.
    pub cache: Arc<dyn SemanticCache>,
    /// Controller (optional for Phase 1).
    pub controller: Option<Arc<dyn Controller>>,
    /// Optional distributed rate limiter.
    pub rate_limiter: Option<Arc<dyn DistributedRateLimiter>>,
    /// Approval gate for HITL flow (optional).
    /// Approval gate for HITL flow (optional).
    pub approval_gate: Option<Arc<ChannelApprovalGate>>,
    /// Logs broadcast channel for "Fog of War" UI.
    pub logs_channel: Option<tokio::sync::broadcast::Sender<String>>,
    /// Policy engine for rule-based risk assessment.
    pub policy_engine: Option<Arc<tokio::sync::RwLock<multi_agent_governance::PolicyEngine>>>,
    /// Admin state for configuration persistence.
    pub admin_state: Option<Arc<multi_agent_admin::AdminState>>,
    /// Plugin manager for dynamic tool loading.
    pub plugin_manager: Option<Arc<multi_agent_ecosystem::PluginManager>>,
    /// Application configuration.
    pub app_config: multi_agent_core::config::AppConfig,
    /// Research orchestrator for P0 workflow.
    pub research_orchestrator: Option<Arc<crate::research::ResearchOrchestrator>>,
    /// Idempotency store for side-effect endpoints.
    pub idempotency_store: Arc<IdempotencyStore>,
    /// Scheduler for controller execution lanes.
    pub controller_scheduler: Arc<ControllerScheduler>,
    /// Shared versioned routing policy store.
    pub routing_policy_store: Option<Arc<RoutingPolicyStore>>,
    /// Tiered routing LLM client.
    pub tiered_client: Option<Arc<multi_agent_model_gateway::TieredRoutingLlmClient>>,
}

impl AppState {
    /// Emit a structured event to the logs channel.
    pub fn emit_event(&self, envelope: multi_agent_core::events::EventEnvelope) {
        if let Some(tx) = &self.logs_channel {
            // Serialize to JSON and broadcast
            if let Ok(json) = serde_json::to_string(&envelope) {
                // Ignore send errors (no listeners)
                let _ = tx.send(json);
            }
        }
    }
}

use metrics_exporter_prometheus::PrometheusHandle;
use multi_agent_core::traits::DistributedRateLimiter;

/// Gateway server.
pub struct GatewayServer {
    config: GatewayConfig,
    state: Arc<AppState>,
    metrics_handle: Option<PrometheusHandle>,
    admin_state: Option<Arc<multi_agent_admin::AdminState>>,
}

impl GatewayServer {
    /// Create a new gateway server.
    pub fn new(
        config: GatewayConfig,
        router: Arc<dyn IntentRouter>,
        cache: Arc<dyn SemanticCache>,
    ) -> Self {
        Self {
            config,
            state: Arc::new(AppState {
                router,
                cache,
                controller: None,
                rate_limiter: None,
                approval_gate: None,
                logs_channel: None,
                policy_engine: None,
                admin_state: None,
                plugin_manager: None,
                app_config: multi_agent_core::config::AppConfig::load().unwrap_or_default(),
                research_orchestrator: None,
                idempotency_store: Arc::new(IdempotencyStore::new()),
                controller_scheduler: Arc::new(ControllerScheduler::default()),
                routing_policy_store: None,
                tiered_client: None,
            }),
            metrics_handle: None,
            admin_state: None,
        }
    }

    /// Set the policy engine.
    pub fn with_policy_engine(
        mut self,
        engine: Arc<tokio::sync::RwLock<multi_agent_governance::PolicyEngine>>,
    ) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.policy_engine = Some(engine);
        }
        self
    }

    /// Set the controller.
    pub fn with_controller(mut self, controller: Arc<dyn Controller>) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.controller = Some(controller);
        }
        self
    }

    /// Set the plugin manager.
    pub fn with_plugin_manager(
        mut self,
        manager: Arc<multi_agent_ecosystem::PluginManager>,
    ) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.plugin_manager = Some(manager);
        }
        self
    }

    /// Set metrics handle.
    pub fn with_metrics(mut self, handle: PrometheusHandle) -> Self {
        self.metrics_handle = Some(handle);
        self
    }

    /// Set admin state.
    pub fn with_admin(mut self, state: Arc<multi_agent_admin::AdminState>) -> Self {
        self.admin_state = Some(state.clone());
        if let Some(s) = Arc::get_mut(&mut self.state) {
            s.app_config = state.app_config.clone();
            s.admin_state = Some(state);
        }
        self
    }

    /// Set distributed rate limiter (e.g., Redis-backed).
    pub fn with_rate_limiter(mut self, limiter: Arc<dyn DistributedRateLimiter>) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.rate_limiter = Some(limiter);
        }
        self
    }

    /// Set the approval gate for HITL flow.
    pub fn with_approval_gate(mut self, gate: Arc<ChannelApprovalGate>) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.approval_gate = Some(gate);
        }
        self
    }

    /// Set the research orchestrator.
    pub fn with_research_orchestrator(
        mut self,
        orchestrator: Arc<crate::research::ResearchOrchestrator>,
    ) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.research_orchestrator = Some(orchestrator);
        }
        self
    }

    /// Set the logs broadcast channel.
    pub fn with_logs_channel(mut self, sender: tokio::sync::broadcast::Sender<String>) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.logs_channel = Some(sender);
        }
        self
    }

    /// Set shared versioned routing policy store.
    pub fn with_routing_policy_store(mut self, store: Arc<RoutingPolicyStore>) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.routing_policy_store = Some(store);
        }
        self
    }

    /// Set tiered routing LLM client.
    pub fn with_tiered_client(
        mut self,
        client: Arc<multi_agent_model_gateway::TieredRoutingLlmClient>,
    ) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.state) {
            state.tiered_client = Some(client);
        }
        self
    }

    /// Build the Axum router.
    pub fn build_router(&self) -> Router {
        // System Routes
        let metrics_handle = self.metrics_handle.clone();
        let system_router = Router::new()
            .route("/health", get(health_handler))
            .route("/healthz", get(healthz_handler))
            .route("/readyz", get(readyz_handler))
            .route("/schema/gateway", get(gateway_schema_handler))
            .route(
                "/metrics",
                get(move || {
                    let handle = metrics_handle.clone();
                    async move {
                        if let Some(h) = handle {
                            h.render()
                        } else {
                            "Metrics not enabled".to_string()
                        }
                    }
                }),
            );

        // Agent Routes
        let agent_router = Router::new()
            .route("/chat", post(chat_handler))
            .route("/intent", post(intent_handler))
            .route("/webhook/:event_type", post(webhook_handler))
            .route("/ws/approval", get(approval_ws_handler))
            .route("/ws/logs", get(logs_ws_handler))
            .route("/approve/:request_id", post(approve_rest_handler))
            .route("/onboarding/status", get(onboarding_status_handler))
            .route("/onboarding/setup", post(onboarding_setup_handler))
            .route("/research", post(research_handler))
            .route("/policy", get(get_policy_handler).put(put_policy_handler))
            .route("/plugins", get(get_plugins_handler))
            .route("/plugins/{plugin_id}", get(get_plugin_details_handler))
            .route("/plugins/{plugin_id}/toggle", post(toggle_plugin_handler))
            .layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                bearer_auth_middleware,
            ));

        let mut router = Router::new()
            // Consolidated v1 namespace
            .nest("/v1/system", system_router)
            .nest("/v1/agent", agent_router)
            // Backward compatibility
            .route("/health", get(health_handler))
            .route("/v1/chat", post(chat_handler))
            .route("/v1/intent", post(intent_handler))
            .route("/v1/webhook/:event_type", post(webhook_handler))
            .route(
                "/v1/chat/completions",
                post(openai_chat_completions_handler).route_layer(
                    axum::middleware::from_fn_with_state(
                        self.state.clone(),
                        bearer_auth_middleware,
                    ),
                ),
            )
            .route(
                "/v1/approve/:request_id",
                post(approve_rest_handler).route_layer(axum::middleware::from_fn_with_state(
                    self.state.clone(),
                    bearer_auth_middleware,
                )),
            )
            .with_state(self.state.clone());

        // Admin API
        if let Some(admin_state) = &self.admin_state {
            let admin_api = multi_agent_admin::admin_api_router(admin_state.clone())
                .route_layer(axum::middleware::from_fn_with_state(
                    self.state.clone(),
                    restrict_to_localhost,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    self.state.clone(),
                    bearer_auth_middleware,
                ));
            router = router.nest("/v1/admin", admin_api);

            let routing_admin_api = Router::new()
                .route("/simulate", post(admin_routing_simulate_handler))
                .route("/publish", post(admin_routing_publish_handler))
                .route("/promote", post(admin_routing_promote_handler))
                .route("/rollback", post(admin_routing_rollback_handler))
                .route("/audits", get(admin_routing_audits_handler))
                .route("/policies", get(admin_routing_policies_handler))
                .route_layer(axum::middleware::from_fn_with_state(
                    self.state.clone(),
                    restrict_to_localhost,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    self.state.clone(),
                    bearer_auth_middleware,
                ))
                .with_state(self.state.clone());
            router = router.nest("/v1/admin/routing", routing_admin_api);

            // Management Console (Static assets)
            router = router.nest("/console", multi_agent_admin::admin_static_router());
            // Also serve the console from the site root and with a trailing slash,
            // so relative asset links resolve in every entry way.
            router = router
                .route("/", get(multi_agent_admin::dashboard_index))
                .route("/console/", get(multi_agent_admin::dashboard_index));
        }

        // Apply rate limiting: Distributed (Redis) or Local (Governor)
        if self.state.rate_limiter.is_some() {
            tracing::info!("Using Distributed Rate Limiter (Redis)");
            router = router.layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                distributed_rate_limit,
            ));
        } else {
            tracing::info!("Using Local Rate Limiter (Tower Governor)");
            use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

            // Rate limit: ~120 requests per minute per IP
            let governor_conf = GovernorConfigBuilder::default()
                .per_second(2) // ~120/min
                .burst_size(30) // Allow bursts
                .finish()
                .expect("Failed to build rate limiter config");

            let governor_limiter = GovernorLayer {
                config: std::sync::Arc::new(governor_conf),
            };
            router = router.layer(governor_limiter);
        }

        if self.config.enable_cors {
            // CORS: Use configured allowed origins
            if self.config.allowed_origins.iter().any(|o| o == "*") {
                tracing::warn!("CORS: Wildcard allowed_origins set. Allowing ALL origins.");
                router = router.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any));
            } else {
                use axum::http::HeaderValue;
                let origins: Vec<HeaderValue> = self
                    .config
                    .allowed_origins
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect();

                if !origins.is_empty() {
                    router =
                        router.layer(CorsLayer::new().allow_origin(origins).allow_methods(Any));
                } else {
                    tracing::warn!("CORS: Enabled but no valid origins provided. Blocking all.");
                }
            }
        }

        if self.config.enable_tracing {
            router = router.layer(TraceLayer::new_for_http());
        }

        router
    }

    /// Run the server.
    pub async fn run(self) -> Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        if self.config.tls.enabled {
            use axum_server::tls_rustls::RustlsConfig;

            let cert_path = self.config.tls.cert_path.as_ref().ok_or_else(|| {
                multi_agent_core::Error::gateway("TLS enabled but cert_path missing")
            })?;
            let key_path = self.config.tls.key_path.as_ref().ok_or_else(|| {
                multi_agent_core::Error::gateway("TLS enabled but key_path missing")
            })?;

            tracing::info!(addr = %addr, "Gateway server starting (TLS ENABLED)");

            let config = RustlsConfig::from_pem_file(cert_path, key_path)
                .await
                .map_err(|e| {
                    multi_agent_core::Error::gateway(format!("TLS config error: {}", e))
                })?;

            axum_server::bind_rustls(addr.parse::<std::net::SocketAddr>().unwrap(), config)
                .serve(
                    self.build_router()
                        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .map_err(|e| {
                    multi_agent_core::Error::gateway(format!("TLS Server error: {}", e))
                })?;
        } else {
            tracing::info!(addr = %addr, "Gateway server starting (PLAIN HTTP)");
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .map_err(|e| multi_agent_core::Error::gateway(format!("Failed to bind: {}", e)))?;

            axum::serve(
                listener,
                self.build_router()
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .map_err(|e| multi_agent_core::Error::gateway(format!("Server error: {}", e)))?;
        }

        Ok(())
    }
}

// =============================================================================
// Request/Response Types
// =============================================================================

/// Chat request.
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// Message content.
    pub message: String,
    /// Optional session ID.
    pub session_id: Option<String>,
    /// Optional user ID.
    pub user_id: Option<String>,
    /// Optional workspace ID for isolation.
    pub workspace_id: Option<String>,
}

/// Chat response.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    /// Trace ID for this request.
    pub trace_id: String,
    /// Classified intent.
    pub intent: UserIntent,
    /// Result (if controller is available).
    pub result: Option<AgentResult>,
    /// Whether the response was from cache.
    pub cached: bool,
}

/// Intent-only request.
#[derive(Debug, Deserialize)]
pub struct IntentRequest {
    /// Message to classify.
    pub message: String,
}

/// Research request.
#[derive(Debug, Deserialize)]
pub struct ResearchRequest {
    /// Research query.
    pub query: String,
    /// User ID (optional, normally from JWT).
    pub user_id: Option<String>,
}

/// Intent response.
#[derive(Debug, Serialize)]
pub struct IntentResponse {
    /// Trace ID.
    pub trace_id: String,
    /// Classified intent.
    pub intent: UserIntent,
}

/// Health response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Status.
    pub status: String,
    /// Version.
    pub version: String,
}

/// Error response.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Error code.
    pub code: String,
    /// Error message.
    pub message: String,
    /// Trace ID.
    pub trace_id: Option<String>,
}

// =============================================================================
// Handlers
// =============================================================================

/// Health check handler.
async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn gateway_schema_handler() -> impl IntoResponse {
    let schema = schema_for!(ApiEnvelope<serde_json::Value>);
    Json(serde_json::json!({
        "name": "gateway_contract",
        "version": GATEWAY_CONTRACT_VERSION,
        "schema": schema,
    }))
}

/// Liveness check handler (k8s style).
async fn healthz_handler() -> impl IntoResponse {
    StatusCode::OK
}

/// Readiness check handler (k8s style).
async fn readyz_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut errors = Vec::new();

    // Check Artifact Store
    if let Some(admin) = &state.admin_state {
        if let Some(store) = &admin.artifact_store {
            if let Err(e) = store.health_check().await {
                errors.push(format!("ArtifactStore: {}", e));
            }
        }

        // Check Session Store
        if let Some(store) = &admin.session_store {
            if let Err(e) = store.health_check().await {
                errors.push(format!("SessionStore: {}", e));
            }
        }
    }

    if errors.is_empty() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "unready",
                "errors": errors
            })),
        )
            .into_response()
    }
}

/// Onboarding status response.
#[derive(Debug, Serialize)]
pub struct OnboardingStatus {
    pub openai_key_set: bool,
    pub anthropic_key_set: bool,
    pub onboarding_completed: bool,
}

/// Onboarding setup request.
#[derive(Debug, Deserialize)]
pub struct OnboardingSetup {
    pub openai_key: Option<String>,
    pub anthropic_key: Option<String>,
}

async fn onboarding_status_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sm = match &state.admin_state {
        Some(s) => &s.secrets,
        None => {
            return Json(OnboardingStatus {
                openai_key_set: false,
                anthropic_key_set: false,
                onboarding_completed: false,
            })
        }
    };

    let openai_key_set = sm
        .retrieve("openai_api_key")
        .await
        .unwrap_or(None)
        .is_some()
        || state.app_config.model_gateway.openai_api_key.is_some();

    let anthropic_key_set = sm
        .retrieve("anthropic_api_key")
        .await
        .unwrap_or(None)
        .is_some()
        || state.app_config.model_gateway.anthropic_api_key.is_some();

    let onboarding_completed = openai_key_set || anthropic_key_set;

    Json(OnboardingStatus {
        openai_key_set,
        anthropic_key_set,
        onboarding_completed,
    })
}

async fn onboarding_setup_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OnboardingSetup>,
) -> impl IntoResponse {
    let sm = match &state.admin_state {
        Some(s) => &s.secrets,
        None => return StatusCode::SERVICE_UNAVAILABLE,
    };

    if let Some(key) = payload.openai_key {
        if !key.is_empty() {
            if let Err(e) = sm.store("openai_api_key", &key).await {
                tracing::error!("Failed to store OpenAI key: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
    }

    if let Some(key) = payload.anthropic_key {
        if !key.is_empty() {
            if let Err(e) = sm.store("anthropic_api_key", &key).await {
                tracing::error!("Failed to store Anthropic key: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
    }

    StatusCode::OK
}

async fn get_policy_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match &state.policy_engine {
        Some(engine) => {
            let engine = engine.read().await;
            (StatusCode::OK, Json(engine.policy.clone())).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Policy engine not configured"})),
        )
            .into_response(),
    }
}

async fn put_policy_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<multi_agent_governance::PolicyFile>,
) -> impl IntoResponse {
    match &state.policy_engine {
        Some(engine) => {
            let mut engine = engine.write().await;

            // Persist to disk
            let policy_path = std::path::Path::new(".sovereign_claw/policies/default.yaml");
            if let Some(parent) = policy_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(content) = serde_yaml::to_string(&payload) {
                if let Err(e) = std::fs::write(policy_path, content) {
                    tracing::error!("Failed to persist policy: {}", e);
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }

            engine.policy = payload;
            StatusCode::OK.into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Policy engine not configured"})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct RoutingPublishRequest {
    pub version: String,
    pub name: Option<String>,
    pub channel: Option<RoutingPolicyChannel>,
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Deserialize)]
pub struct RoutingSimulateRequest {
    pub scenarios: Vec<RoutingContext>,
    pub rules: Option<Vec<RoutingRule>>,
}

#[derive(Debug, Deserialize)]
pub struct RoutingRollbackRequest {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct RoutingPromoteRequest {
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoutingAuditQuery {
    pub action: Option<String>,
    pub version: Option<String>,
    pub channel: Option<String>,
    pub limit: Option<usize>,
}

async fn log_routing_admin_audit(
    state: &Arc<AppState>,
    action: &str,
    metadata: serde_json::Value,
    outcome: AuditOutcome,
) {
    let Some(admin_state) = &state.admin_state else {
        return;
    };
    let _ = admin_state
        .audit_store
        .log(AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: action.to_string(),
            resource: "routing_policy".to_string(),
            outcome,
            metadata: Some(metadata),
            previous_hash: None,
            hash: None,
        })
        .await;
}

async fn admin_routing_publish_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RoutingPublishRequest>,
) -> impl IntoResponse {
    let Some(store) = &state.routing_policy_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Routing policy store not configured"})),
        )
            .into_response();
    };

    let publish_version = payload.version.clone();
    let publish_channel = payload.channel.unwrap_or(RoutingPolicyChannel::Canary);
    let release = RoutingPolicyRelease {
        version: publish_version.clone(),
        name: payload.name,
        published_at: chrono::Utc::now().timestamp(),
        channel: publish_channel,
        rules: payload.rules,
    };

    match store.publish(release).await {
        Ok(()) => {
            state.emit_event(
                multi_agent_core::events::EventEnvelope::new(
                    multi_agent_core::events::EventType::AuditAppended,
                    serde_json::json!({
                        "action": "ROUTING_POLICY_PUBLISH",
                        "active": store.active_release().await
                    }),
                )
                .with_actor("admin"),
            );
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_PUBLISH",
                serde_json::json!({
                    "version": publish_version,
                    "channel": publish_channel,
                    "active": store.active_release().await
                }),
                AuditOutcome::Success,
            )
            .await;
            let active = store.active_release().await;
            (
                StatusCode::OK,
                Json(serde_json::json!({"published": true, "active": active})),
            )
                .into_response()
        }
        Err(e) => {
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_PUBLISH",
                serde_json::json!({
                    "error": e.to_string(),
                    "version": publish_version,
                    "channel": publish_channel
                }),
                AuditOutcome::Error(e.to_string()),
            )
            .await;
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"published": false, "error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn admin_routing_simulate_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RoutingSimulateRequest>,
) -> impl IntoResponse {
    let Some(store) = &state.routing_policy_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Routing policy store not configured"})),
        )
            .into_response();
    };

    let result = if let Some(rules) = payload.rules {
        let engine = RoutingPolicyEngine::new(rules);
        engine.simulate(&payload.scenarios)
    } else {
        match store.simulate_active(&payload.scenarios).await {
            Some(sim) => sim,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error":"No active routing policy"})),
                )
                    .into_response();
            }
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "simulated": true,
            "count": result.len(),
            "result": result
        })),
    )
        .into_response()
}

async fn admin_routing_promote_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RoutingPromoteRequest>,
) -> impl IntoResponse {
    let Some(store) = &state.routing_policy_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Routing policy store not configured"})),
        )
            .into_response();
    };

    match store
        .promote_canary_to_stable(payload.version.as_deref())
        .await
    {
        Ok(()) => {
            let active_stable = store
                .active_release_for_channel(RoutingPolicyChannel::Stable)
                .await;
            state.emit_event(
                multi_agent_core::events::EventEnvelope::new(
                    multi_agent_core::events::EventType::AuditAppended,
                    serde_json::json!({
                        "action": "ROUTING_POLICY_PROMOTE",
                        "version": payload.version,
                        "active_stable": active_stable
                    }),
                )
                .with_actor("admin"),
            );
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_PROMOTE",
                serde_json::json!({
                    "version": payload.version,
                    "active_stable": store.active_release_for_channel(RoutingPolicyChannel::Stable).await
                }),
                AuditOutcome::Success,
            )
            .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "promoted": true,
                    "active_stable": store.active_release_for_channel(RoutingPolicyChannel::Stable).await
                })),
            )
                .into_response()
        }
        Err(e) => {
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_PROMOTE",
                serde_json::json!({
                    "version": payload.version,
                    "error": e.to_string(),
                }),
                AuditOutcome::Error(e.to_string()),
            )
            .await;
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "promoted": false,
                    "error": e.to_string()
                })),
            )
                .into_response()
        }
    }
}

async fn admin_routing_rollback_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RoutingRollbackRequest>,
) -> impl IntoResponse {
    let Some(store) = &state.routing_policy_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Routing policy store not configured"})),
        )
            .into_response();
    };

    match store.rollback_to(&payload.version).await {
        Ok(()) => {
            state.emit_event(
                multi_agent_core::events::EventEnvelope::new(
                    multi_agent_core::events::EventType::AuditAppended,
                    serde_json::json!({
                        "action": "ROUTING_POLICY_ROLLBACK",
                        "version": payload.version,
                        "active": store.active_release().await
                    }),
                )
                .with_actor("admin"),
            );
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_ROLLBACK",
                serde_json::json!({
                    "version": payload.version,
                    "active": store.active_release().await
                }),
                AuditOutcome::Success,
            )
            .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "rolled_back": true,
                    "active": store.active_release().await
                })),
            )
                .into_response()
        }
        Err(e) => {
            log_routing_admin_audit(
                &state,
                "ROUTING_POLICY_ROLLBACK",
                serde_json::json!({
                    "version": payload.version,
                    "error": e.to_string()
                }),
                AuditOutcome::Error(e.to_string()),
            )
            .await;
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "rolled_back": false,
                    "error": e.to_string()
                })),
            )
                .into_response()
        }
    }
}

async fn admin_routing_audits_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RoutingAuditQuery>,
) -> impl IntoResponse {
    let Some(admin_state) = &state.admin_state else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Admin state not configured"})),
        )
            .into_response();
    };

    let filter = AuditFilter {
        action: query.action.clone(),
        limit: query.limit.or(Some(200)),
        ..Default::default()
    };

    match admin_state.audit_store.query(filter).await {
        Ok(entries) => {
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| query.action.is_some() || e.action.starts_with("ROUTING_POLICY_"))
                .filter(|e| {
                    if let Some(ref version) = query.version {
                        e.metadata
                            .as_ref()
                            .and_then(|m| m.get("version"))
                            .and_then(|v| v.as_str())
                            == Some(version.as_str())
                    } else {
                        true
                    }
                })
                .filter(|e| {
                    if let Some(ref channel) = query.channel {
                        e.metadata
                            .as_ref()
                            .and_then(|m| m.get("channel"))
                            .and_then(|v| v.as_str())
                            == Some(channel.as_str())
                    } else {
                        true
                    }
                })
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "count": filtered.len(),
                    "entries": filtered
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn admin_routing_policies_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(store) = &state.routing_policy_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Routing policy store not configured"})),
        )
            .into_response();
    };
    let active = store.active_release().await;
    let active_stable = store
        .active_release_for_channel(RoutingPolicyChannel::Stable)
        .await;
    let active_canary = store
        .active_release_for_channel(RoutingPolicyChannel::Canary)
        .await;
    let history = store.list_versions().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "active": active,
            "active_stable": active_stable,
            "active_canary": active_canary,
            "history": history
        })),
    )
        .into_response()
}

/// Research agent handler.
async fn research_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResearchRequest>,
) -> impl IntoResponse {
    let orchestrator = match &state.research_orchestrator {
        Some(o) => o,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Research orchestrator not enabled"})),
            )
                .into_response()
        }
    };

    let session_id = format!("sync-rs-{}", Uuid::new_v4());
    let user_id = req.user_id.unwrap_or_else(|| "anonymous".to_string());

    match orchestrator
        .run_research(&session_id, &user_id, &req.query)
        .await
    {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "report": report,
                "session_id": session_id,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Research failed: {}", e)
            })),
        )
            .into_response(),
    }
}

/// Chat handler.
async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ChatRequest>,
) -> impl IntoResponse {
    let trace_id = Uuid::new_v4().to_string();

    tracing::info!(
        trace_id = %trace_id,
        message_len = payload.message.len(),
        "Processing chat request"
    );

    // Emit REQUEST_RECEIVED event
    {
        use multi_agent_core::events::{EventEnvelope, EventType};
        let event = EventEnvelope::new(
            EventType::RequestReceived,
            serde_json::json!({
                "message_len": payload.message.len(),
                "has_session": payload.session_id.is_some(),
                "has_user": payload.user_id.is_some()
            }),
        )
        .with_trace(&trace_id)
        .with_actor(payload.user_id.as_deref().unwrap_or("anonymous"));

        if let Some(sid) = &payload.session_id {
            state.emit_event(event.with_session(sid));
        } else {
            state.emit_event(event);
        }
    }

    let workspace_id = payload.workspace_id.as_deref().unwrap_or("default");
    let session_id = payload.session_id.as_deref().unwrap_or("default");

    // Check semantic cache first
    match state
        .cache
        .get(workspace_id, session_id, &payload.message)
        .await
    {
        Ok(Some(cached_response)) => {
            tracing::info!(trace_id = %trace_id, workspace = %workspace_id, session = %session_id, "Cache hit");
            return (
                StatusCode::OK,
                Json(ApiEnvelope::success(
                    trace_id.clone(),
                    ChatResponse {
                        trace_id,
                        intent: UserIntent::FastAction {
                            tool_name: "cache".to_string(),
                            args: serde_json::json!({}),
                            user_id: payload.user_id.clone(),
                        },
                        result: Some(AgentResult::Text(cached_response)),
                        cached: true,
                    },
                )),
            )
                .into_response();
        }
        Ok(None) => {
            tracing::debug!(trace_id = %trace_id, "Cache miss");
        }
        Err(e) => {
            tracing::warn!(trace_id = %trace_id, error = %e, "Cache error");
        }
    }

    // Create normalized request
    let request = NormalizedRequest {
        trace_id: trace_id.clone(),
        content: payload.message.clone(),
        original_content: multi_agent_core::types::RequestContent::Text(payload.message.clone()),
        refs: Vec::new(),
        metadata: RequestMetadata {
            user_id: payload.user_id.clone(),
            workspace_id: payload.workspace_id.clone(),
            session_id: payload.session_id.clone(),
            trace_id: Some(trace_id.clone()),
            custom: Default::default(),
        },
    };

    // Classify intent
    let intent = match state.router.classify_detailed(&request).await {
        Ok((intent, routing_diagnostics)) => {
            // Emit INTENT_RESOLVED
            {
                use multi_agent_core::events::{EventEnvelope, EventType};
                let event = EventEnvelope::new(
                    EventType::IntentResolved,
                    serde_json::json!({
                        "intent_type": format!("{:?}", intent),
                        "routing": routing_diagnostics.get("routing").cloned().unwrap_or_else(|| serde_json::json!({"source":"unknown"}))
                    }),
                )
                .with_trace(&trace_id);
                state.emit_event(event);
            }
            intent
        }
        Err(e) => {
            tracing::error!(trace_id = %trace_id, error = %e, "Failed to classify intent");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiEnvelope::success(
                    trace_id.clone(),
                    ApiErrorBody::new(ApiErrorCode::RoutingFailed, e.to_string(), true)
                        .with_details(serde_json::json!({
                            "user_id": payload.user_id,
                        })),
                )),
            )
                .into_response();
        }
    };

    // Execute via controller if available
    let result = if let Some(ref controller) = state.controller {
        let controller = controller.clone();
        let intent_for_exec = intent.clone();
        let trace_for_exec = trace_id.clone();
        let session_lane = request.metadata.session_id.clone();
        let execution = state
            .controller_scheduler
            .run(session_lane.as_deref(), move || async move {
                controller.execute(intent_for_exec, trace_for_exec).await
            })
            .await;
        match execution {
            Ok(result) => {
                // Cache successful text responses
                if let AgentResult::Text(ref text) = result {
                    // Extract IDs again as payload was moved or use references
                    let w_id = request
                        .metadata
                        .workspace_id
                        .as_deref()
                        .unwrap_or("default");
                    let s_id = request.metadata.session_id.as_deref().unwrap_or("default");
                    let _ = state.cache.set(w_id, s_id, &request.content, text).await;
                }
                Some(result)
            }
            Err(e) => {
                tracing::error!(trace_id = %trace_id, error = %e, "Controller execution failed");
                Some(AgentResult::Error {
                    message: e.to_string(),
                    code: "EXECUTION_ERROR".to_string(),
                })
            }
        }
    } else {
        // No controller - return intent classification only
        tracing::debug!(trace_id = %trace_id, "No controller, returning intent only");
        Some(AgentResult::Text(format!(
            "Intent classified. Controller not available in Phase 1. Intent: {:?}",
            intent
        )))
    };

    (
        StatusCode::OK,
        Json(ApiEnvelope::success(
            trace_id.clone(),
            ChatResponse {
                trace_id,
                intent,
                result,
                cached: false,
            },
        )),
    )
        .into_response()
}

/// OpenAI-compatible chat completions request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

/// OpenAI-compatible message.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// OpenAI-compatible chat completions response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAiChoice>,
    pub usage: OpenAiUsage,
}

/// OpenAI-compatible choice.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiChoice {
    pub index: u32,
    pub message: OpenAiMessage,
    pub finish_reason: String,
}

/// OpenAI-compatible usage.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

async fn openai_chat_completions_handler(
    State(state): State<Arc<AppState>>,
    user_ctx: Option<axum::Extension<multi_agent_governance::rbac::UserContext>>,
    user_roles: Option<axum::Extension<multi_agent_governance::rbac::UserRoles>>,
    Json(payload): Json<OpenAiChatRequest>,
) -> impl IntoResponse {
    let user_id = if let Some(axum::Extension(u)) = user_ctx {
        u.user_id.clone()
    } else if let Some(axum::Extension(u)) = user_roles {
        u.user_id.clone()
    } else {
        "anonymous".to_string()
    };

    let client = match &state.tiered_client {
        Some(c) => c,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": "LLM Gateway tiered routing client is not configured on this server.",
                        "type": "internal_error",
                        "code": "tiered_client_missing"
                    }
                })),
            )
                .into_response();
        }
    };

    if payload.stream.unwrap_or(false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": "Streaming completions are not supported by the gateway.",
                    "type": "invalid_request_error",
                    "code": "streaming_not_supported"
                }
            })),
        )
            .into_response();
    }

    use multi_agent_core::traits::{ChatMessage, LlmClient};
    use multi_agent_core::types::ModelTier;

    let chat_messages: Vec<ChatMessage> = payload
        .messages
        .iter()
        .map(|msg| ChatMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: None,
        })
        .collect();

    // Call tiered client to execute chat completion
    match client.chat(&chat_messages).await {
        Ok(llm_res) => {
            // Determine resolved model tier and details
            let tier = client.classify_messages(&chat_messages);
            let resolved_model = match tier {
                ModelTier::Fast => "openai:gpt-4o-mini",
                ModelTier::Standard => "openai:gpt-4o",
                ModelTier::Premium => "anthropic:claude-3-5-sonnet-20241022",
            };

            let prompt_tokens = llm_res.usage.prompt_tokens;
            let completion_tokens = llm_res.usage.completion_tokens;
            let total_tokens = llm_res.usage.total_tokens;

            // Fetch pricing for cost calculation
            let pricing_registry = multi_agent_model_gateway::PricingRegistry::with_defaults();
            let cost = pricing_registry
                .get(resolved_model)
                .map(|p| p.estimate_cost(prompt_tokens, completion_tokens))
                .unwrap_or(0.0);

            // Log cryptographic audit trail if admin state is present
            if let Some(admin_state) = &state.admin_state {
                let _ = admin_state
                    .audit_store
                    .log(multi_agent_governance::AuditEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        user_id: user_id.clone(),
                        action: "chat_completions".to_string(),
                        resource: resolved_model.to_string(),
                        outcome: multi_agent_governance::AuditOutcome::Success,
                        metadata: Some(serde_json::json!({
                            "requested_model": payload.model,
                            "resolved_model": resolved_model,
                            "prompt_tokens": prompt_tokens,
                            "completion_tokens": completion_tokens,
                            "total_tokens": total_tokens,
                            "cost": cost,
                        })),
                        previous_hash: None,
                        hash: None,
                    })
                    .await;
            }

            // Construct standard OpenAI chat completion response
            let response = OpenAiChatResponse {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion".to_string(),
                created: chrono::Utc::now().timestamp() as u64,
                model: resolved_model.to_string(),
                choices: vec![OpenAiChoice {
                    index: 0,
                    message: OpenAiMessage {
                        role: "assistant".to_string(),
                        content: llm_res.content,
                        name: None,
                    },
                    finish_reason: llm_res.finish_reason,
                }],
                usage: OpenAiUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                },
            };

            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!("LLM Gateway chat completion failed: {}", e);
            if let Some(admin_state) = &state.admin_state {
                let _ = admin_state
                    .audit_store
                    .log(multi_agent_governance::AuditEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        user_id: user_id.clone(),
                        action: "chat_completions".to_string(),
                        resource: payload.model.clone(),
                        outcome: multi_agent_governance::AuditOutcome::Error(e.to_string()),
                        metadata: Some(serde_json::json!({
                            "requested_model": payload.model,
                        })),
                        previous_hash: None,
                        hash: None,
                    })
                    .await;
            }

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("LLM Gateway completion failed: {}", e),
                        "type": "api_error",
                        "code": "completion_failed"
                    }
                })),
            )
                .into_response()
        }
    }
}

/// Intent classification handler (for debugging/testing).
async fn intent_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<IntentRequest>,
) -> impl IntoResponse {
    let trace_id = Uuid::new_v4().to_string();
    let request = NormalizedRequest::text(&payload.message);

    match state.router.classify_detailed(&request).await {
        Ok((intent, routing_diagnostics)) => {
            tracing::debug!(trace_id = %trace_id, diagnostics = %routing_diagnostics, "Intent endpoint diagnostics");
            (
                StatusCode::OK,
                Json(ApiEnvelope::success(
                    trace_id.clone(),
                    IntentResponse { trace_id, intent },
                )),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiEnvelope::success(
                trace_id,
                ApiErrorBody::new(ApiErrorCode::RoutingFailed, e.to_string(), true),
            )),
        )
            .into_response(),
    }
}

/// Webhook request payload.
#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    /// Arbitrary event data.
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Webhook response.
#[derive(Debug, Serialize)]
pub struct WebhookResponse {
    /// Trace ID.
    pub trace_id: String,
    /// Whether the event was accepted.
    pub accepted: bool,
    /// Optional message.
    pub message: Option<String>,
    /// Classified intent (if processing is enabled).
    pub intent: Option<UserIntent>,
}

/// Webhook handler for system events.
///
/// Accepts events at `/v1/webhook/:event_type` and processes them
/// as system events through the normal intent routing pipeline.
async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(event_type): Path<String>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    let trace_id = Uuid::new_v4().to_string();

    tracing::info!(
        trace_id = %trace_id,
        event_type = %event_type,
        "Received webhook event"
    );

    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let request_fingerprint = serde_json::json!({
        "event_type": event_type,
        "payload": payload.data,
    });
    let request_hash = IdempotencyStore::hash_payload(&request_fingerprint);
    if let Some(ref key) = idempotency_key {
        match state.idempotency_store.check("webhook", key, &request_hash) {
            IdempotencyLookup::Replay(record) => {
                let status = StatusCode::from_u16(record.status).unwrap_or(StatusCode::OK);
                return (status, Json(record.body)).into_response();
            }
            IdempotencyLookup::Conflict => {
                return (
                    StatusCode::CONFLICT,
                    Json(ApiEnvelope::success(
                        trace_id.clone(),
                        ApiErrorBody::new(
                            ApiErrorCode::Conflict,
                            "Idempotency key reused with different payload",
                            false,
                        ),
                    )),
                )
                    .into_response();
            }
            IdempotencyLookup::Miss => {}
        }
    }

    // Create a normalized request from the system event
    let event_summary = format!(
        "System event: {} - {}",
        event_type,
        serde_json::to_string(&request_fingerprint["payload"])
            .unwrap_or_else(|_| "invalid payload".to_string())
            .chars()
            .take(200)
            .collect::<String>()
    );

    let request = NormalizedRequest {
        trace_id: trace_id.clone(),
        content: event_summary.clone(),
        original_content: RequestContent::SystemEvent {
            event_type: event_type.clone(),
            payload: request_fingerprint["payload"].clone(),
        },
        refs: Vec::new(),
        metadata: RequestMetadata::default(),
    };

    // Classify the event
    let intent = match state.router.classify_detailed(&request).await {
        Ok((intent, routing_diagnostics)) => {
            tracing::debug!(trace_id = %trace_id, diagnostics = %routing_diagnostics, "Webhook intent diagnostics");
            Some(intent)
        }
        Err(e) => {
            tracing::warn!(
                trace_id = %trace_id,
                error = %e,
                "Failed to classify webhook event"
            );
            None
        }
    };

    // For now, just acknowledge the event
    // In full implementation, we would execute via controller
    let response_body = ApiEnvelope::success(
        trace_id.clone(),
        WebhookResponse {
            trace_id,
            accepted: true,
            message: Some(format!("Event '{}' received", event_type)),
            intent,
        },
    );
    let response_value = serde_json::to_value(&response_body).unwrap_or_else(|_| {
        serde_json::json!({
            "version": GATEWAY_CONTRACT_VERSION,
            "trace_id": "",
            "data": { "accepted": false, "message": "serialization error" },
        })
    });
    if let Some(key) = idempotency_key {
        state.idempotency_store.store(
            "webhook",
            &key,
            request_hash,
            StatusCode::OK.as_u16(),
            response_value.clone(),
        );
    }

    (StatusCode::OK, Json(response_value)).into_response()
}

// =============================================================================
// HITL Approval Endpoints
// =============================================================================

/// WebSocket approval request message (sent to client).
#[derive(Debug, Serialize)]
struct WsApprovalRequest {
    /// Message type.
    #[serde(rename = "type")]
    msg_type: String,
    /// The approval request data.
    data: multi_agent_core::types::ApprovalRequest,
}

/// WebSocket approval response message (received from client).
#[derive(Debug, Deserialize)]
struct WsApprovalResponse {
    /// Message type (should be "approval_response").
    #[serde(rename = "type")]
    #[allow(dead_code)]
    msg_type: String,
    /// Request ID being responded to.
    request_id: String,
    /// Nonce for security validation.
    nonce: String,
    /// Decision: "approved", "denied", or "modified".
    decision: String,
    /// Reason (for denied or optional for others).
    reason: Option<String>,
    /// Reason code (mandatory for auditing).
    reason_code: Option<String>,
    /// Modified args (for modified).
    modified_args: Option<serde_json::Value>,
}

/// REST approval request body.
#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    /// Nonce for security validation.
    pub nonce: String,
    /// Decision: "approved" or "denied".
    pub decision: String,
    /// Reason (for denied).
    pub reason: Option<String>,
    /// Reason code (e.g., "USER_APPROVED", "USER_DENIED").
    pub reason_code: Option<String>,
}

/// REST approval response.
#[derive(Debug, Serialize)]
pub struct ApproveResponse {
    /// Whether the response was accepted.
    pub accepted: bool,
    /// Message.
    pub message: String,
}

/// WebSocket handler for real-time approval flow.
///
/// Clients connect via `ws://host/ws/approval` and receive approval requests
/// as JSON. They respond with approval/denial decisions.
async fn approval_ws_handler(
    State(state): State<Arc<AppState>>,
    user: Option<axum::Extension<multi_agent_governance::rbac::UserContext>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let user_ctx = user.map(|axum::Extension(u)| u).unwrap_or_else(|| {
        multi_agent_governance::rbac::UserContext {
            user_id: "anonymous_approver".to_string(),
            roles: vec!["admin".to_string()],
            permissions: vec!["*".to_string()],
            session_id: None,
        }
    });
    ws.on_upgrade(move |socket| handle_approval_ws(state, user_ctx, socket))
}

async fn handle_approval_ws(
    state: Arc<AppState>,
    user: multi_agent_governance::rbac::UserContext,
    mut socket: WebSocket,
) {
    let gate = match &state.approval_gate {
        Some(gate) => gate.clone(),
        None => {
            tracing::warn!(
                "WebSocket approval connection attempted but no approval gate configured"
            );
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": "Approval gate not configured"})
                        .to_string(),
                ))
                .await;
            return;
        }
    };

    let mut rx = gate.subscribe();

    loop {
        tokio::select! {
            // Forward approval requests from broadcast channel to WebSocket
            result = rx.recv() => {
                match result {
                    Ok(req) => {
                        let msg = WsApprovalRequest {
                            msg_type: "approval_request".to_string(),
                            data: req,
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if socket.send(Message::Text(json)).await.is_err() {
                                break; // Client disconnected
                            }
                        }
                    }
                    Err(_) => break, // Broadcast sender dropped
                }
            }
            // Receive approval responses from WebSocket client
            result = socket.recv() => {
                match result {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsApprovalResponse>(&text) {
                            Ok(resp) => {
                                let approval_response = match resp.decision.as_str() {
                                    "approved" => ApprovalResponse::Approved {
                                        reason: resp.reason.clone(),
                                        reason_code: resp.reason_code.clone().unwrap_or_else(|| "USER_APPROVED".to_string()),
                                        approver_id: None,
                                        approver_role: None,
                                    },
                                    "denied" => ApprovalResponse::Denied {
                                        reason: resp.reason.clone().unwrap_or_else(|| "Denied via WebSocket".into()),
                                        reason_code: resp.reason_code.clone().unwrap_or_else(|| "USER_DENIED".to_string()),
                                        approver_id: None,
                                        approver_role: None,
                                    },
                                    "modified" => match resp.modified_args {
                                        Some(args) => ApprovalResponse::Modified {
                                            args,
                                            reason: resp.reason.clone(),
                                            reason_code: resp.reason_code.clone().unwrap_or_else(|| "USER_MODIFIED".to_string()),
                                            approver_id: None,
                                            approver_role: None,
                                        },
                                        None => ApprovalResponse::Denied {
                                            reason: "Modified without args".into(),
                                            reason_code: "INVALID_RESPONSE".to_string(),
                                            approver_id: None,
                                            approver_role: None,
                                        },
                                    },
                                    _ => {
                                        tracing::warn!("Unknown decision: {}", resp.decision);
                                        continue;
                                    }
                                };

                                if let Err(e) = gate.submit_response(
                                    &resp.request_id,
                                    &resp.nonce,
                                    user.user_id.clone(),
                                    user.roles.clone(),
                                    approval_response
                                ).await {
                                    tracing::warn!("Failed to submit approval response: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Invalid approval response JSON: {}", e);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::warn!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    tracing::info!("Approval WebSocket session ended");
}

async fn logs_ws_handler(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_logs_ws(state, socket))
}

async fn handle_logs_ws(state: Arc<AppState>, mut socket: WebSocket) {
    let mut rx = match &state.logs_channel {
        Some(tx) => tx.subscribe(),
        None => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"type": "error", "message": "Logs channel not configured"})
                        .to_string(),
                ))
                .await;
            return;
        }
    };

    loop {
        match rx.recv().await {
            Ok(log_line) => {
                if socket.send(Message::Text(log_line)).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                // Skip lagged messages
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break;
            }
        }
    }
}

/// REST endpoint for submitting approval decisions.
///
/// `POST /v1/approve/:request_id`
async fn approve_rest_handler(
    State(state): State<Arc<AppState>>,
    user: Option<axum::Extension<multi_agent_governance::rbac::UserContext>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    Json(payload): Json<ApproveRequest>,
) -> impl IntoResponse {
    let trace_id = Uuid::new_v4().to_string();
    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let request_fingerprint = serde_json::json!({
        "request_id": request_id,
        "nonce": payload.nonce,
        "decision": payload.decision,
        "reason": payload.reason,
        "reason_code": payload.reason_code,
    });
    let request_hash = IdempotencyStore::hash_payload(&request_fingerprint);
    if let Some(ref key) = idempotency_key {
        match state.idempotency_store.check("approve", key, &request_hash) {
            IdempotencyLookup::Replay(record) => {
                let status = StatusCode::from_u16(record.status).unwrap_or(StatusCode::OK);
                return (status, Json(record.body)).into_response();
            }
            IdempotencyLookup::Conflict => {
                return (
                    StatusCode::CONFLICT,
                    Json(ApiEnvelope::success(
                        trace_id.clone(),
                        ApiErrorBody::new(
                            ApiErrorCode::Conflict,
                            "Idempotency key reused with different payload",
                            false,
                        ),
                    )),
                )
                    .into_response();
            }
            IdempotencyLookup::Miss => {}
        }
    }

    let finalize = |status: StatusCode, body: ApproveResponse| -> Response {
        let wrapped = ApiEnvelope::success(trace_id.clone(), body);
        if let Some(ref key) = idempotency_key {
            if let Ok(value) = serde_json::to_value(&wrapped) {
                state.idempotency_store.store(
                    "approve",
                    key,
                    request_hash.clone(),
                    status.as_u16(),
                    value.clone(),
                );
                return (status, Json(value)).into_response();
            }
        }
        (status, Json(wrapped)).into_response()
    };

    let user_ctx = user.map(|axum::Extension(u)| u).unwrap_or_else(|| {
        multi_agent_governance::rbac::UserContext {
            user_id: "anonymous_approver".to_string(),
            roles: vec!["admin".to_string()],
            permissions: vec!["*".to_string()],
            session_id: None,
        }
    });

    let gate = match &state.approval_gate {
        Some(gate) => gate.clone(),
        None => {
            return finalize(
                StatusCode::SERVICE_UNAVAILABLE,
                ApproveResponse {
                    accepted: false,
                    message: "Approval gate not configured".into(),
                },
            );
        }
    };

    let response = match payload.decision.as_str() {
        "approved" => ApprovalResponse::Approved {
            reason: payload.reason.clone(),
            reason_code: payload
                .reason_code
                .clone()
                .unwrap_or_else(|| "USER_APPROVED".to_string()),
            approver_id: None,
            approver_role: None,
        },
        "denied" => ApprovalResponse::Denied {
            reason: payload
                .reason
                .clone()
                .unwrap_or_else(|| "Denied via REST".into()),
            reason_code: payload
                .reason_code
                .clone()
                .unwrap_or_else(|| "USER_DENIED".to_string()),
            approver_id: None,
            approver_role: None,
        },
        _ => {
            return finalize(
                StatusCode::BAD_REQUEST,
                ApproveResponse {
                    accepted: false,
                    message: format!(
                        "Unknown decision: '{}'. Use 'approved' or 'denied'.",
                        payload.decision
                    ),
                },
            );
        }
    };

    match gate
        .submit_response(
            &request_id,
            &payload.nonce,
            user_ctx.user_id.clone(),
            user_ctx.roles.clone(),
            response,
        )
        .await
    {
        Ok(()) => finalize(
            StatusCode::OK,
            ApproveResponse {
                accepted: true,
                message: format!("Response submitted for request '{}'", request_id),
            },
        ),
        Err(e) => finalize(
            StatusCode::NOT_FOUND,
            ApproveResponse {
                accepted: false,
                message: e,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_handler() {
        let response = health_handler().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    use axum::body::Body;
    use axum::middleware::from_fn_with_state;
    use multi_agent_governance::StaticTokenRbacConnector;

    #[tokio::test]
    async fn test_bearer_auth_middleware_unauthorized() {
        let rbac = Arc::new(StaticTokenRbacConnector::new("secret-token"));
        let state = Arc::new(AppState {
            router: Arc::new(crate::DefaultRouter::new()),
            cache: Arc::new(crate::InMemorySemanticCache::new(Arc::new(
                multi_agent_model_gateway::MockLlmClient::new("dummy"),
            ))),
            controller: None,
            rate_limiter: None,
            approval_gate: None,
            logs_channel: None,
            policy_engine: None,
            admin_state: Some(Arc::new(multi_agent_admin::AdminState {
                audit_store: Arc::new(multi_agent_governance::InMemoryAuditStore::new()),
                rbac: rbac.clone(),
                metrics: None,
                mcp_registry: Arc::new(multi_agent_skills::mcp_registry::McpRegistry::new()),
                providers: Arc::new(tokio::sync::RwLock::new(vec![])),
                provider_store: None,
                secrets: Arc::new(multi_agent_governance::AesGcmSecretsManager::new(None)),
                privacy_controller: None,
                artifact_store: None,
                session_store: None,
                app_config: multi_agent_core::config::AppConfig::default(),
                network_policy: Arc::new(tokio::sync::RwLock::new(
                    multi_agent_governance::network::NetworkPolicy::default(),
                )),
                llm_client: None,
                tool_registry: None,
            })),
            plugin_manager: None,
            app_config: multi_agent_core::config::AppConfig::default(),
            research_orchestrator: None,
            idempotency_store: Arc::new(IdempotencyStore::new()),
            controller_scheduler: Arc::new(ControllerScheduler::default()),
            routing_policy_store: None,
            tiered_client: None,
        });

        let app = Router::new()
            .route("/", get(|| async { "Secret content" }))
            .layer(from_fn_with_state(state.clone(), bearer_auth_middleware))
            .with_state(state);

        use axum::http::Request;
        use tower::ServiceExt;

        // No header
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Invalid token
        let req = Request::builder()
            .uri("/")
            .header("Authorization", "Bearer invalid")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Valid token
        let req = Request::builder()
            .uri("/")
            .header("Authorization", "Bearer secret-token")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

// =============================================================================
// Middleware
// =============================================================================

/// Middleware for distributed rate limiting.
async fn distributed_rate_limit(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Extract ConnectInfo manually
    let addr = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or_else(|| std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));

    if let Some(limiter) = &state.rate_limiter {
        let key = format!("rate_limit:{}", addr);

        // 120 requests per minute
        match limiter
            .check_and_increment(&key, 120, std::time::Duration::from_secs(60))
            .await
        {
            Ok(allowed) => {
                if !allowed {
                    return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
                }
            }
            Err(e) => {
                tracing::error!("Rate limiter error: {}", e);
            }
        }
    }

    next.run(req).await
}

/// Middleware for bearer token authentication.
async fn bearer_auth_middleware(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum_extra::extract::cookie::CookieJar;

    // Check for Admin Token first (if configured)
    if let Some(admin_secret) = &state.app_config.governance.admin_token {
        use secrecy::ExposeSecret;
        let admin_token_str = admin_secret.expose_secret();

        // 1. Check Header: x-admin-token
        let header_token = req
            .headers()
            .get("x-admin-token")
            .and_then(|h| h.to_str().ok());

        // 2. Check Cookie: admin_token
        let cookie_token = if header_token.is_none() {
            let jar = CookieJar::from_headers(req.headers());
            jar.get("admin_token").map(|c| c.value().to_string())
        } else {
            None
        };

        let candidate = header_token.or(cookie_token.as_deref());

        if let Some(t) = candidate {
            if t == admin_token_str {
                // Determine user identity
                // For admin token, we create a superuser identity
                let user = multi_agent_governance::rbac::UserContext {
                    user_id: "admin".to_string(),
                    roles: vec!["admin".to_string()],
                    permissions: vec!["*".to_string()], // Superuser
                    session_id: None,
                };

                let mut req = req;
                req.extensions_mut().insert(user);
                return next.run(req).await;
            }
        }
    }

    // 3. Fallback to Standard Bearer Auth (RBAC)
    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                "Missing or invalid authorization header",
            )
                .into_response();
        }
    };

    // Validate RBAC token
    let rbac = state.admin_state.as_ref().map(|s| s.rbac.clone());

    match rbac {
        Some(rbac) => match rbac.validate(token).await {
            Ok(user) => {
                let mut req = req;
                req.extensions_mut().insert(user);
                next.run(req).await
            }
            Err(e) => {
                tracing::warn!("Auth validation failed: {}", e);
                (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
            }
        },
        None => {
            tracing::error!("Auth middleware active but no RBAC connector configured");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication system misconfigured",
            )
                .into_response()
        }
    }
}

/// Middleware to restrict access to localhost.
async fn restrict_to_localhost(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if state.app_config.governance.admin_allow_external_access || addr.ip().is_loopback() {
        next.run(req).await
    } else {
        tracing::warn!(client_ip = %addr.ip(), "Blocked non-localhost access to Admin API");
        (StatusCode::FORBIDDEN, "Admin API restricted to localhost").into_response()
    }
}

// =============================================================================
// Plugin Management Endpoints
// =============================================================================

/// List all plugins.
async fn get_plugins_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if let Some(manager) = &state.plugin_manager {
        let plugins = manager.list();
        let response: Vec<serde_json::Value> = plugins
            .into_iter()
            .map(|(manifest, enabled)| {
                serde_json::json!({
                    "id": manifest.id,
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": manifest.description,
                    "enabled": enabled,
                    "capabilities": manifest.capabilities,
                })
            })
            .collect();
        (StatusCode::OK, Json(response)).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Plugin manager not configured"})),
        )
            .into_response()
    }
}

/// Get plugin details.
async fn get_plugin_details_handler(
    State(state): State<Arc<AppState>>,
    Path(plugin_id): Path<String>,
) -> impl IntoResponse {
    if let Some(manager) = &state.plugin_manager {
        if let Some(manifest) = manager.get(&plugin_id) {
            let enabled = manager.is_enabled(&plugin_id);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "manifest": manifest,
                    "enabled": enabled,
                })),
            )
                .into_response()
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Plugin not found"})),
            )
                .into_response()
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Plugin manager not configured"})),
        )
            .into_response()
    }
}

/// Toggle plugin enabled state.
#[derive(Debug, Deserialize)]
pub struct TogglePluginRequest {
    pub enabled: bool,
}

async fn toggle_plugin_handler(
    State(state): State<Arc<AppState>>,
    Path(plugin_id): Path<String>,
    Json(payload): Json<TogglePluginRequest>,
) -> impl IntoResponse {
    if let Some(manager) = &state.plugin_manager {
        let result = if payload.enabled {
            manager.enable(&plugin_id).await
        } else {
            manager.disable(&plugin_id).await
        };

        match result {
            Ok(_) => {
                // Return updated state
                let enabled = manager.is_enabled(&plugin_id);
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "id": plugin_id,
                        "enabled": enabled,
                        "message": if enabled { "Plugin enabled" } else { "Plugin disabled" }
                    })),
                )
                    .into_response()
            }
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Plugin manager not configured"})),
        )
            .into_response()
    }
}
