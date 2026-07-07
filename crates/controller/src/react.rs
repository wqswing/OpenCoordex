//! ReAct loop implementation.
//!
//! ReAct (Reason + Act) is the core control loop for the agent:
//! 1. Reason about the current state
//! 2. Choose an action (tool or respond)
//! 3. Execute the action
//! 4. Observe the result
//! 5. Repeat until done or max iterations
//!
//! v0.2 Autonomous Capabilities:
//! - Dynamic Context Compression (auto-compresses when token threshold exceeded)
//! - Subagent Delegation (allows spawning child agents for subtasks)

use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

use multi_agent_core::{
    traits::{
        ApprovalGate, ChatMessage, Controller, LlmClient, LlmResponse, SessionStore, ToolRegistry,
    },
    types::{
        AgentResult, ApprovalRequest, ApprovalResponse, HistoryEntry, Session, SessionStatus,
        TaskState, TokenUsage, ToolCallInfo, ToolRiskLevel, UserIntent,
    },
    Error, Result,
};

use crate::capability::AgentCapability;

// v0.3: Security Integration
// (Guardrail unused in pure Controller struct if verified via capabilities)
// Keeping core imports minimal

/// ReAct controller configuration.
#[derive(Debug, Clone)]
pub struct ReActConfig {
    /// Maximum iterations before giving up.
    pub max_iterations: usize,
    /// Default token budget.
    pub default_budget: u64,
    /// Enable state persistence.
    pub persist_state: bool,
    /// Temperature for LLM calls.
    pub temperature: f32,
    /// KYA: Unique ID of the agent instance.
    pub agent_id: String,
    /// KYA: Type/role of the agent (e.g., "Coder", "Researcher", "Planner").
    pub agent_type: String,
    /// KYA: Underlying LLM model name.
    pub model_name: String,
}

impl Default for ReActConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            default_budget: 50_000,
            persist_state: true,
            temperature: 0.7,
            agent_id: "default-agent-id".to_string(),
            agent_type: "General".to_string(),
            model_name: "gpt-4o".to_string(),
        }
    }
}

// Use the new parser module
use crate::parser::ReActAction;

/// ReAct controller for executing complex tasks.
pub struct ReActController {
    /// Configuration.
    pub(crate) config: ReActConfig,
    /// LLM client for reasoning.
    pub(crate) llm: Option<Arc<dyn LlmClient>>,
    /// Tool registry for actions.
    pub(crate) tools: Option<Arc<dyn ToolRegistry>>,
    /// Session store for persistence.
    pub(crate) session_store: Option<Arc<dyn SessionStore>>,
    /// Agent capabilities (Unification of Compression, Delegation, MCP, Security).
    pub(crate) capabilities: Vec<Arc<dyn AgentCapability>>,
    /// Human-in-the-Loop approval gate for high-risk tools.
    pub(crate) approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// Policy Engine for rule-based risk assessment.
    pub(crate) policy_engine:
        Option<Arc<tokio::sync::RwLock<multi_agent_governance::PolicyEngine>>>,
    /// Event emitter for structured events.
    pub(crate) event_emitter: Option<Arc<dyn multi_agent_core::traits::EventEmitter>>,
}

impl ReActController {
    /// Create a new builder for ReActController.
    pub fn builder() -> crate::builder::ReActBuilder {
        crate::builder::ReActBuilder::new()
    }

    /// Create a new ReAct controller with default config (legacy support).
    pub fn new(config: ReActConfig) -> Self {
        Self {
            config,
            llm: None,
            tools: None,
            session_store: None,
            capabilities: Vec::new(),
            approval_gate: None,
            event_emitter: None,
            policy_engine: None,
        }
    }

    /// Create a new session.
    fn create_session(&self, goal: &str, trace_id: &str, user_id: Option<String>) -> Session {
        Session {
            id: Uuid::new_v4().to_string(),
            trace_id: trace_id.to_string(),
            user_id,
            status: SessionStatus::Running,
            history: vec![HistoryEntry {
                role: "system".to_string(),
                content: Arc::new(self.build_system_prompt(goal)),
                tool_call: None,
                timestamp: chrono_timestamp(),
            }],
            task_state: Some(TaskState {
                iteration: 0,
                goal: goal.to_string(),
                observations: Vec::new(),
                pending_actions: Vec::new(),
                consecutive_rejections: 0,
            }),
            token_usage: TokenUsage::with_budget(self.config.default_budget),
            created_at: chrono_timestamp(),
            updated_at: chrono_timestamp(),
        }
    }

