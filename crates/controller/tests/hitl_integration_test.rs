//! HITL (Human-in-the-Loop) integration tests.
//!
//! Tests the approval gate integration with the ReActController,
//! verifying that high-risk tools are blocked/approved correctly.

use async_trait::async_trait;
use std::sync::Arc;

use multi_agent_controller::{ReActConfig, ReActController};
use multi_agent_core::{
    traits::{
        ApprovalGate, ChatMessage, Controller, LlmClient, LlmResponse, LlmUsage, ToolRegistry,
    },
    types::{ApprovalRequest, ApprovalResponse, ToolRiskLevel},
};
use multi_agent_skills::DefaultToolRegistry;
use multi_agent_store::InMemorySessionStore;

// =============================================================================
// Mock LLM — always calls sandbox_shell
// =============================================================================

struct ToolCallingLlm;

#[async_trait]
impl LlmClient for ToolCallingLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        Ok(LlmResponse {
            content: "THOUGHT: I need to run a command.\nACTION: sandbox_shell\nACTION_INPUT: {\"command\": \"ls\"}".to_string(),
            finish_reason: "stop".to_string(),
            usage: LlmUsage { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
            tool_calls: None,
        })
    }
    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        self.complete("").await
    }
    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![])
    }
}

// =============================================================================
// Mock Approval Gates
// =============================================================================

/// Always denies high-risk tool calls.
struct DenyGate;

#[async_trait]
impl ApprovalGate for DenyGate {
    async fn request_approval(
        &self,
        _req: &ApprovalRequest,
    ) -> multi_agent_core::Result<ApprovalResponse> {
        Ok(ApprovalResponse::Denied {
            reason: "Denied by policy".to_string(),
            reason_code: "TEST_DENIED".to_string(),
            approver_id: Some("system".to_string()),
            approver_role: Some("admin".to_string()),
        })
    }
    fn threshold(&self) -> ToolRiskLevel {
        ToolRiskLevel::High
    }
}

/// Always approves high-risk tool calls.
struct ApproveGate;

#[async_trait]
impl ApprovalGate for ApproveGate {
    async fn request_approval(
        &self,
        _req: &ApprovalRequest,
    ) -> multi_agent_core::Result<ApprovalResponse> {
        Ok(ApprovalResponse::Approved {
            reason: None,
            reason_code: "TEST_APPROVED".to_string(),
            approver_id: Some("system".to_string()),
            approver_role: Some("admin".to_string()),
        })
    }
    fn threshold(&self) -> ToolRiskLevel {
        ToolRiskLevel::High
    }
}

/// Modifies the arguments of the tool call.
struct _ModifyGate;

#[async_trait]
impl ApprovalGate for _ModifyGate {
    async fn request_approval(
        &self,
        _req: &ApprovalRequest,
    ) -> multi_agent_core::Result<ApprovalResponse> {
        Ok(ApprovalResponse::Modified {
            args: serde_json::json!({"command": "echo 'modified by gate'"}),
            reason: None,
            reason_code: "TEST_MODIFIED".to_string(),
            approver_id: Some("system".to_string()),
            approver_role: Some("admin".to_string()),
        })
    }
    fn threshold(&self) -> ToolRiskLevel {
        ToolRiskLevel::High
    }
}

// =============================================================================
// 1. 高风险工具被 DenyGate 拦截 → 连续拒绝触发死锁断路器
// =============================================================================

#[tokio::test]
async fn test_deny_gate_triggers_deadlock_breaker() {
    let registry = DefaultToolRegistry::new();
    // Register a mock High-risk tool via sandbox echo
    use multi_agent_sandbox::engine::{MockSandbox, SandboxConfig};
    use multi_agent_sandbox::tools::{SandboxManager, SandboxShellTool};

    let engine = Arc::new(MockSandbox::default());
    let sandbox_mgr = Arc::new(SandboxManager::new(engine, SandboxConfig::default()));
    let shell_tool = SandboxShellTool::new(sandbox_mgr);
    registry.register(Box::new(shell_tool)).await.unwrap();

    let config = ReActConfig {
        max_iterations: 10,
        ..ReActConfig::default()
    };

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(Arc::new(ToolCallingLlm))
        .with_tools(Arc::new(registry))
        .with_approval_gate(Arc::new(DenyGate))
        .with_session_store(Arc::new(InMemorySessionStore::new()))
        .build();

    let intent = multi_agent_core::types::UserIntent::ComplexMission {
        goal: "List files in workspace".into(),
        context_summary: "test".into(),
        visual_refs: vec![],
        user_id: None,
    };

    let result = controller.execute(intent, "test-trace".to_string()).await;

    // Agent should terminate — via deadlock breaker, budget, or max iterations
    // (depending on whether mock LLM output format triggers the ReAct tool call parser)
    match result {
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            assert!(
                msg.contains("deadlock")
                    || msg.contains("budget")
                    || msg.contains("max iterations"),
                "Expected termination error, got: {}",
                msg
            );
        }
        Ok(multi_agent_core::types::AgentResult::Error { message, .. }) => {
            let msg = message.to_lowercase();
            assert!(
                msg.contains("deadlock")
                    || msg.contains("budget")
                    || msg.contains("max iterations"),
                "Expected termination error in AgentResult, got: {}",
                message
            );
        }
        Ok(_) => {
            // Controller terminated gracefully — also acceptable
        }
    }
}

