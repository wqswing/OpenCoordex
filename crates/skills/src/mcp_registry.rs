//! MCP Registry Center for Autonomous Server Selection.
//!
//! Provides a centralized registry of MCP servers with capability metadata,
//! allowing agents to autonomously discover and select the appropriate
//! MCP server for their specific task requirements.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::mcp_adapter::{McpToolAdapter, McpTransport};
use multi_agent_core::{Error, Result};

/// Capability category for MCP servers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum McpCapability {
    /// File system operations (read, write, list).
    FileSystem,
    /// Database operations (SQL, queries).
    Database,
    /// Web/HTTP operations (fetch, scrape).
    Web,
    /// Code execution/REPL.
    CodeExecution,
    /// Search (semantic, keyword).
    Search,
    /// Memory/Knowledge base.
    Memory,
    /// Git/version control.
    Git,
    /// Email/messaging.
    Communication,
    /// Custom capability.
    Custom(String),
}

impl std::fmt::Display for McpCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileSystem => write!(f, "filesystem"),
            Self::Database => write!(f, "database"),
            Self::Web => write!(f, "web"),
            Self::CodeExecution => write!(f, "code_execution"),
            Self::Search => write!(f, "search"),
            Self::Memory => write!(f, "memory"),
            Self::Git => write!(f, "git"),
            Self::Communication => write!(f, "communication"),
            Self::Custom(s) => write!(f, "{}", s),
        }
    }
}

/// Information about a registered MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    /// Unique server identifier.
    pub id: String,
    /// Human-readable server name.
    pub name: String,
    /// Server description.
    pub description: String,
    /// Capabilities provided by this server.
    pub capabilities: Vec<McpCapability>,
    /// Keywords for semantic matching.
    pub keywords: Vec<String>,
    /// Connection URL or command.
    pub connection_uri: String,
    /// Command arguments (for stdio transport).
    pub args: Vec<String>,
    /// Transport type (stdio, sse, websocket).
    /// Transport type (stdio, sse, websocket).
    pub transport_type: String,
    /// Priority (higher = preferred).
    pub priority: u8,
    /// Whether the server is currently available.
    pub available: bool,
}