    /// Build the system prompt for the agent.
    fn build_system_prompt(&self, goal: &str) -> String {
        let tools_description = self.get_tools_description();

        format!(
            r#"You are an AI assistant that uses the ReAct (Reasoning + Acting) pattern.

GOAL: {goal}

AVAILABLE TOOLS:
{tools_description}

INSTRUCTIONS:
1. Think step by step about what needs to be done
2. Use tools when needed by responding with ACTION
3. After receiving tool results, continue reasoning
4. When done, provide your FINAL ANSWER

RESPONSE FORMAT:
Use exactly one of these formats in each response:

For thinking/reasoning:
THOUGHT: <your reasoning here>

For tool calls:
ACTION: <tool_name>
ARGS: <json arguments>

For final answer (when task is complete):
FINAL ANSWER: <your complete answer>

Always think before acting. Be concise and focused on the goal."#
        )
    }

    /// Get description of available tools (for system prompt building).
    fn get_tools_description(&self) -> String {
        // For the system prompt, we return a placeholder since we can't call async here.
        // The actual tools list is fetched async when executing.
        "Tools will be loaded when execution starts.".to_string()
    }

    /// Build chat messages from session history (static version for capabilities).
    pub fn build_messages_static(session: &Session) -> Vec<ChatMessage> {
        session
            .history
            .iter()
            .map(|entry| ChatMessage {
                role: entry.role.clone(),
                content: entry.content.to_string(),
                tool_calls: None,
            })
            .collect()
    }

    /// Build chat messages from session history.
    fn build_messages(&self, session: &Session) -> Vec<ChatMessage> {
        Self::build_messages_static(session)
    }

    /// Parse the LLM response to extract action.
    fn parse_action(&self, response: &str) -> ReActAction {
        crate::parser::ActionParser::new(self.capabilities.clone()).parse(response)
    }

