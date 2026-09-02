//! Sandbox tools implementing the `Tool` trait.
//!
//! These tools are registered in the L2 Skills layer and allow the agent
//! to execute code, read/write files in an isolated Docker sandbox.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use multi_agent_core::{traits::Tool, types::ToolOutput, Result};

use crate::engine::{SandboxConfig, SandboxEngine, SandboxId};

// =============================================================================
// Sandbox Manager
// =============================================================================

/// Manages sandbox lifecycle and provides tools to the agent.
///
/// Holds a reference to the sandbox engine and the active sandbox ID.
/// On first use, creates a sandbox lazily. On drop, destroys it.
pub struct SandboxManager {
    engine: Arc<dyn SandboxEngine>,
    config: SandboxConfig,
    active_sandbox: tokio::sync::RwLock<Option<SandboxId>>,
    event_emitter: Option<Arc<dyn multi_agent_core::traits::EventEmitter>>,
    active_constraints: std::sync::RwLock<Vec<String>>,
}

impl SandboxManager {
    /// Create a new sandbox manager.
    pub fn new(engine: Arc<dyn SandboxEngine>, config: SandboxConfig) -> Self {
        Self {
            engine,
            config,
            active_sandbox: tokio::sync::RwLock::new(None),
            event_emitter: None,
            active_constraints: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Set an event emitter for auditing.
    pub fn with_event_emitter(
        mut self,
        emitter: Arc<dyn multi_agent_core::traits::EventEmitter>,
    ) -> Self {
        self.event_emitter = Some(emitter);
        self
    }

    /// Get or create the active sandbox.
    pub async fn get_or_create(&self) -> Result<SandboxId> {
        // Fast path: check if sandbox exists
        {
            let guard = self.active_sandbox.read().await;
            if let Some(ref id) = *guard {
                return Ok(id.clone());
            }
        }

        // Slow path: create a new sandbox
        let mut guard = self.active_sandbox.write().await;
        // Double-check after acquiring write lock
        if let Some(ref id) = *guard {
            return Ok(id.clone());
        }

        let id = self.engine.create(&self.config).await?;
        *guard = Some(id.clone());
        Ok(id)
    }

    /// Destroy the active sandbox.
    pub async fn teardown(&self) -> Result<()> {
        let mut guard = self.active_sandbox.write().await;
        if let Some(id) = guard.take() {
            self.engine.destroy(&id).await?;
        }
        Ok(())
    }

    /// Get a reference to the sandbox engine.
    pub fn engine(&self) -> &Arc<dyn SandboxEngine> {
        &self.engine
    }

    /// Check if the sandbox backend is available.
    pub async fn is_available(&self) -> bool {
        self.engine.is_available().await
    }

    /// Get active constraints.
    pub fn get_constraints(&self) -> Vec<String> {
        self.active_constraints.read().unwrap().clone()
    }
}

impl multi_agent_core::traits::ConstraintListener for SandboxManager {
    fn update_constraints(&self, constraints: Vec<String>) {
        if let Ok(mut guard) = self.active_constraints.write() {
            *guard = constraints;
        }
    }
}

// =============================================================================
// Sandbox Shell Tool
// =============================================================================

/// Tool for executing shell commands inside the sandbox.
///
/// Risk level: HIGH — requires human approval when HITL is enabled.
pub struct SandboxShellTool {
    manager: Arc<SandboxManager>,
}

impl SandboxShellTool {
    /// Create a new sandbox shell tool.
    pub fn new(manager: Arc<SandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SandboxShellTool {
    fn name(&self) -> &str {
        "sandbox_shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command inside an isolated Docker sandbox. \
         The sandbox has no access to the host system. \
         Commands run as a non-root user in /workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 300)",
                    "default": 30
                }
            },
            "required": ["command"]
        })
    }

    fn risk_level(&self) -> multi_agent_core::types::ToolRiskLevel {
        multi_agent_core::types::ToolRiskLevel::High
    }

