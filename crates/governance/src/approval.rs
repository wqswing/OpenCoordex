//! HITL (Human-in-the-Loop) approval gate implementations.
//!
//! Provides mechanisms for human review and approval of high-risk tool calls.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, Mutex};

use multi_agent_core::{
    traits::ApprovalGate,
    types::{ApprovalRequest, ApprovalResponse, ToolRiskLevel},
    Error, Result,
};

struct PendingRequest {
    sender: oneshot::Sender<ApprovalResponse>,
    nonce: String,
    risk_level: ToolRiskLevel,
    tool_name: String,
    agent_id: String,
    agent_type: String,
}

// =============================================================================
// Channel-Based Approval Gate
// =============================================================================

/// Approval gate that uses channels for async notification.
///
/// When a tool requires approval, a request is published to listeners
/// (e.g., a WebSocket handler) and the execution pauses until a response
/// arrives via the oneshot channel.
pub struct ChannelApprovalGate {
    /// Minimum risk level that triggers approval.
    threshold: ToolRiskLevel,
    /// Pending approval requests, keyed by request_id.
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
    /// Broadcast channel for notifying listeners about new requests.
    request_tx: broadcast::Sender<ApprovalRequest>,
    /// Timeout for waiting for approval (default: 5 minutes).
    timeout: std::time::Duration,
}

impl ChannelApprovalGate {
    /// Create a new channel-based approval gate.
    pub fn new(threshold: ToolRiskLevel) -> Self {
        let (request_tx, _) = broadcast::channel(32);
        Self {
            threshold,
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_tx,
            timeout: std::time::Duration::from_secs(300), // 5 minutes
        }
    }

    /// Set the approval timeout.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Subscribe to approval request notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<ApprovalRequest> {
        self.request_tx.subscribe()
    }

    /// Helper function to check role clearance based on risk level.
    fn check_role_clearance(
        risk_level: ToolRiskLevel,
        roles: &[String],
    ) -> std::result::Result<String, String> {
        let mut best_role: Option<&str> = None;
        for r in roles {
            let r_str = r.to_lowercase();
            match r_str.as_str() {
                "admin" => {
                    best_role = Some("admin");
                    break;
                }
                "security_officer" | "security" => {
                    if best_role.is_none()
                        || best_role == Some("compliance")
                        || best_role == Some("operator")
                    {
                        best_role = Some("security_officer");
                    }
                }
                "compliance" => {
                    if best_role.is_none() || best_role == Some("operator") {
                        best_role = Some("compliance");
                    }
                }
                "operator" if best_role.is_none() => {
                    best_role = Some("operator");
                }
                _ => {}
            }
        }

        let meets_requirement = match risk_level {
            ToolRiskLevel::Critical => {
                matches!(best_role, Some("admin") | Some("security_officer"))
            }
            ToolRiskLevel::High => {
                matches!(
                    best_role,
                    Some("admin") | Some("security_officer") | Some("compliance")
                )
            }
            ToolRiskLevel::Medium => {
                matches!(
                    best_role,
                    Some("admin")
                        | Some("security_officer")
                        | Some("compliance")
                        | Some("operator")
                )
            }
            ToolRiskLevel::Low => true,
        };

        if meets_requirement {
            Ok(best_role.unwrap_or("user").to_string())
        } else {
            Err(format!(
                "Insufficient clearance: tool risk level {:?} requires appropriate role, but user has roles {:?}",
                risk_level, roles
            ))
        }
    }

    /// Submit a human's response to a pending approval request.
    ///
    /// Called by WebSocket/REST handlers when the human reviews a request.
    pub async fn submit_response(
        &self,
        request_id: &str,
        nonce: &str,
        approver_id: String,
        approver_roles: Vec<String>,
        mut response: ApprovalResponse,
    ) -> std::result::Result<(), String> {
        let mut pending = self.pending.lock().await;
        let pending_req = match pending.get(request_id) {
            Some(req) => req,
            None => return Err(format!("No pending request with ID: {}", request_id)),
        };

        if pending_req.nonce != nonce {
            return Err("Invalid nonce".to_string());
        }

        // Enforce Separation of Duties / Clearance checks
        let primary_role = Self::check_role_clearance(pending_req.risk_level, &approver_roles)?;

        // Now that validation is successful, we remove the request from the pending map
        let pending_req = pending.remove(request_id).unwrap();

        tracing::info!(
            request_id = %request_id,
            agent_id = %pending_req.agent_id,
            agent_type = %pending_req.agent_type,
            tool = %pending_req.tool_name,
            "Submitting response for pending approval request"
        );

        // Inject KYA / Human responsibility metrics
        match &mut response {
            ApprovalResponse::Approved {
                approver_id: id,
                approver_role: role,
                ..
            }
            | ApprovalResponse::Denied {
                approver_id: id,
                approver_role: role,
                ..
            }
            | ApprovalResponse::Modified {
                approver_id: id,
                approver_role: role,
                ..
            } => {
                *id = Some(approver_id);
                *role = Some(primary_role);
            }
        }

        pending_req
            .sender
            .send(response)
            .map_err(|_| "Request channel closed (agent may have timed out)".to_string())
    }

    /// Get the list of currently pending approval requests.
    pub async fn list_pending(&self) -> Vec<String> {
        self.pending.lock().await.keys().cloned().collect()
    }
}

