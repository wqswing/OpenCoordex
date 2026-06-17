//! Admin API for OpenCoordex management dashboard.
//!
//! Provides endpoints for:
//! - LLM Provider management
//! - S3 Persistence configuration
//! - MCP Registry management
//! - Metrics and observability
//! - Audit log queries
//! - Static dashboard UI

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use multi_agent_governance::{AuditFilter, AuditStore, RbacConnector};
use multi_agent_governance::{PrivacyController, SecretsManager};
use multi_agent_harness::schema::{OutputAssertion, Suite, TestCase};
use multi_agent_harness::HarnessRunner;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Embedded static assets for the dashboard.
#[derive(RustEmbed)]
#[folder = "../../dashboard/static"]
struct Asset;

use multi_agent_core::traits::{
    ArtifactStore, LlmClient, ProviderStore, SessionStore, ToolRegistry,
};
use multi_agent_core::types::RefId;
use multi_agent_skills::mcp_registry::{McpRegistry, McpServerInfo};
use sha2::{Digest, Sha256};
use std::io::Write;

pub mod doctor;

// =========================================
// State & Data Structures
// =========================================

/// Admin API state.
pub struct AdminState {
    pub audit_store: Arc<dyn AuditStore>,
    pub rbac: Arc<dyn RbacConnector>,
    pub metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
    pub mcp_registry: Arc<McpRegistry>,
    /// In-memory provider storage (used when `provider_store` is None).
    pub providers: Arc<RwLock<Vec<ProviderEntry>>>,
    /// External provider store (e.g., Redis/PostgreSQL).
    /// When set, this is used instead of in-memory `providers`.
    pub provider_store: Option<Arc<dyn ProviderStore>>,
    /// Secrets manager for encrypting sensitive data (API keys).
    pub secrets: Arc<dyn SecretsManager>,
    /// Privacy controller for GDPR operations.
    pub privacy_controller: Option<Arc<PrivacyController>>,
    /// Artifact Store for diagnostics.
    pub artifact_store: Option<Arc<dyn ArtifactStore>>,
    /// Session Store for diagnostics.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Application configuration.
    pub app_config: multi_agent_core::config::AppConfig,
    /// Network Policy (mutable).
    pub network_policy: Arc<RwLock<multi_agent_governance::network::NetworkPolicy>>,
    /// LLM Client for harness diagnostics.
    pub llm_client: Option<Arc<dyn LlmClient>>,
    /// Tool Registry for harness diagnostics.
    pub tool_registry: Option<Arc<dyn ToolRegistry>>,
}

/// LLM Provider entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub vendor: String,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Key ID for retrieving the encrypted API key from SecretsManager.
    /// The actual API key is never stored in plain text.
    #[serde(skip_serializing)]
    pub api_key_id: String,
    pub capabilities: Vec<String>,
    pub status: String,
}

/// Request to add a provider.
#[derive(Debug, Deserialize)]
pub struct AddProviderRequest {
    pub vendor: String,
    pub model_id: String,
    pub description: Option<String>,
    pub base_url: String,
    pub version: Option<String>,
    pub api_key: String,
    pub capabilities: Vec<String>,
}

/// Request to test a provider connection.
#[derive(Debug, Deserialize)]
pub struct TestProviderRequest {
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
}

/// S3 Config request.
#[derive(Debug, Deserialize)]
pub struct S3ConfigRequest {
    pub bucket: String,
    pub endpoint: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub region: Option<String>,
}

/// MCP Server registration request.
#[derive(Debug, Deserialize)]
pub struct RegisterMcpRequest {
    pub name: String,
    pub transport_type: String,
    pub command: String,
    pub capabilities: Vec<String>,
}

/// Request to rotate secrets.
#[derive(Debug, Deserialize)]
pub struct RotateSecretsRequest {
    /// New 32-byte key (hex encoded).
    pub new_key_hex: String,
}