    /// Execute a single ReAct iteration with LLM.
    async fn execute_iteration_with_llm(
        &self,
        session: &mut Session,
        iteration: usize,
    ) -> Result<Option<AgentResult>> {
        let llm = self
            .llm
            .as_ref()
            .ok_or_else(|| Error::controller("LLM client not configured"))?;

        tracing::info!(
            session_id = %session.id,
            iteration = iteration,
            history_len = session.history.len(),
            "Executing ReAct iteration"
        );

        // v0.3: Capabilities On-Pre-Reasoning Hook (Compression, Security, etc.)
        for cap in &self.capabilities {
            cap.on_pre_reasoning(session)
                .await
                .map_err(|e| Error::controller(e.to_string()))?;
        }

        let messages = self.build_messages(session); // Rebuild messages after potential compression

        // Call LLM with (possibly compressed) messages
        let response: LlmResponse = llm.chat(&messages).await?;

        // Update token usage
        session.token_usage.add(
            response.usage.prompt_tokens,
            response.usage.completion_tokens,
        );

        tracing::debug!(
            response_len = response.content.len(),
            tokens_used = session.token_usage.total_tokens,
            "LLM response received"
        );

        // Add assistant response to history
        session.history.push(HistoryEntry {
            role: "assistant".to_string(),
            content: Arc::new(response.content.clone()),
            tool_call: None,
            timestamp: chrono_timestamp(),
        });

        // Parse and execute action
        let action = self.parse_action(&response.content);

        match action {
            ReActAction::FinalAnswer(ref answer) => {
                // Check capabilities on execution (Security Output check)
                for cap in &self.capabilities {
                    if let Some(result) = cap.on_execute(&action, session).await? {
                        // If a capability interrupts/handles FinalAnswer (e.g., blocks it), return that result
                        // Standard security cap returns Err on violation, keeping this flow simple.
                        if let AgentResult::Error { .. } = result {
                            return Ok(Some(result));
                        }
                    }
                }

                let final_result = AgentResult::Text(answer.clone());

                // Run on_finish hooks (e.g., knowledge summarization)
                for cap in &self.capabilities {
                    if let Err(e) = cap.on_finish(session, &final_result).await {
                        tracing::warn!(
                            capability = cap.name(),
                            error = %e,
                            "on_finish hook failed (non-fatal)"
                        );
                    }
                }

                tracing::info!(
                    answer_len = answer.len(),
                    "Task completed with final answer"
                );
                Ok(Some(final_result))
            }

            ReActAction::ToolCall { name, args } => {
                self.handle_tool_call(session, name, args).await
            }

            ReActAction::Think(thought) => {
                tracing::debug!(thought_len = thought.len(), "Agent thinking");

                // Ask the agent to take an action
                session.history.push(HistoryEntry {
                    role: "user".to_string(),
                    content: Arc::new("Please take an action using a tool, or provide your FINAL ANSWER if the task is complete.".to_string()),
                    tool_call: None,
                    timestamp: chrono_timestamp(),
                });

                // v0.4: Post-Execute Hook
                for cap in &self.capabilities {
                    cap.on_post_execute(session)
                        .await
                        .map_err(|e| Error::controller(e.to_string()))?;
                }

                Ok(None) // Continue loop
            }

            // Fallback: Check custom capability actions
            _ => {
                for cap in &self.capabilities {
                    if let Some(result) = cap.on_execute(&action, session).await? {
                        // Add observation to history if returned
                        if let AgentResult::Text(observation) = &result {
                            session.history.push(HistoryEntry {
                                role: "user".to_string(),
                                content: Arc::new(format!("OBSERVATION: {}", observation)),
                                tool_call: None,
                                timestamp: chrono_timestamp(),
                            });
                            // Update task state
                            if let Some(ref mut task_state) = session.task_state {
                                task_state.observations.push(Arc::new(observation.clone()));
                            }
                        }

                        // v0.4: Post-Execute Hook
                        for cap in &self.capabilities {
                            cap.on_post_execute(session)
                                .await
                                .map_err(|e| Error::controller(e.to_string()))?;
                        }

                        return Ok(None); // Action handled, continue loop
                    }
                }
                // If no capability handled it, default behavior (shouldn't happen if parsed correctly)
                Ok(None)
            }
        }
    }

    /// Execute iteration (mock if no LLM, real if LLM configured).
    async fn execute_iteration(
        &self,
        session: &mut Session,
        iteration: usize,
    ) -> Result<Option<AgentResult>> {
        if self.llm.is_some() {
            self.execute_iteration_with_llm(session, iteration).await
        } else {
            // Mock implementation for testing without LLM
            tracing::info!(
                session_id = %session.id,
                iteration = iteration,
                "Executing ReAct iteration (mock - no LLM)"
            );

            Ok(Some(AgentResult::Text(format!(
                "Mock ReAct execution. Goal: {}. Configure LLM client for real execution.",
                session
                    .task_state
                    .as_ref()
                    .map(|t| t.goal.as_str())
                    .unwrap_or("unknown")
            ))))
        }
    }

    async fn persist_session(&self, session: &Session) {
        if self.config.persist_state {
            if let Some(store) = &self.session_store {
                if let Err(e) = store.save(session).await {
                    tracing::warn!(error = %e, "Failed to save session state");
                }
            }
        }
    }

    async fn validate_fast_action_security(&self, args: &serde_json::Value) -> Result<()> {
        for cap in &self.capabilities {
            if cap.name() == "security_guardrails" {
                let mut temp_session =
                    self.create_session("fast_action_check", "temp-trace-id", None);
                temp_session.history.push(HistoryEntry {
                    role: "user".to_string(),
                    content: Arc::new(serde_json::to_string(args).unwrap_or_default()),
                    tool_call: None,
                    timestamp: chrono_timestamp(),
                });
                cap.on_pre_reasoning(&mut temp_session)
                    .await
                    .map_err(|e| Error::controller(e.to_string()))?;
            }
        }
        Ok(())
    }