    async fn execute(&self, args: Value) -> Result<ToolOutput> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| multi_agent_core::Error::invalid_request("command is required"))?;

        // Check active constraints
        let constraints = self.manager.get_constraints();
        for c in &constraints {
            let lower_c = c.to_lowercase();
            if lower_c.contains("no python") && command.to_lowercase().contains("python") {
                return Err(multi_agent_core::Error::SecurityViolation(format!(
                    "Command blocked by active workspace constraint: {}",
                    c
                )));
            }
            if lower_c.contains("read-only") || lower_c.contains("no write") {
                let lower_cmd = command.to_lowercase();
                if lower_cmd.contains("echo ") && lower_cmd.contains(">")
                    || lower_cmd.contains("touch ")
                    || lower_cmd.contains("rm ")
                    || lower_cmd.contains("mkdir ")
                {
                    return Err(multi_agent_core::Error::SecurityViolation(format!(
                        "Write command blocked by read-only workspace constraint: {}",
                        c
                    )));
                }
            }
        }

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(300); // cap at 5 minutes

        let timeout = Duration::from_secs(timeout_secs);

        let sandbox_id = self.manager.get_or_create().await?;
        let result = self
            .manager
            .engine()
            .exec(&sandbox_id, command, timeout)
            .await?;

        if result.timed_out {
            return Ok(ToolOutput::error(format!(
                "Command timed out after {}s.\nPartial stdout:\n{}\nStderr:\n{}",
                timeout_secs, result.stdout, result.stderr
            )));
        }

        let mut output = String::new();
        if !result.stdout.is_empty() {
            output.push_str(&result.stdout);
        }
        if !result.stderr.is_empty() {
            if !output.is_empty() {
                output.push_str("\n--- stderr ---\n");
            }
            output.push_str(&result.stderr);
        }
        if output.is_empty() {
            output = format!("Command completed with exit code {}", result.exit_code);
        }

        if result.success() {
            Ok(ToolOutput::text(output).with_data(json!({
                "exit_code": result.exit_code,
                "timed_out": false,
            })))
        } else {
            Ok(ToolOutput::error(format!(
                "Command failed (exit code {}):\n{}",
                result.exit_code, output
            ))
            .with_data(json!({
                "exit_code": result.exit_code,
                "timed_out": false,
            })))
        }
    }
}

// =============================================================================
// Sandbox Write File Tool
// =============================================================================

/// Tool for writing files into the sandbox's /workspace.
///
/// Risk level: MEDIUM.
pub struct SandboxWriteFileTool {
    manager: Arc<SandboxManager>,
}