impl McpServerInfo {
    /// Create a new MCP server info entry.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            capabilities: Vec::new(),
            keywords: Vec::new(),
            connection_uri: String::new(),
            args: Vec::new(),
            transport_type: "stdio".to_string(),
            priority: 5,
            available: true,
        }
    }

    /// Set description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add capabilities.
    pub fn with_capabilities(mut self, caps: Vec<McpCapability>) -> Self {
        self.capabilities = caps;
        self
    }

    /// Add keywords.
    pub fn with_keywords(mut self, keywords: Vec<&str>) -> Self {
        self.keywords = keywords.into_iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set connection URI.
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.connection_uri = uri.into();
        self
    }

    /// Set command arguments.
    pub fn with_args(mut self, args: Vec<&str>) -> Self {
        self.args = args.into_iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set transport type.
    pub fn with_transport(mut self, transport: impl Into<String>) -> Self {
        self.transport_type = transport.into();
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Check if server has a specific capability.
    pub fn has_capability(&self, cap: &McpCapability) -> bool {
        self.capabilities.contains(cap)
    }

    /// Check if server matches a keyword (case-insensitive).
    pub fn matches_keyword(&self, keyword: &str) -> bool {
        let kw_lower = keyword.to_lowercase();
        self.keywords
            .iter()
            .any(|k| k.to_lowercase().contains(&kw_lower))
            || self.name.to_lowercase().contains(&kw_lower)
            || self.description.to_lowercase().contains(&kw_lower)
    }
}

// v0.3: Registry Unification
use async_trait::async_trait;
use multi_agent_core::traits::{Tool, ToolRegistry};
use multi_agent_core::types::{ToolDefinition, ToolOutput};
use serde_json::Value;

#[async_trait]
impl ToolRegistry for McpRegistry {
    async fn register(&self, _tool: Box<dyn Tool>) -> Result<()> {
        Err(Error::McpAdapter(
            "Cannot register local tools directly to McpRegistry. Use register_server instead."
                .to_string(),
        ))
    }

    async fn get(&self, name: &str) -> Result<Option<Box<dyn Tool>>> {
        // Delegate to adapter to find tool definition
        if let Some(def) = self.adapter.get_tool_definition(name).await? {
            let tool = crate::mcp_adapter::McpToolWrapper {
                adapter: self.adapter.clone(),
                name: def.name.clone(),
                description: def.description.clone(),
                parameters: def.parameters.clone(),
            };
            return Ok(Some(Box::new(tool)));
        }
        Ok(None)
    }

    async fn list(&self) -> Result<Vec<ToolDefinition>> {
        self.adapter.list_tools().await
    }

    async fn execute(&self, name: &str, args: Value) -> Result<ToolOutput> {
        // The adapter handles finding which server owns the tool
        self.adapter.call_tool(name, args).await
    }
}

/// MCP Registry Center for managing and selecting MCP servers.
pub struct McpRegistry {
    /// Registered servers.
    servers: DashMap<String, McpServerInfo>,
    /// MCP adapter for actual connections.
    adapter: Arc<McpToolAdapter>,
}

impl McpRegistry {
    /// Create a new MCP registry.
    pub fn new() -> Self {
        Self {
            servers: DashMap::new(),
            adapter: Arc::new(McpToolAdapter::new()),
        }
    }

    /// Create with a shared MCP adapter.
    pub fn with_adapter(adapter: Arc<McpToolAdapter>) -> Self {
        Self {
            servers: DashMap::new(),
            adapter,
        }
    }

    /// Register an MCP server.
    pub fn register(&self, server: McpServerInfo) {
        tracing::info!(id = %server.id, name = %server.name, "Registering MCP server");
        self.servers.insert(server.id.clone(), server);
    }

    /// Unregister an MCP server.
    pub fn unregister(&self, id: &str) -> Option<McpServerInfo> {
        tracing::info!(id = %id, "Unregistering MCP server");
        // TODO: In a real implementation, we should also signal the adapter to disconnect.
        // For now, removing from registry prevents future selection.
        self.servers.remove(id).map(|(_, v)| v)
    }

    /// Check if a server is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.servers.contains_key(id)
    }

    /// List all registered servers.
    pub fn list_all(&self) -> Vec<McpServerInfo> {
        self.servers.iter().map(|e| e.value().clone()).collect()
    }

    /// Find servers by capability.
    pub fn find_by_capability(&self, capability: &McpCapability) -> Vec<McpServerInfo> {
        self.servers
            .iter()
            .filter(|e| e.value().has_capability(capability) && e.value().available)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Find servers by keyword.
    pub fn find_by_keyword(&self, keyword: &str) -> Vec<McpServerInfo> {
        self.servers
            .iter()
            .filter(|e| e.value().matches_keyword(keyword) && e.value().available)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Autonomously select the best MCP server for a task.
    ///
    /// Agents can call this to find the most suitable MCP server
    /// based on the task description.
    pub fn select_for_task(&self, task_description: &str) -> Option<McpServerInfo> {
        let desc_lower = task_description.to_lowercase();

        // Score each server based on keyword matches
        let mut scored: Vec<(McpServerInfo, u32)> = self
            .servers
            .iter()
            .filter(|e| e.value().available)
            .map(|e| {
                let server = e.value().clone();
                let mut score: u32 = 0;

                // Check capability hints in task
                if (desc_lower.contains("file")
                    || desc_lower.contains("read")
                    || desc_lower.contains("write"))
                    && server.has_capability(&McpCapability::FileSystem)
                {
                    score += 10;
                }
                if (desc_lower.contains("database")
                    || desc_lower.contains("sql")
                    || desc_lower.contains("query"))
                    && server.has_capability(&McpCapability::Database)
                {
                    score += 10;
                }
                if (desc_lower.contains("web")
                    || desc_lower.contains("http")
                    || desc_lower.contains("fetch")
                    || desc_lower.contains("scrape"))
                    && server.has_capability(&McpCapability::Web)
                {
                    score += 10;
                }
                if (desc_lower.contains("code")
                    || desc_lower.contains("run")
                    || desc_lower.contains("execute")
                    || desc_lower.contains("python"))
                    && server.has_capability(&McpCapability::CodeExecution)
                {
                    score += 10;
                }
                if (desc_lower.contains("search") || desc_lower.contains("find"))
                    && server.has_capability(&McpCapability::Search)
                {
                    score += 10;
                }
                if (desc_lower.contains("git")
                    || desc_lower.contains("commit")
                    || desc_lower.contains("branch"))
                    && server.has_capability(&McpCapability::Git)
                {
                    score += 10;
                }
                if (desc_lower.contains("remember")
                    || desc_lower.contains("memory")
                    || desc_lower.contains("store"))
                    && server.has_capability(&McpCapability::Memory)
                {
                    score += 10;
                }

                // Keyword matching
                for keyword in &server.keywords {
                    if desc_lower.contains(&keyword.to_lowercase()) {
                        score += 5;
                    }
                }

                // Priority bonus
                score += server.priority as u32;

                (server, score)
            })
            .collect();

        // Sort by score descending
        scored.sort_by_key(|b| std::cmp::Reverse(b.1));

        scored
            .into_iter()
            .next()
            .filter(|(_, score)| *score > 0)
            .map(|(s, _)| s)
    }

    /// Connect to the selected server via the adapter.
    pub async fn connect_server(&self, server_id: &str) -> Result<()> {
        let server = self.servers.get(server_id).ok_or_else(|| {
            Error::mcp_adapter(format!("Server '{}' not found in registry", server_id))
        })?;

        let transport = match server.transport_type.as_str() {
            "stdio" => McpTransport::Stdio {
                command: server.connection_uri.clone(),
                args: server.args.clone(),
            },
            "sse" => McpTransport::Sse {
                url: server.connection_uri.clone(),
            },
            "websocket" => McpTransport::WebSocket {
                url: server.connection_uri.clone(),
            },
            _ => {
                return Err(Error::mcp_adapter(format!(
                    "Unknown transport type: {}",
                    server.transport_type
                )))
            }
        };

        self.adapter.connect(&server.id, transport).await
    }

    /// Get the underlying MCP adapter.
    pub fn adapter(&self) -> Arc<McpToolAdapter> {
        self.adapter.clone()
    }

    /// Register default/common MCP servers.
    pub fn register_defaults(&self) {
        // Filesystem server
        self.register(
            McpServerInfo::new("mcp-filesystem", "Filesystem Server")
                .with_description("Read, write, and manage files on the local filesystem")
                .with_capabilities(vec![McpCapability::FileSystem])
                .with_keywords(vec!["file", "read", "write", "directory", "path", "folder"])
                .with_uri("npx")
                .with_args(vec![
                    "-y",
                    "@modelcontextprotocol/server-filesystem",
                    "/tmp",
                ])
                .with_transport("stdio")
                .with_priority(8),
        );

        // SQLite server
        self.register(
            McpServerInfo::new("mcp-sqlite", "SQLite Database Server")
                .with_description("Execute SQL queries on SQLite databases")
                .with_capabilities(vec![McpCapability::Database])
                .with_keywords(vec![
                    "sql", "database", "query", "sqlite", "table", "select",
                ])
                .with_uri("npx")
                .with_args(vec!["-y", "@modelcontextprotocol/server-sqlite"])
                .with_transport("stdio")
                .with_priority(8),
        );

        // Fetch/Web server
        self.register(
            McpServerInfo::new("mcp-fetch", "Web Fetch Server")
                .with_description("Fetch content from URLs and web pages")
                .with_capabilities(vec![McpCapability::Web])
                .with_keywords(vec!["http", "url", "fetch", "web", "download", "api"])
                .with_uri("npx")
                .with_args(vec!["-y", "@modelcontextprotocol/server-fetch"])
                .with_transport("stdio")
                .with_priority(7),
        );

        // Memory server
        self.register(
            McpServerInfo::new("mcp-memory", "Memory Server")
                .with_description("Store and retrieve knowledge and memories")
                .with_capabilities(vec![McpCapability::Memory, McpCapability::Search])
                .with_keywords(vec!["remember", "memory", "store", "retrieve", "knowledge"])
                .with_uri("npx")
                .with_args(vec!["-y", "@modelcontextprotocol/server-memory"])
                .with_transport("stdio")
                .with_priority(6),
        );

        // Git server
        self.register(
            McpServerInfo::new("mcp-git", "Git Server")
                .with_description("Perform Git operations on repositories")
                .with_capabilities(vec![McpCapability::Git])
                .with_keywords(vec![
                    "git",
                    "commit",
                    "branch",
                    "push",
                    "pull",
                    "repository",
                ])
                .with_uri("npx")
                .with_args(vec!["-y", "@modelcontextprotocol/server-git"])
                .with_transport("stdio")
                .with_priority(6),
        );
    }
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_find() {
        let registry = McpRegistry::new();

        registry.register(
            McpServerInfo::new("test-fs", "Test FS")
                .with_capabilities(vec![McpCapability::FileSystem])
                .with_keywords(vec!["file", "read"]),
        );

        let found = registry.find_by_capability(&McpCapability::FileSystem);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "test-fs");
    }

    #[test]
    fn test_select_for_task() {
        let registry = McpRegistry::new();
        registry.register_defaults();

        // Task mentioning files
        let selected = registry.select_for_task("Read the contents of config.json file");
        assert!(selected.is_some());
        assert!(selected.unwrap().has_capability(&McpCapability::FileSystem));

        // Task mentioning database
        let selected = registry.select_for_task("Query the users table in the database");
        assert!(selected.is_some());
        assert!(selected.unwrap().has_capability(&McpCapability::Database));

        // Task mentioning web
        let selected = registry.select_for_task("Fetch the homepage from https://example.com");
        assert!(selected.is_some());
        assert!(selected.unwrap().has_capability(&McpCapability::Web));
    }

    #[test]
    fn test_keyword_matching() {
        let server =
            McpServerInfo::new("test", "Test Server").with_keywords(vec!["file", "document"]);

        assert!(server.matches_keyword("file"));
        assert!(server.matches_keyword("DOCUMENT"));
        assert!(!server.matches_keyword("database"));
    }
}
