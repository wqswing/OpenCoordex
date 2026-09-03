use crate::manifest::PluginManifest;
use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

/// Key-value store for plugin state (enabled/disabled).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PluginStateStore {
    /// Map of plugin ID to enabled status.
    enabled_plugins: std::collections::HashMap<String, bool>,
}

/// Manages the lifecycle of plugins (install, enable, disable).
pub struct PluginManager {
    /// Directory where plugins are installed.
    plugins_dir: PathBuf,
    /// Path to the state file (plugins.json).
    state_file: PathBuf,
    /// In-memory cache of loaded manifests.
    manifests: DashMap<String, PluginManifest>,
    /// In-memory cache of enabled state.
    enabled_state: DashMap<String, bool>,
    /// Event emitter for lifecycle events.
    event_emitter: Option<Arc<dyn multi_agent_core::traits::EventEmitter>>,
}

impl PluginManager {
    /// Create a new PluginManager.
    pub fn new(plugins_dir: impl AsRef<Path>, state_file: impl AsRef<Path>) -> Self {
        Self {
            plugins_dir: plugins_dir.as_ref().to_path_buf(),
            state_file: state_file.as_ref().to_path_buf(),
            manifests: DashMap::new(),
            enabled_state: DashMap::new(),
            event_emitter: None,
        }
    }

    /// Set the event emitter.
    pub fn with_event_emitter(
        mut self,
        emitter: Arc<dyn multi_agent_core::traits::EventEmitter>,
    ) -> Self {
        self.event_emitter = Some(emitter);
        self
    }