#[async_trait]
impl ApprovalGate for ChannelApprovalGate {
    async fn request_approval(&self, req: &ApprovalRequest) -> Result<ApprovalResponse> {
        let (tx, rx) = oneshot::channel();

        // Register the pending request
        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                req.request_id.clone(),
                PendingRequest {
                    sender: tx,
                    nonce: req.nonce.clone(),
                    risk_level: req.risk_level,
                    tool_name: req.tool_name.clone(),
                    agent_id: req.agent_id.clone(),
                    agent_type: req.agent_type.clone(),
                },
            );
        }

        // Notify listeners (WebSocket, etc.)
        let _ = self.request_tx.send(req.clone());

        tracing::info!(
            request_id = %req.request_id,
            tool = %req.tool_name,
            risk = ?req.risk_level,
            agent_id = %req.agent_id,
            agent_type = %req.agent_type,
            "Waiting for human approval (timeout: {:?})",
            self.timeout
        );

        // Wait for response with timeout
        let timeout = req
            .timeout_secs
            .map(std::time::Duration::from_secs)
            .unwrap_or(self.timeout);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                // Channel dropped — clean up
                self.pending.lock().await.remove(&req.request_id);
                Err(Error::governance("Approval channel closed unexpectedly"))
            }
            Err(_) => {
                // Timeout — auto-deny
                self.pending.lock().await.remove(&req.request_id);
                tracing::warn!(
                    request_id = %req.request_id,
                    "Approval request timed out — auto-denied"
                );
                Ok(ApprovalResponse::Denied {
                    reason: "Approval timed out (auto-denied for safety)".to_string(),
                    reason_code: "TIMEOUT".to_string(),
                    approver_id: Some("system".to_string()),
                    approver_role: Some("security_officer".to_string()),
                })
            }
        }
    }

    fn threshold(&self) -> ToolRiskLevel {
        self.threshold
    }
}

// =============================================================================
// Auto-Approve Gate (for development/testing)
// =============================================================================