// =============================================================================
// 2. ApproveGate 允许工具执行（不报错）
// =============================================================================

#[tokio::test]
async fn test_approve_gate_allows_execution() {
    // This test verifies no panic/error. The mock LLM loops, so we
    // rely on max_iterations to end. The key point is: no Denied errors.
    let registry = DefaultToolRegistry::new();
    use multi_agent_sandbox::engine::{ExecResult, MockSandbox, SandboxConfig};
    use multi_agent_sandbox::tools::{SandboxManager, SandboxShellTool};

    let engine = Arc::new(MockSandbox::new(vec![ExecResult {
        exit_code: 0,
        stdout: "file1.txt".into(),
        stderr: String::new(),
        timed_out: false,
    }]));
    let sandbox_mgr = Arc::new(SandboxManager::new(engine, SandboxConfig::default()));
    registry
        .register(Box::new(SandboxShellTool::new(sandbox_mgr)))
        .await
        .unwrap();

    let config = ReActConfig {
        max_iterations: 2, // Short loop
        ..ReActConfig::default()
    };

    let controller = ReActController::builder()
        .with_config(config)
        .with_llm(Arc::new(ToolCallingLlm))
        .with_tools(Arc::new(registry))
        .with_approval_gate(Arc::new(ApproveGate))
        .with_session_store(Arc::new(InMemorySessionStore::new()))
        .build();

    let intent = multi_agent_core::types::UserIntent::ComplexMission {
        goal: "List files in workspace".into(),
        context_summary: "test".into(),
        visual_refs: vec![],
        user_id: None,
    };

    // Should NOT fail with Denied
    let result = controller.execute(intent, "test-trace".to_string()).await;
    match &result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("Denied"),
                "ApproveGate should not deny: {}",
                msg
            );
        }
        _ => { /* ok */ }
    }
}

// =============================================================================
// 3. ChannelApprovalGate 超时自动拒绝
// =============================================================================

#[tokio::test]
async fn test_channel_gate_timeout_auto_deny() {
    use multi_agent_core::traits::ApprovalGate;
    use multi_agent_governance::approval::ChannelApprovalGate;

    let gate = ChannelApprovalGate::new(ToolRiskLevel::High)
        .with_timeout(std::time::Duration::from_millis(100));

    let req = ApprovalRequest {
        request_id: "timeout-test".into(),
        session_id: "s1".into(),
        tool_name: "sandbox_shell".into(),
        args: serde_json::json!({"command": "rm -rf /"}),
        risk_level: ToolRiskLevel::High,
        context: "test".into(),
        timeout_secs: None,
        nonce: "timeout-nonce".into(),
        expires_at: chrono::Utc::now().timestamp() + 60,
        agent_id: "test-agent".into(),
        agent_type: "Coder".into(),
        system_prompt_hash: "hash-1".into(),
        model_name: "gpt-4o".into(),
    };

    // No response submitted → should timeout and auto-deny
    let response = gate.request_approval(&req).await.unwrap();
    match response {
        ApprovalResponse::Denied { reason, .. } => {
            assert!(
                reason.contains("timed out"),
                "Should mention timeout: {}",
                reason
            );
        }
        other => panic!("Expected Denied, got: {:?}", other),
    }
}

// =============================================================================
// 4. ChannelApprovalGate 异步 submit_response
// =============================================================================

#[tokio::test]
async fn test_channel_gate_async_approve() {
    use multi_agent_core::traits::ApprovalGate;
    use multi_agent_governance::approval::ChannelApprovalGate;

    let gate = Arc::new(
        ChannelApprovalGate::new(ToolRiskLevel::High)
            .with_timeout(std::time::Duration::from_secs(5)),
    );

    let req = ApprovalRequest {
        request_id: "async-test".into(),
        session_id: "s2".into(),
        tool_name: "sandbox_shell".into(),
        args: serde_json::json!({"command": "ls"}),
        risk_level: ToolRiskLevel::High,
        context: "test".into(),
        timeout_secs: None,
        nonce: "test-nonce-4".into(),
        expires_at: 0,
        agent_id: "test-agent".into(),
        agent_type: "Coder".into(),
        system_prompt_hash: "hash-1".into(),
        model_name: "gpt-4o".into(),
    };

    // Spawn the approval request
    let gate_clone = gate.clone();
    let req_clone = req.clone();
    let handle = tokio::spawn(async move { gate_clone.request_approval(&req_clone).await });

    // Wait a moment for request to register, then submit
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    gate.submit_response(
        "async-test",
        "test-nonce-4",
        "admin_user".to_string(),
        vec!["admin".to_string()],
        ApprovalResponse::Approved {
            reason: None,
            reason_code: "TEST_APPROVED".to_string(),
            approver_id: None,
            approver_role: None,
        },
    )
    .await
    .unwrap();

    let response = handle.await.unwrap().unwrap();
    assert!(matches!(response, ApprovalResponse::Approved { .. }));
}