    async fn handle_tool_call(
        &self,
        session: &mut Session,
        name: String,
        args: serde_json::Value,
    ) -> Result<Option<AgentResult>> {
        tracing::info!(tool = %name, "Executing tool call");

        // Emit TOOL_CALL_PROPOSED
        if let Some(emitter) = &self.event_emitter {
            use multi_agent_core::events::{EventEnvelope, EventType};
            let event = EventEnvelope::new(
                EventType::ToolCallProposed,
                serde_json::json!({
                    "tool_name": name,
                    "args": args
                }),
            )
            .with_trace(&session.trace_id)
            .with_session(&session.id);
            emitter.emit(event).await;
        }

        // 0. Trigger capabilities on tool proposed (e.g. Grilling)
        for cap in &self.capabilities {
            cap.on_tool_proposed(session, &name, &args).await?;
        }

        // =====================================================================
        // Policy Evaluation & HITL Approval Gate
        // =====================================================================
        let mut effective_args = args.clone();

        // 1. Evaluate Policy
        let (risk, risk_score, reason, matched_rule, policy_version) =
            if let Some(ref engine) = self.policy_engine {
                let engine = engine.read().await;
                let decision = engine.evaluate(&name, &effective_args);
                (
                    decision.risk_level,
                    decision.risk_score,
                    decision.reason,
                    decision.matched_rule,
                    decision.policy_version,
                )
            } else {
                // Fallback to legacy behavior if no engine is configured
                let risk = if let Some(ref tools) = self.tools {
                    tools.get_risk_level(&name).await
                } else {
                    ToolRiskLevel::Low
                };
                (
                    risk,
                    risk.score(),
                    "Default policy (no engine)".to_string(),
                    None,
                    "0.0.0".to_string(),
                )
            };

        // 2. Emit POLICY_EVALUATED event
        if let Some(emitter) = &self.event_emitter {
            use multi_agent_core::events::{EventEnvelope, EventType, PolicyEvaluationPayload};
            let event = EventEnvelope::new(
                EventType::PolicyEvaluated,
                serde_json::to_value(PolicyEvaluationPayload {
                    tool_name: name.clone(),
                    risk_level: format!("{:?}", risk),
                    risk_score,
                    matched_rule,
                    reason: reason.clone(),
                    policy_version,
                })
                .unwrap_or_default(),
            )
            .with_trace(&session.trace_id)
            .with_session(&session.id);
            emitter.emit(event).await;
        }

        // 3. Approval Check
        if let Some(ref gate) = self.approval_gate {
            let threshold_score = 50; // TODO: Make configurable via policy thresholds

            if risk_score >= threshold_score {
                tracing::info!(
                    tool = %name,
                    risk_score = risk_score,
                    threshold = threshold_score,
                    reason = %reason,
                    "Tool exceeds risk threshold — requesting human approval"
                );

                use sha2::Digest;
                let system_prompt = session
                    .history
                    .iter()
                    .find(|entry| entry.role == "system")
                    .map(|entry| entry.content.as_str())
                    .unwrap_or("");
                let mut hasher = sha2::Sha256::new();
                hasher.update(system_prompt.as_bytes());
                let prompt_hash = format!("{:x}", hasher.finalize());

                let approval_req = ApprovalRequest {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    session_id: session.id.clone(),
                    tool_name: name.clone(),
                    args: effective_args.clone(),
                    risk_level: risk,
                    context: format!(
                        "Policy Reason: {}. Session Goal: {}",
                        reason,
                        session
                            .task_state
                            .as_ref()
                            .map(|t| t.goal.clone())
                            .unwrap_or_default()
                    ),
                    timeout_secs: None,
                    nonce: uuid::Uuid::new_v4().to_string(),
                    expires_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                        + 300, // 5 minutes default expiry
                    agent_id: self.config.agent_id.clone(),
                    agent_type: self.config.agent_type.clone(),
                    system_prompt_hash: prompt_hash,
                    model_name: self.config.model_name.clone(),
                };

                match gate.request_approval(&approval_req).await? {
                    ApprovalResponse::Approved {
                        reason,
                        reason_code,
                        approver_id,
                        approver_role,
                    } => {
                        tracing::info!(
                            tool = %name,
                            reason = ?reason,
                            reason_code = %reason_code,
                            approver_id = ?approver_id,
                            approver_role = ?approver_role,
                            "Tool call APPROVED by human"
                        );
                        // Reset deadlock counter
                        if let Some(ref mut task_state) = session.task_state {
                            task_state.consecutive_rejections = 0;
                        }
                    }
                    ApprovalResponse::Denied {
                        reason,
                        reason_code,
                        approver_id,
                        approver_role,
                    } => {
                        tracing::warn!(
                            tool = %name,
                            reason = %reason,
                            reason_code = %reason_code,
                            approver_id = ?approver_id,
                            approver_role = ?approver_role,
                            "Tool call DENIED by human"
                        );

                        // Increment deadlock counter
                        if let Some(ref mut task_state) = session.task_state {
                            task_state.consecutive_rejections += 1;
                        }

                        let observation = format!(
                            "Tool '{}' was DENIED by human reviewer ({}): {}",
                            name, reason_code, reason
                        );
                        session.history.push(HistoryEntry {
                            role: "user".to_string(),
                            content: Arc::new(format!("OBSERVATION: {}", observation)),
                            tool_call: Some(ToolCallInfo {
                                name: name.clone(),
                                arguments: effective_args,
                                result: Some(Arc::new(observation.clone())),
                            }),
                            timestamp: chrono_timestamp(),
                        });
                        if let Some(ref mut task_state) = session.task_state {
                            task_state.observations.push(Arc::new(observation));
                        }
                        return Ok(None); // Continue loop; agent must adapt
                    }
                    ApprovalResponse::Modified {
                        args,
                        reason,
                        reason_code,
                        approver_id,
                        approver_role,
                    } => {
                        tracing::info!(
                            tool = %name,
                            reason = ?reason,
                            reason_code = %reason_code,
                            approver_id = ?approver_id,
                            approver_role = ?approver_role,
                            "Tool call MODIFIED by human"
                        );
                        // Reset deadlock counter
                        if let Some(ref mut task_state) = session.task_state {
                            task_state.consecutive_rejections = 0;
                        }
                        effective_args = args;
                    }
                }
            }
        }

        // =====================================================================
        // Execute the tool
        // =====================================================================
        let observation = if let Some(ref tools) = self.tools {
            // Emit TOOL_EXEC_STARTED
            if let Some(emitter) = &self.event_emitter {
                use multi_agent_core::events::{EventEnvelope, EventType};
                let event = EventEnvelope::new(
                    EventType::ToolExecStarted,
                    serde_json::json!({ "tool_name": name }),
                )
                .with_trace(&session.trace_id)
                .with_session(&session.id);
                emitter.emit(event).await;
            }

            let start_time = std::time::Instant::now();
            let result = tools.execute(&name, effective_args.clone()).await;
            let duration = start_time.elapsed().as_millis() as u64;

            // Emit TOOL_EXEC_FINISHED
            if let Some(emitter) = &self.event_emitter {
                use multi_agent_core::events::{EventEnvelope, EventType};
                let (success, output) = match &result {
                    Ok(o) => (o.success, o.content.clone()),
                    Err(e) => (false, e.to_string()),
                };

                let event = EventEnvelope::new(
                    EventType::ToolExecFinished,
                    serde_json::json!({
                        "tool_name": name,
                        "success": success,
                        "duration_ms": duration,
                        "output_len": output.len()
                    }),
                )
                .with_trace(&session.trace_id)
                .with_session(&session.id);
                emitter.emit(event).await;
            }

            match result {
                Ok(output) => {
                    if output.success {
                        format!("Tool '{}' succeeded:\n{}", name, output.content)
                    } else {
                        format!("Tool '{}' failed:\n{}", name, output.content)
                    }
                }
                Err(e) => format!("Tool '{}' error: {}", name, e),
            }
        } else {
            format!("Tool '{}' not available (no tools configured)", name)
        };

        session.history.push(HistoryEntry {
            role: "user".to_string(),
            content: Arc::new(format!("OBSERVATION: {}", observation)),
            tool_call: Some(ToolCallInfo {
                name: name.clone(),
                arguments: effective_args,
                result: Some(Arc::new(observation.clone())),
            }),
            timestamp: chrono_timestamp(),
        });

        if let Some(ref mut task_state) = session.task_state {
            task_state.observations.push(Arc::new(observation));
        }

        for cap in &self.capabilities {
            cap.on_post_execute(session)
                .await
                .map_err(|e| Error::controller(e.to_string()))?;
        }

        Ok(None)
    }