/// Approval gate that auto-approves all requests.
///
/// Use only in development/testing environments.
pub struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn request_approval(&self, req: &ApprovalRequest) -> Result<ApprovalResponse> {
        tracing::warn!(
            tool = %req.tool_name,
            risk = ?req.risk_level,
            "AUTO-APPROVED (development mode — do NOT use in production)"
        );
        Ok(ApprovalResponse::Approved {
            reason: Some("Auto-approved in development mode".to_string()),
            reason_code: "AUTO_APPROVED".to_string(),
            approver_id: Some("auto_approver".to_string()),
            approver_role: Some("admin".to_string()),
        })
    }

    fn threshold(&self) -> ToolRiskLevel {
        ToolRiskLevel::Critical // Only trigger on Critical in dev mode
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_auto_approve_gate() {
        let gate = AutoApproveGate;
        let req = ApprovalRequest {
            request_id: "test-1".into(),
            session_id: "session-1".into(),
            tool_name: "sandbox_shell".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            risk_level: ToolRiskLevel::Critical,
            context: "test".into(),
            timeout_secs: None,
            nonce: "test-nonce-1".into(),
            expires_at: 0,
            agent_id: "test-agent".into(),
            agent_type: "Coder".into(),
            system_prompt_hash: "hash-1".into(),
            model_name: "gpt-4o".into(),
        };

        let response = gate.request_approval(&req).await.unwrap();
        assert!(matches!(response, ApprovalResponse::Approved { .. }));
    }

    #[tokio::test]
    async fn test_channel_gate_submit_response() {
        let gate = ChannelApprovalGate::new(ToolRiskLevel::High)
            .with_timeout(std::time::Duration::from_secs(10));

        let req = ApprovalRequest {
            request_id: "test-2".into(),
            session_id: "session-1".into(),
            tool_name: "sandbox_shell".into(),
            args: serde_json::json!({"command": "ls"}),
            risk_level: ToolRiskLevel::High,
            context: "test".into(),
            timeout_secs: None,
            nonce: "test-nonce-2".into(),
            expires_at: 0,
            agent_id: "test-agent".into(),
            agent_type: "Coder".into(),
            system_prompt_hash: "hash-1".into(),
            model_name: "gpt-4o".into(),
        };

        // Spawn the approval request
        let gate_clone = Arc::new(gate);
        let gate_for_task = gate_clone.clone();
        let req_clone = req.clone();

        let handle = tokio::spawn(async move { gate_for_task.request_approval(&req_clone).await });

        // Give the request time to register
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Submit approval with admin user (authorized)
        gate_clone
            .submit_response(
                "test-2",
                "test-nonce-2",
                "admin_user".to_string(),
                vec!["admin".to_string()],
                ApprovalResponse::Approved {
                    reason: None,
                    reason_code: "USER_APPROVED".into(),
                    approver_id: None,
                    approver_role: None,
                },
            )
            .await
            .unwrap();

        let response = handle.await.unwrap().unwrap();
        match response {
            ApprovalResponse::Approved {
                approver_id,
                approver_role,
                ..
            } => {
                assert_eq!(approver_id.as_deref(), Some("admin_user"));
                assert_eq!(approver_role.as_deref(), Some("admin"));
            }
            _ => panic!("Expected Approved"),
        }
    }

    #[tokio::test]
    async fn test_channel_gate_denial() {
        let gate = Arc::new(
            ChannelApprovalGate::new(ToolRiskLevel::High)
                .with_timeout(std::time::Duration::from_secs(10)),
        );

        let req = ApprovalRequest {
            request_id: "test-3".into(),
            session_id: "session-1".into(),
            tool_name: "sandbox_shell".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            risk_level: ToolRiskLevel::High,
            context: "test".into(),
            timeout_secs: None,
            nonce: "test-nonce-3".into(),
            expires_at: 0,
            agent_id: "test-agent".into(),
            agent_type: "Coder".into(),
            system_prompt_hash: "hash-1".into(),
            model_name: "gpt-4o".into(),
        };

        let gate_for_task = gate.clone();
        let req_clone = req.clone();

        let handle = tokio::spawn(async move { gate_for_task.request_approval(&req_clone).await });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Submit denial with compliance user (authorized for High)
        gate.submit_response(
            "test-3",
            "test-nonce-3",
            "compliance_user".to_string(),
            vec!["compliance".to_string()],
            ApprovalResponse::Denied {
                reason: "too dangerous".into(),
                reason_code: "USER_DENIED".into(),
                approver_id: None,
                approver_role: None,
            },
        )
        .await
        .unwrap();

        let response = handle.await.unwrap().unwrap();
        match response {
            ApprovalResponse::Denied {
                reason,
                approver_id,
                approver_role,
                ..
            } => {
                assert_eq!(reason, "too dangerous");
                assert_eq!(approver_id.as_deref(), Some("compliance_user"));
                assert_eq!(approver_role.as_deref(), Some("compliance"));
            }
            _ => panic!("Expected Denied"),
        }
    }

    #[tokio::test]
    async fn test_channel_gate_insufficient_clearance() {
        let gate = Arc::new(
            ChannelApprovalGate::new(ToolRiskLevel::Critical)
                .with_timeout(std::time::Duration::from_secs(10)),
        );

        let req = ApprovalRequest {
            request_id: "test-so-1".into(),
            session_id: "session-1".into(),
            tool_name: "sandbox_shell".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            risk_level: ToolRiskLevel::Critical,
            context: "test".into(),
            timeout_secs: None,
            nonce: "test-nonce-so-1".into(),
            expires_at: 0,
            agent_id: "test-agent".into(),
            agent_type: "Coder".into(),
            system_prompt_hash: "hash-1".into(),
            model_name: "gpt-4o".into(),
        };

        let gate_for_task = gate.clone();
        let req_clone = req.clone();

        let _handle = tokio::spawn(async move { gate_for_task.request_approval(&req_clone).await });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Submit response with operator user (unauthorized for Critical)
        let res = gate
            .submit_response(
                "test-so-1",
                "test-nonce-so-1",
                "operator_user".to_string(),
                vec!["operator".to_string()],
                ApprovalResponse::Approved {
                    reason: None,
                    reason_code: "USER_APPROVED".into(),
                    approver_id: None,
                    approver_role: None,
                },
            )
            .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Insufficient clearance"));
    }

    #[tokio::test]
    async fn test_channel_gate_timeout() {
        let gate = ChannelApprovalGate::new(ToolRiskLevel::High)
            .with_timeout(std::time::Duration::from_millis(200));

        let req = ApprovalRequest {
            request_id: "test-4".into(),
            session_id: "session-1".into(),
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

        // Don't submit any response — should timeout
        let response = gate.request_approval(&req).await.unwrap();
        match response {
            ApprovalResponse::Denied {
                reason,
                reason_code,
                approver_id,
                approver_role,
            } => {
                assert!(reason.contains("timed out"));
                assert_eq!(reason_code, "TIMEOUT");
                assert_eq!(approver_id.as_deref(), Some("system"));
                assert_eq!(approver_role.as_deref(), Some("security_officer"));
            }
            _ => panic!("Expected Denied due to timeout"),
        }
    }
}
