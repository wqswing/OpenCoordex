use chrono::Utc;
use multi_agent_admin::AdminState;
use multi_agent_core::{
    events::{EventEnvelope, EventType},
    traits::{ApprovalGate, ArtifactStore, KnowledgeEntry, KnowledgeStore},
    types::research::ResearchPlan,
    Error, Result,
};
use multi_agent_governance::{
    approval::ChannelApprovalGate,
    network::{NetworkDecision, NetworkPolicy},
};
use reqwest;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openai;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// State of a research task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ResearchStatus {
    Planning,
    AwaitingApproval,
    Denied,
    Executing,
    Synthesizing,
    Completed,
    Failed(String),
}

use multi_agent_core::config::SafetyConfig;
use multi_agent_governance::PolicyEngine;

/// Orchestrator for the Research Workflow.
pub struct ResearchOrchestrator {
    _admin_state: Arc<AdminState>,
    approval_gate: Arc<ChannelApprovalGate>,
    policy: Arc<RwLock<NetworkPolicy>>,
    policy_engine: Option<Arc<RwLock<PolicyEngine>>>,
    safety: SafetyConfig,
    artifact_store: Arc<dyn ArtifactStore>,
    knowledge_store: Arc<dyn KnowledgeStore>,
    logs_channel: Option<tokio::sync::broadcast::Sender<String>>,
}