    /// Run the ReAct loop for a session.
    async fn run_loop(&self, session: &mut Session) -> Result<AgentResult> {
        let start_iteration = session
            .task_state
            .as_ref()
            .map(|t| t.iteration)
            .unwrap_or(0);

        tracing::info!(
            session_id = %session.id,
            start_iteration = start_iteration,
            "Starting/Resuming ReAct loop"
        );

        // Run on_start capabilities for fresh sessions (not resuming)
        if start_iteration == 0 {
            for cap in &self.capabilities {
                cap.on_start(session)
                    .await
                    .map_err(|e| Error::controller(e.to_string()))?;
            }
        }

        for iteration in start_iteration..self.config.max_iterations {
            if let Some(ref mut task_state) = session.task_state {
                task_state.iteration = iteration;
            }

            // 1. Check Budget Limits
            if session.token_usage.is_exceeded() {
                tracing::warn!(session_id = %session.id, "Token budget exceeded");
                session.status = SessionStatus::Failed;
                self.persist_session(session).await;
                return Err(Error::BudgetExceeded {
                    used: session.token_usage.total_tokens,
                    limit: session.token_usage.budget_limit,
                });
            }

            // 2. Check Deadlock Circuit Breaker
            if let Some(ref task_state) = session.task_state {
                if task_state.consecutive_rejections >= 3 {
                    tracing::error!(session_id = %session.id, "Deadlock detected: too many consecutive rejections");
                    session.status = SessionStatus::Failed;
                    self.persist_session(session).await;
                    return Err(Error::controller(
                        "Deadlock: Too many consecutive human rejections (3). Terminating session.",
                    ));
                }
            }

            match self.execute_iteration(session, iteration).await? {
                Some(result) => {
                    session.updated_at = chrono_timestamp();
                    session.status = SessionStatus::Completed;
                    self.persist_session(session).await;
                    return Ok(result);
                }
                None => {
                    session.updated_at = chrono_timestamp();
                    self.persist_session(session).await;

                    if session.token_usage.is_exceeded() {
                        session.status = SessionStatus::Failed;
                        // Persist failure state
                        self.persist_session(session).await;
                        return Err(Error::BudgetExceeded {
                            used: session.token_usage.total_tokens,
                            limit: session.token_usage.budget_limit,
                        });
                    }
                    continue;
                }
            }
        }

        session.status = SessionStatus::Failed;
        self.persist_session(session).await;
        Err(Error::MaxIterationsExceeded(self.config.max_iterations))
    }
}

