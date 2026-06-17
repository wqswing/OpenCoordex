use super::refs::RefId;
use serde::{Deserialize, Serialize};

// =============================================================================
// Agent Result Types (L1 Output)
// =============================================================================

/// Result from the agent after completing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum AgentResult {
    /// Text response.
    Text(String),

    /// File artifact (code, document, etc.).
    File {
        /// Reference to the file in L3.
        ref_id: RefId,
        /// File name.
        filename: String,
        /// MIME type.
        mime_type: String,
    },

    /// Structured data response.
    Data(serde_json::Value),

    /// Interactive UI component (React/JSON).
    UiComponent {
        /// Component type.
        component_type: String,
        /// Component props/configuration.
        props: serde_json::Value,
    },

    /// Error result.
    Error {
        /// Error message.
        message: String,
        /// Error code.
        code: String,
    },
}

// =============================================================================
// Tool Risk Levels & Approval Types (L4 Governance)
// =============================================================================

/// Risk level classification for tools.
///
/// Used by the HITL (Human-in-the-Loop) approval gate to determine
/// which tool calls require human approval before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum ToolRiskLevel {
    /// No risk — pure read-only / informational.
    #[default]
    Low,
    /// Moderate risk — writes data but is reversible.
    Medium,
    /// High risk — executes code, modifies state, or accesses external systems.
    High,
    /// Critical — destructive, irreversible, or affects production systems.
    Critical,
}

impl ToolRiskLevel {
    /// Get the numeric risk score (0-100).
    pub fn score(&self) -> u32 {
        match self {
            Self::Low => 10,
            Self::Medium => 30,
            Self::High => 60,
            Self::Critical => 90,
        }
    }
}

/// Request sent to the human for approval before executing a high-risk tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Unique ID for this approval request.
    pub request_id: String,
    /// Session this request belongs to.
    pub session_id: String,
    /// Name of the tool to be executed.
    pub tool_name: String,
    /// Arguments the agent wants to pass.
    pub args: serde_json::Value,
    /// Risk level of the tool.
    pub risk_level: ToolRiskLevel,
    /// Agent's reasoning for why it wants to run this tool.
    pub context: String,
    /// Timeout for this specific request.
    pub timeout_secs: Option<u64>,
    /// Cryptographic nonce to prevent replay attacks.
    pub nonce: String,
    /// Expiration timestamp (Unix epoch).
    pub expires_at: i64,

    /// KYA: Unique ID of the agent instance.
    #[serde(default)]
    pub agent_id: String,
    /// KYA: Type/role of the agent (e.g., "Coder", "Researcher", "Planner").
    #[serde(default)]
    pub agent_type: String,
    /// KYA: SHA-256 fingerprint of the agent's system prompt/instructions.
    #[serde(default)]
    pub system_prompt_hash: String,
    /// KYA: Underlying LLM serving the agent.
    #[serde(default)]
    pub model_name: String,
}

/// Human's response to an approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ApprovalResponse {
    /// Approved — proceed with execution.
    Approved {
        /// Optional reason for approval.
        reason: Option<String>,
        /// Mandatory reason code for auditing (e.g., "USER_APPROVED", "AUTO_APPROVED").
        reason_code: String,
        /// Identity of the user who approved this.
        #[serde(default)]
        approver_id: Option<String>,
        /// Primary role of the approver.
        #[serde(default)]
        approver_role: Option<String>,
    },
    /// Denied — do not execute.
    Denied {
        /// Reason for denial.
        reason: String,
        /// Mandatory reason code for auditing (e.g., "USER_DENIED", "TIMEOUT", "POLICY_VIOLATION").
        reason_code: String,
        /// Identity of the user who denied this.
        #[serde(default)]
        approver_id: Option<String>,
        /// Primary role of the approver.
        #[serde(default)]
        approver_role: Option<String>,
    },
    /// Modified — execute with different arguments.
    Modified {
        /// Modified arguments.
        args: serde_json::Value,
        /// Optional reason.
        reason: Option<String>,
        /// Mandatory reason code.
        reason_code: String,
        /// Identity of the user who modified this.
        #[serde(default)]
        approver_id: Option<String>,
        /// Primary role of the approver.
        #[serde(default)]
        approver_role: Option<String>,
    },
}