impl SandboxWriteFileTool {
    /// Create a new sandbox write file tool.
    pub fn new(manager: Arc<SandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SandboxWriteFileTool {
    fn name(&self) -> &str {
        "sandbox_write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file inside the isolated sandbox at /workspace. \
         Path is relative to /workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to /workspace (e.g. 'main.py', 'src/app.js')"
                },
                "content": {
                    "type": "string",
                    "description": "The file content to write"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn risk_level(&self) -> multi_agent_core::types::ToolRiskLevel {
        multi_agent_core::types::ToolRiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| multi_agent_core::Error::invalid_request("path is required"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| multi_agent_core::Error::invalid_request("content is required"))?;

        // Check active constraints
        let constraints = self.manager.get_constraints();
        for c in &constraints {
            let lower_c = c.to_lowercase();
            if lower_c.contains("read-only") || lower_c.contains("no write") {
                return Err(multi_agent_core::Error::SecurityViolation(format!(
                    "Writing to path {} blocked by read-only workspace constraint: {}",
                    path, c
                )));
            }
        }

        // Security: validate path using fs_policy
        let validated_path = multi_agent_core::fs_policy::validate_sandbox_path("/workspace", path)
            .map_err(|e| {
                multi_agent_core::Error::invalid_request(format!("Invalid path: {}", e))
            })?;

        // Convert back to string for engine
        let path_str = validated_path.to_string_lossy();

        let sandbox_id = self.manager.get_or_create().await?;

        // Create parent directories if needed
        if let Some(parent) = validated_path.parent() {
            if !parent.as_os_str().is_empty() {
                let mkdir_cmd = format!("mkdir -p /workspace/{}", parent.display());
                self.manager
                    .engine()
                    .exec(&sandbox_id, &mkdir_cmd, Duration::from_secs(5))
                    .await?;
            }
        }

        self.manager
            .engine()
            .write_file(&sandbox_id, &path_str, content.as_bytes())
            .await?;

        Ok(ToolOutput::text(format!(
            "File written: /workspace/{} ({} bytes)",
            path_str,
            content.len()
        )))
    }
}

// =============================================================================
// Sandbox Read File Tool
// =============================================================================

/// Tool for reading files from the sandbox's /workspace.
///
/// Risk level: LOW.
pub struct SandboxReadFileTool {
    manager: Arc<SandboxManager>,
}

impl SandboxReadFileTool {
    /// Create a new sandbox read file tool.
    pub fn new(manager: Arc<SandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SandboxReadFileTool {
    fn name(&self) -> &str {
        "sandbox_read_file"
    }

    fn description(&self) -> &str {
        "Read the content of a file from the isolated sandbox's /workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to /workspace"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| multi_agent_core::Error::invalid_request("path is required"))?;

        // Security: validate path using fs_policy
        let validated_path = multi_agent_core::fs_policy::validate_sandbox_path("/workspace", path)
            .map_err(|e| {
                multi_agent_core::Error::invalid_request(format!("Invalid path: {}", e))
            })?;

        let path_str = validated_path.to_string_lossy();

        let sandbox_id = self.manager.get_or_create().await?;
        let bytes = self
            .manager
            .engine()
            .read_file(&sandbox_id, &path_str)
            .await?;

        Ok(ToolOutput::text(
            String::from_utf8_lossy(&bytes).to_string(),
        ))
    }
}

// =============================================================================
// Sandbox List Files Tool
// =============================================================================

/// Tool for listing files in the sandbox's /workspace.
///
/// Risk level: LOW.
pub struct SandboxListFilesTool {
    manager: Arc<SandboxManager>,
}

impl SandboxListFilesTool {
    /// Create a new sandbox list files tool.
    pub fn new(manager: Arc<SandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SandboxListFilesTool {
    fn name(&self) -> &str {
        "sandbox_list_files"
    }

    fn description(&self) -> &str {
        "List files and directories in the sandbox's /workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to /workspace (default: '.')",
                    "default": "."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        // Security: validate path using fs_policy
        let validated_path = multi_agent_core::fs_policy::validate_sandbox_path("/workspace", path)
            .map_err(|e| {
                multi_agent_core::Error::invalid_request(format!("Invalid path: {}", e))
            })?;

        let path_str = validated_path.to_string_lossy();

        let sandbox_id = self.manager.get_or_create().await?;
        let command = format!("ls -la /workspace/{}", path_str.trim_start_matches('/'));
        let result = self
            .manager
            .engine()
            .exec(&sandbox_id, &command, Duration::from_secs(5))
            .await?;

        if result.success() {
            Ok(ToolOutput::text(result.stdout))
        } else {
            Ok(ToolOutput::error(format!(
                "Failed to list files: {}",
                result.stderr
            )))
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ExecResult, MockSandbox};
    use multi_agent_core::traits::ConstraintListener;

    fn make_manager(responses: Vec<ExecResult>) -> Arc<SandboxManager> {
        let engine = Arc::new(MockSandbox::new(responses));
        Arc::new(SandboxManager::new(engine, SandboxConfig::default()))
    }

    #[tokio::test]
    async fn test_shell_tool_success() {
        let manager = make_manager(vec![ExecResult {
            exit_code: 0,
            stdout: "Hello Sovereign World\n".into(),
            stderr: String::new(),
            timed_out: false,
        }]);

        let tool = SandboxShellTool::new(manager);
        let result = tool
            .execute(json!({"command": "echo Hello Sovereign World"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.content.contains("Hello Sovereign World"));
    }

    #[tokio::test]
    async fn test_shell_tool_failure() {
        let manager = make_manager(vec![ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "command not found".into(),
            timed_out: false,
        }]);

        let tool = SandboxShellTool::new(manager);
        let result = tool
            .execute(json!({"command": "nonexistent_command"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.content.contains("exit code 1"));
    }

    #[tokio::test]
    async fn test_shell_tool_timeout() {
        let manager = make_manager(vec![ExecResult {
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        }]);

        let tool = SandboxShellTool::new(manager);
        let result = tool
            .execute(json!({"command": "sleep 999", "timeout_secs": 1}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn test_write_file_path_traversal() {
        let manager = make_manager(vec![]);
        let tool = SandboxWriteFileTool::new(manager);

        let result = tool
            .execute(json!({"path": "../../../etc/passwd", "content": "evil"}))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Path traversal"));
    }

    #[tokio::test]
    async fn test_read_file_path_traversal() {
        let manager = make_manager(vec![]);
        let tool = SandboxReadFileTool::new(manager);

        let result = tool.execute(json!({"path": "/etc/passwd"})).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Absolute paths"));
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let engine = Arc::new(MockSandbox::default());
        let manager = Arc::new(SandboxManager::new(engine, SandboxConfig::default()));

        let write_tool = SandboxWriteFileTool::new(manager.clone());
        let read_tool = SandboxReadFileTool::new(manager);

        // Write
        let w_result = write_tool
            .execute(json!({"path": "hello.txt", "content": "Hello World"}))
            .await
            .unwrap();
        assert!(w_result.success);

        // Read back
        let r_result = read_tool
            .execute(json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert!(r_result.success);
        assert_eq!(r_result.content, "Hello World");
    }

    #[tokio::test]
    async fn test_sandbox_manager_lazy_creation() {
        let engine = Arc::new(MockSandbox::default());
        let manager = SandboxManager::new(engine, SandboxConfig::default());

        // First call creates
        let id1 = manager.get_or_create().await.unwrap();
        // Second call returns same
        let id2 = manager.get_or_create().await.unwrap();
        assert_eq!(id1.0, id2.0);
    }

    #[tokio::test]
    async fn test_sandbox_dynamic_constraints_no_python() {
        let manager = make_manager(vec![]);
        // Implement ConstraintListener manually or call update_constraints
        manager.update_constraints(vec!["Constraint: No Python".to_string()]);

        let tool = SandboxShellTool::new(manager);
        let result = tool.execute(json!({"command": "python script.py"})).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("blocked by active workspace constraint"));
    }

    #[tokio::test]
    async fn test_sandbox_dynamic_constraints_read_only() {
        let manager = make_manager(vec![]);
        manager.update_constraints(vec!["Constraint: Read-Only mode".to_string()]);

        let write_tool = SandboxWriteFileTool::new(manager.clone());
        let shell_tool = SandboxShellTool::new(manager);

        // 1. Write file tool should be blocked
        let w_res = write_tool
            .execute(json!({"path": "foo.txt", "content": "bar"}))
            .await;
        assert!(w_res.is_err());
        assert!(w_res
            .unwrap_err()
            .to_string()
            .contains("blocked by read-only workspace constraint"));

        // 2. Write commands in shell tool should be blocked
        let s_res = shell_tool
            .execute(json!({"command": "touch hello.txt"}))
            .await;
        assert!(s_res.is_err());
        assert!(s_res
            .unwrap_err()
            .to_string()
            .contains("blocked by read-only workspace constraint"));
    }
}