#[async_trait]
impl Controller for ReActController {
    async fn execute(&self, intent: UserIntent, trace_id: String) -> Result<AgentResult> {
        match intent {
            UserIntent::FastAction {
                tool_name,
                args,
                user_id: _,
            } => {
                self.validate_fast_action_security(&args).await?;

                // Fast path: direct tool execution
                tracing::info!(tool = %tool_name, "Fast path execution");

                if let Some(ref tools) = self.tools {
                    match tools.execute(&tool_name, args).await {
                        Ok(output) => {
                            if output.success {
                                Ok(AgentResult::Text(output.content))
                            } else {
                                Ok(AgentResult::Error {
                                    message: output.content,
                                    code: "TOOL_ERROR".to_string(),
                                })
                            }
                        }
                        Err(e) => Ok(AgentResult::Error {
                            message: e.to_string(),
                            code: "TOOL_NOT_FOUND".to_string(),
                        }),
                    }
                } else {
                    Ok(AgentResult::Text(format!(
                        "Fast path: would execute tool '{}'. Tools not configured.",
                        tool_name
                    )))
                }
            }

            UserIntent::ComplexMission {
                goal,
                context_summary: _,
                visual_refs: _,
                user_id,
            } => {
                let mut session = self.create_session(&goal, &trace_id, user_id);
                // Run the loop
                self.run_loop(&mut session).await
            }
        }
    }