impl ResearchOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        admin_state: Arc<AdminState>,
        approval_gate: Arc<ChannelApprovalGate>,
        policy: Arc<RwLock<NetworkPolicy>>,
        policy_engine: Option<Arc<RwLock<PolicyEngine>>>,
        safety: SafetyConfig,
        artifact_store: Arc<dyn ArtifactStore>,
        knowledge_store: Arc<dyn KnowledgeStore>,
        logs_channel: Option<tokio::sync::broadcast::Sender<String>>,
    ) -> Self {
        Self {
            _admin_state: admin_state,
            approval_gate,
            policy,
            policy_engine,
            safety,
            artifact_store,
            knowledge_store,
            logs_channel,
        }
    }

    /// Execute the full research workflow.
    pub async fn run_research(
        &self,
        session_id: &str,
        user_id: &str,
        query: &str,
    ) -> Result<String> {
        let trace_id = Uuid::new_v4().to_string();

        self.emit_audit(
            session_id,
            &trace_id,
            EventType::ResearchCreated,
            serde_json::json!({
                "query": query,
                "orchestrator_version": "P0"
            }),
        );

        // 1. Planning State
        tracing::info!(trace_id, "Transitioning to PLANNING");
        let plan = self
            .plan_research(session_id, user_id, &trace_id, query)
            .await?;

        // 2. Policy Evaluation
        tracing::info!(trace_id, "Transitioning to GOVERNANCE");
        let decision = self.check_policy(&plan).await;

        // STRICT BLOCK: If network policy denies, we fail immediately.
        if let NetworkDecision::Denied(reason) = &decision {
            self.emit_audit(
                session_id,
                &trace_id,
                EventType::PolicyEvaluated,
                serde_json::json!({
                    "decision": "DENIED",
                    "reason": reason,
                    "plan_summary": plan.goals
                }),
            );
            return Err(Error::governance(format!(
                "Research blocked by network policy: {}",
                reason
            )));
        }

        self.emit_audit(
            session_id,
            &trace_id,
            EventType::PolicyEvaluated,
            serde_json::json!({
                "decision": decision,
                "plan_summary": plan.goals
            }),
        );

        // 3. Approval Gate (Risk-Based)
        // Evaluate risk score using PolicyEngine
        if self.requires_approval(&plan).await {
            tracing::info!(trace_id, "Transitioning to AWAITING_APPROVAL");
            self.emit_audit(
                session_id,
                &trace_id,
                EventType::ApprovalRequested,
                serde_json::json!({
                    "plan": plan,
                    "reason": "Risk score exceeds approval threshold"
                }),
            );

            let approval_req = multi_agent_core::types::ApprovalRequest {
                request_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                tool_name: "research_agent".to_string(),
                args: serde_json::to_value(&plan).unwrap_or_default(),
                // We default to High if we don't have exact risk level from engine, or use engine's level
                risk_level: multi_agent_core::types::ToolRiskLevel::High,
                context: format!("Research query: {}", query),
                timeout_secs: Some(600),
                nonce: Uuid::new_v4().to_string(),
                expires_at: (Utc::now() + chrono::Duration::seconds(600)).timestamp(),
                agent_id: "research-agent-id".to_string(),
                agent_type: "Researcher".to_string(),
                system_prompt_hash: String::new(),
                model_name: "gpt-4o".to_string(),
            };

            let response = self.approval_gate.request_approval(&approval_req).await?;

            match response {
                multi_agent_core::types::ApprovalResponse::Approved { .. } => {
                    self.emit_audit(
                        session_id,
                        &trace_id,
                        EventType::ApprovalDecided,
                        serde_json::json!({"status": "APPROVED"}),
                    );
                }
                _ => {
                    self.emit_audit(
                        session_id,
                        &trace_id,
                        EventType::ApprovalDecided,
                        serde_json::json!({"status": "DENIED"}),
                    );
                    return Err(Error::governance(
                        "Research task denied by administrator".to_string(),
                    ));
                }
            }
        }

        // 4. Execution State (Airlock)
        tracing::info!(trace_id, "Transitioning to EXECUTION");
        let findings = self.execute_research(session_id, &trace_id, &plan).await?;

        // 5. Synthesis State
        tracing::info!(trace_id, "Transitioning to SYNTHESIS");
        let report = self
            .synthesize_findings(session_id, user_id, &trace_id, query, findings)
            .await?;

        self.emit_audit(
            session_id,
            &trace_id,
            EventType::ReportGenerated,
            serde_json::json!({
                 "report_len": report.len(),
                 "status": "COMPLETED"
            }),
        );

        Ok(report)
    }

    async fn plan_research(
        &self,
        session_id: &str,
        user_id: &str,
        trace_id: &str,
        query: &str,
    ) -> Result<ResearchPlan> {
        self.emit_audit(
            session_id,
            trace_id,
            EventType::PlanProposed,
            serde_json::json!({"query": query}),
        );

        // Use Rig for planning (M10.1)
        let client = openai::Client::from_env();
        let planner = client.agent("gpt-4o")
            .preamble("You are a research planner. Analyze the query and provide a structured research plan including goals, list of domains to visit, and crawl limits. Output MUST be valid JSON.")
            .build();

        let plan = planner
            .prompt(query)
            .await
            .map_err(|e| Error::internal(format!("Rig planning error: {}", e)))?;

        // Extract structured JSON from the LLM response
        // In a real M10 we'd use rig's structured output but here we demonstrate the intent
        let mut plan: ResearchPlan = serde_json::from_str(&plan)
            .map_err(|e| Error::internal(format!("Failed to parse research plan: {}", e)))?;

        plan.user_id = Some(user_id.to_string());

        Ok(plan)
    }

    async fn check_policy(&self, plan: &ResearchPlan) -> NetworkDecision {
        let p = self.policy.read().await;
        for domain in &plan.candidate_domains {
            // Ensure we handle the Result from check()
            match p.check(domain) {
                Ok(NetworkDecision::Denied(reason)) => return NetworkDecision::Denied(reason),
                Err(e) => return NetworkDecision::Denied(format!("Invalid URL: {}", e)),
                _ => {}
            }
        }
        NetworkDecision::Allowed
    }

    async fn requires_approval(&self, plan: &ResearchPlan) -> bool {
        if let Some(engine) = &self.policy_engine {
            let engine = engine.read().await;

            // Evaluate risk score
            // Treat the plan as args to "research_agent" tool
            let args = serde_json::to_value(plan).unwrap_or_default();
            let decision = engine.evaluate("research_agent", &args);

            let threshold = engine.policy.thresholds.approval_required;

            if decision.risk_score >= threshold {
                return true;
            }
        }

        // Fallback: Default to strict approval if no engine configured? Or safe defaults?
        // Let's assume safe default: No engine -> No extra approval (rely on NetworkPolicy)
        false
    }

    async fn execute_research(
        &self,
        session_id: &str,
        trace_id: &str,
        plan: &ResearchPlan,
    ) -> Result<Vec<String>> {
        let mut results = Vec::new();
        // Client for fetch_with_policy
        let client = reqwest::Client::builder()
            .user_agent("MultiAgent-Research/1.0")
            .redirect(reqwest::redirect::Policy::none()) // Important: manual redirect handling
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| Error::internal(format!("Failed to build HTTP client: {}", e)))?;

        for domain in &plan.candidate_domains {
            let url_str = if domain.starts_with("http") {
                domain.clone()
            } else {
                format!("https://{}", domain)
            };

            let url = match url::Url::parse(&url_str) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("Skipping invalid URL {}: {}", url_str, e);
                    continue;
                }
            };

            // Emit EGRESS_REQUEST
            self.emit_audit(
                session_id,
                trace_id,
                multi_agent_core::events::EventType::EgressRequest,
                serde_json::json!({
                    "url": url_str,
                    "method": "GET"
                }),
            );

            // Use unified egress (fetch_with_policy)
            // We need to read policy lock
            let policy_guard = self.policy.read().await;

            use multi_agent_governance::network::fetch_with_policy;

            let response_result = fetch_with_policy(
                &client,
                &policy_guard,
                &self.safety,
                reqwest::Method::GET,
                url.clone(),
                None, // No headers
                None, // No body
            )
            .await;

            let response = match response_result {
                Ok(resp) => resp,
                Err(e) => {
                    self.emit_audit(
                        session_id,
                        trace_id,
                        multi_agent_core::events::EventType::EgressResult,
                        serde_json::json!({
                            "url": url_str,
                            "status": "ERROR",
                            "error": e.to_string()
                        }),
                    );
                    continue;
                }
            };

            let status = response.status();
            let headers = response.headers().clone();
            let content_type = headers
                .get("content-type")
                .and_then(|h: &reqwest::header::HeaderValue| h.to_str().ok())
                .unwrap_or("unknown")
                .to_string();

            // Read body with safety limit
            use futures::StreamExt;
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut total_size = 0;
            let limit = self.safety.max_download_size_bytes;
            let mut failed = false;

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        total_size += bytes.len() as u64;
                        if total_size > limit {
                            tracing::warn!("Response size exceeded limit for {}", url_str);
                            failed = true;
                            break;
                        }
                        buffer.extend_from_slice(&bytes);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read body chunk from {}: {}", url_str, e);
                        failed = true;
                        break;
                    }
                }
            }

            if failed {
                continue;
            }

            let body = String::from_utf8_lossy(&buffer).to_string();

            // Calculate hash for audit
            let mut hasher = Sha256::new();
            hasher.update(body.as_bytes());
            let body_hash = format!("{:x}", hasher.finalize());

            // Persist finding to ArtifactStore
            // In a real system we'd parse HTML to text, but for now we store raw or simple text
            let ref_id = self
                .artifact_store
                .save_with_type(
                    bytes::Bytes::from(buffer.clone()), // Use buffer directly
                    &content_type,
                )
                .await?;

            // Emit EGRESS_RESULT with reference to artifact and metadata
            self.emit_audit(
                session_id,
                trace_id,
                multi_agent_core::events::EventType::EgressResult,
                serde_json::json!({
                    "url": url_str,
                    "status": status.as_u16(),
                    "content_type": content_type,
                    "body_len": body.len(),
                    "body_hash": body_hash,
                    "artifact_id": ref_id
                }),
            );

            // Use simplified content for the results passed to synthesis
            results.push(format!(
                "Source: {}\nURL: {}\nContent:\n{}",
                domain, url_str, body
            ));
        }

        Ok(results)
    }

    async fn synthesize_findings(
        &self,
        session_id: &str,
        user_id: &str,
        _trace_id: &str,
        query: &str,
        findings: Vec<String>,
    ) -> Result<String> {
        // M10.5: Synthesis (Rig based)
        let client = openai::Client::from_env();
        let synthesis_agent = client.agent("gpt-4o")
            .preamble("You are a research analyst. Consolidate the provided findings into a comprehensive research report.")
            .build();

        let context = findings.join("\n\n---\n\n");
        let prompt = format!("Research Query: {}\n\nFindings:\n{}", query, context);

        let report: String = synthesis_agent
            .prompt(prompt)
            .await
            .map_err(|e| Error::internal(format!("Synthesis error: {}", e)))?;

        // M10.3: Store in Knowledge Base
        let entry = KnowledgeEntry {
            id: Uuid::new_v4().to_string(),
            summary: report.clone(),
            source_task: query.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            embedding: vec![0.0; 1536], // Mock embedding for now, real systems would call an embedding model
            tags: vec!["research".to_string()],
            created_at: Utc::now().timestamp(),
        };

        self.knowledge_store.store(entry).await?;

        Ok(report)
    }

    fn emit_audit(
        &self,
        session_id: &str,
        trace_id: &str,
        event_type: EventType,
        payload: serde_json::Value,
    ) {
        let envelope = EventEnvelope::new(event_type, payload)
            .with_session(session_id)
            .with_trace(trace_id);

        if let Some(tx) = &self.logs_channel {
            if let Ok(json) = serde_json::to_string(&envelope) {
                let _ = tx.send(json);
            }
        }
        tracing::info!(?envelope, "Audit Event");
    }
}