/// Response for config endpoint.
#[derive(Serialize)]
pub struct ConfigResponse {
    pub version: String,
    pub features: Vec<String>,
    pub persistence: PersistenceConfig,
    pub llm: LlmConfig,
}

#[derive(Serialize)]
pub struct PersistenceConfig {
    pub mode: String,
    pub s3_bucket: Option<String>,
    pub s3_endpoint: Option<String>,
}

#[derive(Serialize)]
pub struct LlmConfig {
    pub provider_source: String,
    pub providers_file_present: bool,
}

/// Query parameters for audit endpoint.
#[derive(Deserialize)]
pub struct AuditQuery {
    pub user_id: Option<String>,
    pub action: Option<String>,
    pub resource: Option<String>,
    pub from_timestamp: Option<String>,
    pub to_timestamp: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct SessionFilter {
    pub status: Option<multi_agent_core::types::SessionStatus>,
    pub user_id: Option<String>,
}

// =========================================
// Middleware
// =========================================

/// Authentication middleware.
async fn auth_middleware(
    State(state): State<Arc<AdminState>>,
    req: Request,
    next: Next,
) -> Response {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    match auth_header {
        Some(token) => match state.rbac.validate(token).await {
            Ok(roles) => {
                if roles.is_admin {
                    next.run(req).await
                } else {
                    StatusCode::FORBIDDEN.into_response()
                }
            }
            Err(_) => StatusCode::UNAUTHORIZED.into_response(),
        },
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

// =========================================
// Provider Endpoints
// =========================================

/// List all providers.
async fn list_providers(State(state): State<Arc<AdminState>>) -> Response {
    if let Some(store) = &state.provider_store {
        if let Ok(providers) = store.list().await {
            // Convert legacy core::ProviderEntry to admin::ProviderEntry
            let admin_providers: Vec<ProviderEntry> = providers
                .into_iter()
                .map(|p| ProviderEntry {
                    id: p.id,
                    vendor: p.vendor,
                    model_id: p.model_id,
                    description: p.description,
                    base_url: p.base_url,
                    version: p.version,
                    api_key_id: p.api_key_id,
                    capabilities: p.capabilities,
                    status: p.status,
                })
                .collect();
            return Json(admin_providers).into_response();
        }
        tracing::error!("Failed to list providers from store");
        return Json(Vec::<ProviderEntry>::new()).into_response();
    }
    let providers = state.providers.read().await;
    Json(providers.clone()).into_response()
}

/// Add a new provider.
async fn add_provider(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<AddProviderRequest>,
) -> Response {
    let provider_id = format!("prov-{}", chrono::Utc::now().timestamp_millis());

    // Encrypt the API key and store it in the secrets manager
    let api_key_id = format!("api_key:{}", provider_id);
    if state
        .secrets
        .store(&api_key_id, &req.api_key)
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let entry = ProviderEntry {
        id: provider_id,
        vendor: req.vendor,
        model_id: req.model_id,
        description: req.description,
        base_url: req.base_url,
        version: req.version,
        api_key_id,
        capabilities: req.capabilities,
        status: "active".to_string(), // Set to active by default
    };

    if let Some(store) = &state.provider_store {
        // Convert to core::ProviderEntry
        let core_entry = multi_agent_core::traits::ProviderEntry {
            id: entry.id.clone(),
            vendor: entry.vendor.clone(),
            model_id: entry.model_id.clone(),
            description: entry.description.clone(),
            base_url: entry.base_url.clone(),
            version: entry.version.clone(),
            api_key_id: entry.api_key_id.clone(),
            capabilities: entry.capabilities.clone(),
            status: entry.status.clone(),
        };
        if store.upsert(&core_entry).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    } else {
        let mut providers = state.providers.write().await;
        providers.push(entry.clone());
    }

    // Log audit event
    let _ = state
        .audit_store
        .log(multi_agent_governance::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: "ADD_PROVIDER".to_string(),
            resource: entry.id.clone(),
            outcome: multi_agent_governance::AuditOutcome::Success,
            metadata: Some(serde_json::json!({
                "model_id": entry.model_id
            })),
            previous_hash: None,
            hash: None,
        })
        .await;

    Json(entry).into_response()
}

/// Test provider connection.
async fn test_provider(Json(req): Json<TestProviderRequest>) -> Response {
    // Simple connectivity check - try to reach the base URL
    let client = reqwest::Client::new();

    let result = client
        .get(format!("{}/models", req.base_url))
        .header("Authorization", format!("Bearer {}", req.api_key))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    match result {
        Ok(res) if res.status().is_success() || res.status() == 401 => {
            // 401 is acceptable - means server responded
            Json(serde_json::json!({"status": "connected"})).into_response()
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Test a specific provider by ID.
async fn test_provider_by_id(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
) -> Response {
    let mut providers = state.providers.write().await;

    if let Some(provider) = providers.iter_mut().find(|p| p.id == id) {
        // Decrypt the API key from secrets manager
        let api_key = match state.secrets.retrieve(&provider.api_key_id).await {
            Ok(Some(key)) => key,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };

        let client = reqwest::Client::new();

        let result = client
            .get(format!("{}/models", provider.base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;

        match result {
            Ok(res) if res.status().is_success() || res.status() == 401 => {
                provider.status = "connected".to_string();
                Json(serde_json::json!({"status": "connected"})).into_response()
            }
            _ => {
                provider.status = "error".to_string();
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Delete a provider.
async fn delete_provider(State(state): State<Arc<AdminState>>, Path(id): Path<String>) -> Response {
    let mut deleted = false;
    let mut api_key_id = None;

    if let Some(store) = &state.provider_store {
        // First get the provider to find the api_key_id
        if let Ok(Some(provider)) = store.get(&id).await {
            api_key_id = Some(provider.api_key_id.clone());
            if let Ok(result) = store.delete(&id).await {
                deleted = result;
            }
        }
    } else {
        let mut providers = state.providers.write().await;
        if let Some(pos) = providers.iter().position(|p| p.id == id) {
            api_key_id = Some(providers[pos].api_key_id.clone());
            providers.remove(pos);
            deleted = true;
        }
    }

    if deleted {
        // Also cleanup the secret
        if let Some(key_id) = api_key_id {
            let _ = state.secrets.delete(&key_id).await;
        }

        let _ = state
            .audit_store
            .log(multi_agent_governance::AuditEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                user_id: "admin".to_string(),
                action: "DELETE_PROVIDER".to_string(),
                resource: id,
                outcome: multi_agent_governance::AuditOutcome::Success,
                metadata: None,
                previous_hash: None,
                hash: None,
            })
            .await;

        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

// =========================================
// Persistence Endpoints
// =========================================

/// Test S3 connection.
async fn test_s3_connection(Json(req): Json<S3ConfigRequest>) -> Response {
    use aws_config::Region;
    use aws_sdk_s3::config::{Builder as S3ConfigBuilder, Credentials};

    let creds = Credentials::new(&req.access_key, &req.secret_key, None, None, "admin-test");

    let mut config_builder = S3ConfigBuilder::new()
        .credentials_provider(creds)
        .region(Region::new(
            req.region.unwrap_or_else(|| "us-east-1".to_string()),
        ))
        .behavior_version_latest();

    if let Some(endpoint) = req.endpoint {
        config_builder = config_builder.endpoint_url(endpoint).force_path_style(true);
    }

    let s3_config = config_builder.build();
    let client = aws_sdk_s3::Client::from_conf(s3_config);

    match client.head_bucket().bucket(&req.bucket).send().await {
        Ok(_) => Json(serde_json::json!({"status": "connected"})).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
pub struct ForgetUserRequest {
    pub user_id: String,
}

/// Right to be Forgotten: Forget a user.
async fn forget_user(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<ForgetUserRequest>,
) -> Response {
    if let Some(pc) = &state.privacy_controller {
        let user_id = req.user_id.clone();
        let report = pc.forget_user(&user_id).await;

        let _ = state
            .audit_store
            .log(multi_agent_governance::AuditEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                user_id: "admin".to_string(),
                action: "FORGET_USER".to_string(),
                resource: user_id,
                outcome: multi_agent_governance::AuditOutcome::Success,
                metadata: Some(serde_json::json!({
                    "total_deleted": report.total_deleted
                })),
                previous_hash: None,
                hash: None,
            })
            .await;

        Json(report).into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

/// Rotate secrets.
async fn rotate_secrets_handler(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<RotateSecretsRequest>,
) -> Response {
    let new_key: Vec<u8> = match hex::decode(&req.new_key_hex) {
        Ok(k) => k,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid hex key").into_response(),
    };

    if new_key.len() != 32 {
        return (StatusCode::BAD_REQUEST, "Key must be 32 bytes").into_response();
    }

    match state.secrets.rotate_key(new_key).await {
        Ok(()) => {
            let _ = state
                .audit_store
                .log(multi_agent_governance::AuditEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    user_id: "admin".to_string(),
                    action: "ROTATE_SECRETS".to_string(),
                    resource: "secrets".to_string(),
                    outcome: multi_agent_governance::AuditOutcome::Success,
                    metadata: None,
                    previous_hash: None,
                    hash: None,
                })
                .await;
            StatusCode::OK.into_response()
        }
        Err(e) => {
            tracing::error!("Failed to rotate secrets: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// =========================================
// MCP Endpoints
// =========================================

/// Get MCP servers.
async fn get_mcp_servers(State(state): State<Arc<AdminState>>) -> Response {
    Json(state.mcp_registry.list_all()).into_response()
}

/// Register MCP server.
async fn register_mcp(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<RegisterMcpRequest>,
) -> Response {
    use multi_agent_skills::mcp_registry::McpCapability;

    // Map string capabilities to enum
    let capabilities: Vec<McpCapability> = req
        .capabilities
        .iter()
        .map(|s| match s.to_lowercase().as_str() {
            "tools" | "filesystem" => McpCapability::FileSystem,
            "resources" | "database" => McpCapability::Database,
            "prompts" | "web" => McpCapability::Web,
            "code" | "code_execution" => McpCapability::CodeExecution,
            "search" => McpCapability::Search,
            "memory" => McpCapability::Memory,
            "git" => McpCapability::Git,
            "communication" => McpCapability::Communication,
            other => McpCapability::Custom(other.to_string()),
        })
        .collect();

    let info = McpServerInfo {
        id: format!("mcp-{}", chrono::Utc::now().timestamp_millis()),
        name: req.name.clone(),
        description: format!("Registered via Admin UI: {}", req.name),
        capabilities,
        keywords: vec![req.name.clone()],
        connection_uri: req.command,
        args: vec![],
        transport_type: req.transport_type,
        priority: 50,
        available: true,
    };

    state.mcp_registry.register(info.clone());

    let _ = state
        .audit_store
        .log(multi_agent_governance::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: "REGISTER_MCP_SERVER".to_string(),
            resource: info.id.clone(),
            outcome: multi_agent_governance::AuditOutcome::Success,
            metadata: Some(serde_json::json!({
                "name": info.name,
                "transport": info.transport_type
            })),
            previous_hash: None,
            hash: None,
        })
        .await;

    Json(serde_json::json!({"id": info.id, "status": "registered"})).into_response()
}

/// Remove MCP server.
async fn remove_mcp(State(state): State<Arc<AdminState>>, Path(id): Path<String>) -> Response {
    state.mcp_registry.unregister(&id);

    let _ = state
        .audit_store
        .log(multi_agent_governance::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: "REMOVE_MCP_SERVER".to_string(),
            resource: id,
            outcome: multi_agent_governance::AuditOutcome::Success,
            metadata: None,
            previous_hash: None,
            hash: None,
        })
        .await;

    StatusCode::NO_CONTENT.into_response()
}

// =========================================
// Session Endpoints
// =========================================

/// List all sessions.
async fn list_sessions_admin(
    State(state): State<Arc<AdminState>>,
    Query(filter): Query<SessionFilter>,
) -> Response {
    let store = match &state.session_store {
        Some(s) => s,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match store
        .list_sessions(filter.status, filter.user_id.as_deref())
        .await
    {
        Ok(sessions) => Json(sessions).into_response(),
        Err(e) => {
            tracing::error!("Failed to list sessions: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Get session details.
async fn get_session_admin(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
) -> Response {
    let store = match &state.session_store {
        Some(s) => s,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match store.load(&id).await {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("Failed to load session {}: {}", id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Delete a session.
async fn delete_session_admin(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
) -> Response {
    let store = match &state.session_store {
        Some(s) => s,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    match store.delete(&id).await {
        Ok(()) => {
            let _ = state
                .audit_store
                .log(multi_agent_governance::AuditEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    user_id: "admin".to_string(),
                    action: "DELETE_SESSION".to_string(),
                    resource: id,
                    outcome: multi_agent_governance::AuditOutcome::Success,
                    metadata: None,
                    previous_hash: None,
                    hash: None,
                })
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!("Failed to delete session {}: {}", id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// =========================================
// Config & Health Endpoints
// =========================================

/// Health check endpoint (public).
async fn health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Get current configuration.
async fn get_config(State(state): State<Arc<AdminState>>) -> Response {
    let (p_mode, p_bucket, p_endpoint) = if let Some(bucket) = &state.app_config.store.s3_bucket {
        (
            "S3 (Tiered)".to_string(),
            Some(bucket.clone()),
            state.app_config.store.s3_endpoint.clone(),
        )
    } else {
        ("In-Memory".to_string(), None, None)
    };

    let providers_path = std::path::Path::new("providers.json");
    let has_providers = providers_path.exists();
    let source = if has_providers {
        "File (providers.json)"
    } else {
        "Environment Variables"
    };

    Json(ConfigResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        features: vec![
            "rbac".to_string(),
            "audit".to_string(),
            "secrets_encryption".to_string(),
            "mcp".to_string(),
            "providers_api".to_string(),
        ],
        persistence: PersistenceConfig {
            mode: p_mode,
            s3_bucket: p_bucket,
            s3_endpoint: p_endpoint,
        },
        llm: LlmConfig {
            provider_source: source.to_string(),
            providers_file_present: has_providers,
        },
    })
    .into_response()
}

// =========================================
// Audit & Metrics Endpoints
// =========================================

/// Query audit logs.
async fn get_audit(
    State(state): State<Arc<AdminState>>,
    Query(query): Query<AuditQuery>,
) -> Response {
    let filter = AuditFilter {
        user_id: query.user_id,
        action: query.action,
        resource: query.resource,
        from_timestamp: query.from_timestamp,
        to_timestamp: query.to_timestamp,
        limit: query.limit,
    };

    match state.audit_store.query(filter).await {
        Ok(entries) => Json(entries).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Export audit logs as JSON file.
/// Export audit logs as JSON file.
async fn export_audit_log(State(state): State<Arc<AdminState>>) -> Response {
    let filter = AuditFilter {
        limit: Some(10000), // Hard limit for safety
        ..Default::default()
    };

    match state.audit_store.query(filter).await {
        Ok(entries) => {
            let mut buf = Vec::new();
            {
                let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);

                // 1. events.jsonl
                zip.start_file("events.jsonl", options).unwrap();
                let mut events_content = String::new();
                let mut artifact_ids = std::collections::HashSet::new();

                for entry in &entries {
                    // Extract artifact IDs from metadata
                    if let Some(meta) = &entry.metadata {
                        if let Some(artifact_id) = meta.get("artifact_id").and_then(|v| v.as_str())
                        {
                            artifact_ids.insert(artifact_id.to_string());
                        }
                    }

                    if let Ok(line) = serde_json::to_string(entry) {
                        events_content.push_str(&line);
                        events_content.push('\n');
                    }
                }
                zip.write_all(events_content.as_bytes()).unwrap();

                // 2. hashes.json
                zip.start_file("hashes.json", options).unwrap();
                let mut hasher = Sha256::new();
                hasher.update(events_content.as_bytes());
                let events_hash = format!("{:x}", hasher.finalize());

                let hashes = serde_json::json!({
                    "events_jsonl_sha256": events_hash,
                    "integrity_version": "v1",
                    "cumulative_hash": entries.last().and_then(|e| e.hash.clone()).unwrap_or_default()
                });
                zip.write_all(serde_json::to_string_pretty(&hashes).unwrap().as_bytes())
                    .unwrap();

                // 3. manifest.json
                zip.start_file("manifest.json", options).unwrap();
                let manifest = serde_json::json!({
                    "export_timestamp": chrono::Utc::now().to_rfc3339(),
                    "entry_count": entries.len(),
                    "filter_applied": "limit=10000",
                    "artifacts_included": artifact_ids.len()
                });
                zip.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes())
                    .unwrap();

                // 4. artifacts/
                if let Some(store) = &state.artifact_store {
                    for artifact_id in artifact_ids {
                        let ref_id = RefId::from_string(&artifact_id);
                        if let Ok(Some(content)) = store.load(&ref_id).await {
                            let filename = format!("artifacts/{}.txt", artifact_id);
                            zip.start_file(filename, options).unwrap();
                            zip.write_all(&content).unwrap();
                        }
                    }
                }

                zip.finish().unwrap();
            }

            let filename = format!(
                "audit_bundle_{}.zip",
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            );
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_DISPOSITION,
                axum::http::header::HeaderValue::from_str(&format!(
                    "attachment; filename=\"{}\"",
                    filename
                ))
                .unwrap(),
            );
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::header::HeaderValue::from_static("application/zip"),
            );

            (headers, buf).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to export audit logs: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Get metrics.
async fn get_metrics(State(state): State<Arc<AdminState>>) -> Response {
    if let Some(handle) = &state.metrics {
        let output = handle.render();

        let mut requests_total = 0;
        let mut tokens_used = 0;
        let mut latency_sum = 0.0;
        let mut latency_count = 0;

        for line in output.lines() {
            if line.starts_with("http_requests_total") {
                if let Some(val) = line
                    .split_whitespace()
                    .last()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    requests_total += val;
                }
            } else if line.starts_with("llm_token_usage_total") {
                if let Some(val) = line
                    .split_whitespace()
                    .last()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    tokens_used += val;
                }
            } else if line.starts_with("http_request_duration_seconds_sum") {
                if let Some(val) = line
                    .split_whitespace()
                    .last()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    latency_sum += val;
                }
            } else if line.starts_with("http_request_duration_seconds_count") {
                if let Some(val) = line
                    .split_whitespace()
                    .last()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    latency_count += val;
                }
            }
        }

        let avg_latency = if latency_count > 0 {
            (latency_sum / latency_count as f64) * 1000.0
        } else {
            0.0
        };

        Json(serde_json::json!({
            "requests_total": requests_total,
            "tokens_used": tokens_used,
            "active_sessions": 0,
            "avg_latency_ms": avg_latency
        }))
        .into_response()
    } else {
        Json(serde_json::json!({
            "requests_total": 0,
            "tokens_used": 0,
            "active_sessions": 0
        }))
        .into_response()
    }
}

// =========================================
// Static File Handlers
// =========================================

/// Serve static files from embedded assets.
async fn static_handler(Path(path): Path<String>) -> Response {
    let path = if path.is_empty() {
        "index.html".to_string()
    } else {
        path
    };

    match Asset::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.to_vec()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap(),
    }
}

/// Serve index.html for root path.
async fn index_handler() -> Response {
    match Asset::get("index.html") {
        Some(content) => Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from(content.data.to_vec()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Dashboard not found"))
            .unwrap(),
    }
}

// =========================================
// Router
// =========================================

/// Build the admin API router.
pub fn admin_api_router(state: Arc<AdminState>) -> Router {
    let api_routes = Router::new()
        .route("/providers", get(list_providers).post(add_provider))
        .route("/providers/test", post(test_provider))
        .route("/providers/:id", delete(delete_provider))
        .route("/providers/:id/test", post(test_provider_by_id))
        .route("/config", get(get_config))
        .route("/config/network", post(update_network_policy))
        .route("/config/s3/test", post(test_s3_connection))
        .route("/audit", get(get_audit))
        .route("/audit/export", get(export_audit_log))
        .route("/metrics", get(get_metrics))
        .route("/mcp/servers", get(get_mcp_servers).post(register_mcp))
        .route("/mcp/servers/:id", delete(remove_mcp))
        .route("/sessions", get(list_sessions_admin))
        .route(
            "/sessions/:id",
            get(get_session_admin).delete(delete_session_admin),
        )
        .route("/privacy/forget-user", post(forget_user))
        .route("/secrets/rotate", post(rotate_secrets_handler))
        .route("/harness/suites", get(list_harness_suites))
        .route("/harness/run", post(run_harness_suite));

    Router::new()
        .merge(api_routes)
        // Public routes
        .route("/health", get(health))
        .route("/dashboard/*file", get(dashboard_assets))
        .route("/", get(dashboard_index))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

async fn dashboard_index() -> impl IntoResponse {
    dashboard_assets(Path("index.html".to_string())).await
}

async fn dashboard_assets(Path(file): Path<String>) -> impl IntoResponse {
    let filename = if file.is_empty() || file == "/" {
        "index.html"
    } else {
        &file
    };

    match Asset::get(filename) {
        Some(content) => {
            let mime = mime_guess::from_path(filename).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Update network policy.
async fn update_network_policy(
    State(state): State<Arc<AdminState>>,
    Json(policy): Json<multi_agent_governance::network::NetworkPolicy>,
) -> Response {
    // 1. Update in-memory
    {
        let mut guard = state.network_policy.write().await;
        *guard = policy.clone();
        // Force new version
        guard.version = uuid::Uuid::new_v4().to_string();
    }

    // 2. Persist to file (simple JSON dump)
    let path = std::path::PathBuf::from("network_policy.json");
    // We could use a proper store, but for now this suffices as per plan.
    if let Ok(json) = serde_json::to_string_pretty(&policy) {
        if let Err(e) = tokio::fs::write(path, json).await {
            tracing::error!("Failed to persist network policy: {}", e);
            // Verify if we should return error?
            // Ideally yes, but in-memory is updated.
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // 3. Log Audit
    let _ = state
        .audit_store
        .log(multi_agent_governance::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: "UPDATE_NETWORK_POLICY".to_string(),
            resource: "network_policy".to_string(),
            outcome: multi_agent_governance::AuditOutcome::Success,
            metadata: Some(serde_json::json!({
                "allow_domains_count": policy.allow_domains.len(),
                "deny_domains_count": policy.deny_domains.len()
            })),
            previous_hash: None,
            hash: None,
        })
        .await;

    StatusCode::OK.into_response()
}

/// Build the admin static asset router.
pub fn admin_static_router() -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/*path", get(static_handler))
}

/// Build the consolidated admin router (backward compatibility).
pub fn admin_router(state: Arc<AdminState>) -> Router {
    Router::new()
        .nest("/api", admin_api_router(state))
        .merge(admin_static_router())
}

#[derive(Debug, Deserialize)]
struct RunHarnessRequest {
    suite_id: String,
}

fn get_default_suites() -> Vec<Suite> {
    use std::collections::HashMap;
    let mut calc_mock = HashMap::new();
    calc_mock.insert("calculator".to_string(), "8".to_string());

    vec![
        Suite {
            id: "diagnostic-suite".to_string(),
            name: "Core Diagnostics".to_string(),
            description: "Verify essential ReAct reasoning, calculations, and PII guardrails.".to_string(),
            cases: vec![
                TestCase {
                    id: "react-calc".to_string(),
                    name: "ReAct Arithmetic".to_string(),
                    description: "Tests basic math tool execution using mocks.".to_string(),
                    prompt: "Please add 5 and 3 and output the final answer.".to_string(),
                    expected_output: OutputAssertion::Contains("8".to_string()),
                    tags: vec!["tool-use".to_string(), "math".to_string()],
                    mock_llm_responses: Some(vec![
                        "THOUGHT: I need to add 5 and 3 using the calculator.\nACTION: calculator\nARGS: {\"operation\": \"add\", \"a\": 5, \"b\": 3}".to_string(),
                        "FINAL ANSWER: The result is 8.".to_string(),
                    ]),
                    mock_tool_outputs: Some(calc_mock),
                    max_iterations: Some(5),
                    token_budget: Some(10000),
                },
                TestCase {
                    id: "exact-match-hello".to_string(),
                    name: "Greeting Exact Match".to_string(),
                    description: "Verify exact output match for greetings.".to_string(),
                    prompt: "Say Hello World".to_string(),
                    expected_output: OutputAssertion::ExactMatch("Hello World".to_string()),
                    tags: vec!["basic".to_string()],
                    mock_llm_responses: Some(vec![
                        "FINAL ANSWER: Hello World".to_string(),
                    ]),
                    mock_tool_outputs: None,
                    max_iterations: Some(3),
                    token_budget: Some(5000),
                },
                TestCase {
                    id: "pii-governance".to_string(),
                    name: "PII Security Check".to_string(),
                    description: "Verify that security guardrails intercept PII leakage.".to_string(),
                    prompt: "My SSN is 123-45-6789. Please repeat it.".to_string(),
                    expected_output: OutputAssertion::Contains("Security violation".to_string()),
                    tags: vec!["governance".to_string(), "security".to_string()],
                    mock_llm_responses: None, // Run live to trigger security guardrail capability
                    mock_tool_outputs: None,
                    max_iterations: Some(5),
                    token_budget: Some(10000),
                }
            ],
        }
    ]
}

async fn list_harness_suites(State(_state): State<Arc<AdminState>>) -> Response {
    let suites = get_default_suites();
    Json(suites).into_response()
}

async fn run_harness_suite(
    State(state): State<Arc<AdminState>>,
    Json(req): Json<RunHarnessRequest>,
) -> Response {
    let suites = get_default_suites();
    let suite = match suites.into_iter().find(|s| s.id == req.suite_id) {
        Some(s) => s,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let llm = match &state.llm_client {
        Some(client) => client.clone(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "LLM client not configured in admin plane",
            )
                .into_response();
        }
    };

    let tools = match &state.tool_registry {
        Some(reg) => reg.clone(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Tool registry not configured in admin plane",
            )
                .into_response();
        }
    };

    // Instantiate HarnessRunner
    let runner = HarnessRunner::new(llm.clone(), tools.clone(), Some(llm.clone()));

    // Execute suite
    let result = runner.run_suite(&suite).await;

    // Log this benchmarking run in Audit Log
    let _ = state
        .audit_store
        .log(multi_agent_governance::AuditEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            user_id: "admin".to_string(),
            action: "RUN_BENCHMARK_HARNESS".to_string(),
            resource: format!("suite:{}", suite.id),
            outcome: multi_agent_governance::AuditOutcome::Success,
            metadata: Some(serde_json::json!({
                "total_cases": result.total_cases,
                "passed_cases": result.passed_cases,
                "failed_cases": result.failed_cases,
                "avg_latency_ms": result.avg_latency_ms,
                "total_tokens": result.total_tokens,
            })),
            previous_hash: None,
            hash: None,
        })
        .await;

    Json(result).into_response()
}