    async fn resume(&self, session_id: &str, _user_id: Option<&str>) -> Result<AgentResult> {
        let session_store = self.session_store.as_ref().ok_or_else(|| {
            Error::controller("State persistence not configured (session_store is None)")
        })?;

        // Load session
        let mut session = session_store
            .load(session_id)
            .await?
            .ok_or_else(|| Error::controller(format!("Session {} not found", session_id)))?;

        tracing::info!(session_id = %session_id, status = ?session.status, "Resuming session");

        match session.status {
            SessionStatus::Completed => {
                // If completed, find the final answer in history
                // We search backwards for the first assistant message that looks like a final answer
                // OR we can just return the last message content if it was a final answer
                // Since our `run_loop` returns AgentResult::Text(answer), we try to reconstruct it.
                // For now, let's just return a generic success message or the last content.
                let last_content = session
                    .history
                    .last()
                    .map(|h| h.content.to_string())
                    .unwrap_or_else(|| "Task completed (no content)".to_string());

                Ok(AgentResult::Text(last_content))
            }
            SessionStatus::Failed => Err(Error::controller("Cannot resume failed session")),
            SessionStatus::Running | SessionStatus::Paused => {
                // Resume execution
                self.run_loop(&mut session).await
            }
        }
    }

    async fn cancel(&self, session_id: &str) -> Result<()> {
        tracing::info!(session_id = session_id, "Cancel requested");
        Ok(())
    }
}

/// Get current timestamp.
pub fn chrono_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_final_answer() {
        let controller = ReActController::new(ReActConfig::default());

        let response = "FINAL ANSWER: The result is 42.";
        let action = controller.parse_action(response);

        match action {
            ReActAction::FinalAnswer(answer) => {
                assert_eq!(answer, "The result is 42.");
            }
            _ => panic!("Expected FinalAnswer"),
        }
    }

    #[test]
    fn test_parse_tool_call() {
        let controller = ReActController::new(ReActConfig::default());

        let response = r#"THOUGHT: I need to calculate something.
ACTION: calculator
ARGS: {"operation": "add", "a": 5, "b": 3}"#;

        let action = controller.parse_action(response);

        match action {
            ReActAction::ToolCall { name, args } => {
                assert_eq!(name, "calculator");
                assert_eq!(args["operation"], "add");
            }
            _ => panic!("Expected ToolCall, got {:?}", action),
        }
    }

    #[test]
    fn test_parse_thought() {
        let controller = ReActController::new(ReActConfig::default());

        let response = "THOUGHT: I need to think about this more.";
        let action = controller.parse_action(response);

        match action {
            ReActAction::Think(thought) => {
                assert!(thought.contains("think about"));
            }
            _ => panic!("Expected Think"),
        }
    }

    #[tokio::test]
    async fn test_fast_action() {
        let controller = ReActController::new(ReActConfig::default());

        let intent = UserIntent::FastAction {
            tool_name: "test_tool".to_string(),
            args: serde_json::json!({"query": "test"}),
            user_id: None,
        };

        let result = controller
            .execute(intent, "test-trace".to_string())
            .await
            .unwrap();
        match result {
            AgentResult::Text(text) => {
                assert!(text.contains("test_tool"));
            }
            _ => panic!("Expected Text result"),
        }
    }

    #[tokio::test]
    async fn test_complex_mission_mock() {
        let controller = ReActController::new(ReActConfig::default());

        let intent = UserIntent::ComplexMission {
            goal: "Test goal".to_string(),
            context_summary: "Test context".to_string(),
            visual_refs: vec![],
            user_id: None,
        };

        let result = controller
            .execute(intent, "test-trace".to_string())
            .await
            .unwrap();
        match result {
            AgentResult::Text(text) => {
                assert!(text.contains("Mock ReAct"));
            }
            _ => panic!("Expected Text result"),
        }
    }
}