    /// Initialize by loading manifests and state.
    pub async fn initialize(&self) -> Result<()> {
        // 1. Ensure directories exist
        if !self.plugins_dir.exists() {
            fs::create_dir_all(&self.plugins_dir)
                .await
                .context("Failed to create plugins directory")?;
        }
        if let Some(parent) = self.state_file.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .await
                    .context("Failed to create state file directory")?;
            }
        }

        // 2. Load state
        self.load_state().await?;

        // 3. Scan for plugins
        self.scan_plugins().await?;

        Ok(())
    }

    /// Install a plugin from a source directory (copies it).
    pub async fn install(&self, source_path: impl AsRef<Path>) -> Result<String> {
        let manifest_path = source_path.as_ref().join("manifest.yaml");
        let manifest = PluginManifest::load(&manifest_path)?;
        manifest
            .validate_for_runtime(env!("CARGO_PKG_VERSION"))
            .context("Plugin manifest runtime validation failed")?;

        let target_dir = self.plugins_dir.join(&manifest.id);
        if target_dir.exists() {
            // Simple overwrite for now
            fs::remove_dir_all(&target_dir).await?;
        }

        // Copy directory using a recursive helper or external command?
        // For simplicity in MVP, let's assume flat structure or use a crate if needed.
        // But standard fs doesn't have recursive copy.
        // We'll implement a simple recursive copy.
        copy_dir_recursive(source_path.as_ref(), &target_dir).await?;

        // Update in-memory
        self.manifests.insert(manifest.id.clone(), manifest.clone());

        // Default to disabled on install? Or enabled? Let's say disabled.
        self.enabled_state.insert(manifest.id.clone(), false);
        self.save_state().await?;

        self.emit_event(
            "PLUGIN_INSTALLED",
            &manifest.id,
            serde_json::json!({
                "version": manifest.version
            }),
        )
        .await;

        Ok(manifest.id)
    }

    /// Enable a plugin.
    pub async fn enable(&self, plugin_id: &str) -> Result<()> {
        if !self.manifests.contains_key(plugin_id) {
            return Err(anyhow!("Plugin not found: {}", plugin_id));
        }

        self.enabled_state.insert(plugin_id.to_string(), true);
        self.save_state().await?;

        self.emit_event("PLUGIN_ENABLED", plugin_id, serde_json::json!({}))
            .await;
        Ok(())
    }

    /// Disable a plugin.
    pub async fn disable(&self, plugin_id: &str) -> Result<()> {
        if !self.manifests.contains_key(plugin_id) {
            return Err(anyhow!("Plugin not found: {}", plugin_id));
        }

        self.enabled_state.insert(plugin_id.to_string(), false);
        self.save_state().await?;

        self.emit_event("PLUGIN_DISABLED", plugin_id, serde_json::json!({}))
            .await;
        Ok(())
    }

    /// List all plugins.
    pub fn list(&self) -> Vec<(PluginManifest, bool)> {
        let mut result = Vec::new();
        for entry in &self.manifests {
            let enabled = self
                .enabled_state
                .get(entry.key())
                .map(|r| *r.value())
                .unwrap_or(false);
            result.push((entry.value().clone(), enabled));
        }
        result
    }

    /// Get a specific plugin manifest.
    pub fn get(&self, plugin_id: &str) -> Option<PluginManifest> {
        self.manifests.get(plugin_id).map(|m| m.value().clone())
    }

    /// Check if a plugin is enabled.
    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.enabled_state
            .get(plugin_id)
            .map(|r| *r.value())
            .unwrap_or(false)
    }

    /// Sync enabled plugins with the McpRegistry.
    pub async fn sync_registry(&self, registry: &multi_agent_skills::McpRegistry) {
        for entry in &self.manifests {
            let manifest = entry.value();
            let enabled = self
                .enabled_state
                .get(&manifest.id)
                .map(|r| *r.value())
                .unwrap_or(false);

            if enabled {
                if !registry.contains(&manifest.id) {
                    // Convert manifest to McpServerInfo
                    let mut server_info = multi_agent_skills::mcp_registry::McpServerInfo::new(
                        &manifest.id,
                        &manifest.name,
                    )
                    .with_description(&manifest.description)
                    .with_transport(&manifest.transport.r#type);

                    if let Some(cmd) = &manifest.transport.command {
                        server_info = server_info.with_uri(cmd);
                    } else if let Some(url) = &manifest.transport.url {
                        server_info = server_info.with_uri(url);
                    }

                    if !manifest.transport.args.is_empty() {
                        server_info = server_info.with_args(
                            manifest.transport.args.iter().map(|s| s.as_str()).collect(),
                        );
                    }

                    // Map capabilities string to enum (simplified for now)
                    let caps = manifest
                        .capabilities
                        .iter()
                        .map(|c| match c.as_str() {
                            "filesystem" => {
                                multi_agent_skills::mcp_registry::McpCapability::FileSystem
                            }
                            "database" => multi_agent_skills::mcp_registry::McpCapability::Database,
                            "web" => multi_agent_skills::mcp_registry::McpCapability::Web,
                            "code_execution" => {
                                multi_agent_skills::mcp_registry::McpCapability::CodeExecution
                            }
                            "search" => multi_agent_skills::mcp_registry::McpCapability::Search,
                            "memory" => multi_agent_skills::mcp_registry::McpCapability::Memory,
                            "git" => multi_agent_skills::mcp_registry::McpCapability::Git,
                            "communication" => {
                                multi_agent_skills::mcp_registry::McpCapability::Communication
                            }
                            _ => multi_agent_skills::mcp_registry::McpCapability::Custom(c.clone()),
                        })
                        .collect();
                    server_info = server_info.with_capabilities(caps);

                    registry.register(server_info);
                }
            } else if registry.contains(&manifest.id) {
                registry.unregister(&manifest.id).await;
            }
        }
    }

    // --- Helpers ---

    async fn load_state(&self) -> Result<()> {
        if self.state_file.exists() {
            let content = fs::read_to_string(&self.state_file).await?;
            let state: PluginStateStore = serde_json::from_str(&content).unwrap_or_default(); // Fallback to empty on error for resilience

            for (id, enabled) in state.enabled_plugins {
                self.enabled_state.insert(id, enabled);
            }
        }
        Ok(())
    }

    async fn save_state(&self) -> Result<()> {
        let mut enabled_map = std::collections::HashMap::new();
        for entry in &self.enabled_state {
            enabled_map.insert(entry.key().clone(), *entry.value());
        }
        let state = PluginStateStore {
            enabled_plugins: enabled_map,
        };
        let content = serde_json::to_string_pretty(&state)?;
        fs::write(&self.state_file, content)
            .await
            .context("Failed to save plugin state")?;
        Ok(())
    }

    async fn scan_plugins(&self) -> Result<()> {
        let mut entries = fs::read_dir(&self.plugins_dir).await?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await?.is_dir() {
                let manifest_path = entry.path().join("manifest.yaml");
                if manifest_path.exists() {
                    match PluginManifest::load(&manifest_path) {
                        Ok(manifest) => {
                            if let Err(e) = manifest.validate_for_runtime(env!("CARGO_PKG_VERSION"))
                            {
                                tracing::warn!(
                                    "Skipping incompatible plugin manifest in {:?}: {}",
                                    entry.path(),
                                    e
                                );
                                continue;
                            }
                            self.manifests.insert(manifest.id.clone(), manifest);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load manifest in {:?}: {}", entry.path(), e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn emit_event(&self, event_type: &str, plugin_id: &str, mut payload: serde_json::Value) {
        if let Some(emitter) = &self.event_emitter {
            use multi_agent_core::events::{EventEnvelope, EventType};

            // Inject plugin_id into payload
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "plugin_id".to_string(),
                    serde_json::Value::String(plugin_id.to_string()),
                );
            }

            // Map string to EventType (assuming we add PLUGIN_* types later or use Other)
            let et = match event_type {
                "PLUGIN_INSTALLED" => EventType::Other("PLUGIN_INSTALLED".into()),
                "PLUGIN_ENABLED" => EventType::Other("PLUGIN_ENABLED".into()),
                "PLUGIN_DISABLED" => EventType::Other("PLUGIN_DISABLED".into()),
                _ => EventType::Other(event_type.into()),
            };

            let mut event = EventEnvelope::new(et, payload);
            event.actor = "system".to_string(); // or admin

            emitter.emit(event).await;
        }
    }
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst).await?;
    }
    let mut entries = fs::read_dir(src).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let ty = entry.file_type().await?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            fs::copy(&src_path, &dst_path).await?;
        }
    }
    Ok(())
}
