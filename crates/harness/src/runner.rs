use crate::evaluator::AssertionEvaluator;
use crate::schema::{RunStatus, Suite, SuiteResult, TestCase, TestCaseResult};
use async_trait::async_trait;
use multi_agent_controller::{ReActConfig, ReActController};
use multi_agent_core::traits::{
    ChatMessage, Controller, LlmClient, SessionStore, Tool, ToolRegistry,
};
use multi_agent_core::types::{AgentResult, ToolDefinition, ToolOutput, UserIntent};
use multi_agent_core::{LlmResponse, LlmUsage};
use multi_agent_store::InMemorySessionStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// =============================================================================
// Harness Scripted Mock LLM
// =============================================================================

pub struct HarnessMockLlm {
    responses: Mutex<Vec<String>>,
    call_count: Mutex<usize>,
}

impl HarnessMockLlm {
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
            call_count: Mutex::new(0),
        }
    }
}

#[async_trait]
impl LlmClient for HarnessMockLlm {
    async fn complete(&self, _prompt: &str) -> multi_agent_core::Result<LlmResponse> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;

        let responses = self.responses.lock().unwrap();
        let idx = (*count - 1) % responses.len().max(1);
        let content = responses
            .get(idx)
            .cloned()
            .unwrap_or_else(|| "FINAL ANSWER: Done".to_string());

        Ok(LlmResponse {
            content,
            finish_reason: "stop".to_string(),
            usage: LlmUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            },
            tool_calls: None,
        })
    }

    async fn chat(&self, _messages: &[ChatMessage]) -> multi_agent_core::Result<LlmResponse> {
        self.complete("").await
    }

    async fn embed(&self, _text: &str) -> multi_agent_core::Result<Vec<f32>> {
        Ok(vec![0.0; 1536])
    }
}

// =============================================================================
// Harness Mocked Tool & ToolRegistry
// =============================================================================

pub struct MockedHarnessTool {
    name: String,
    output: String,
}

#[async_trait]
impl Tool for MockedHarnessTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Mocked tool output for harness test"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }
    async fn execute(&self, _args: serde_json::Value) -> multi_agent_core::Result<ToolOutput> {
        Ok(ToolOutput::text(self.output.clone()))
    }
}

pub struct HarnessMockToolRegistry {
    inner: Arc<dyn ToolRegistry>,
    mock_outputs: HashMap<String, String>,
}

impl HarnessMockToolRegistry {
    pub fn new(inner: Arc<dyn ToolRegistry>, mock_outputs: HashMap<String, String>) -> Self {
        Self {
            inner,
            mock_outputs,
        }
    }
}

#[async_trait]
impl ToolRegistry for HarnessMockToolRegistry {
    async fn register(&self, tool: Box<dyn Tool>) -> multi_agent_core::Result<()> {
        self.inner.register(tool).await
    }

    async fn get(&self, name: &str) -> multi_agent_core::Result<Option<Box<dyn Tool>>> {
        if let Some(val) = self.mock_outputs.get(name) {
            return Ok(Some(Box::new(MockedHarnessTool {
                name: name.to_string(),
                output: val.clone(),
            })));
        }
        self.inner.get(name).await
    }

    async fn list(&self) -> multi_agent_core::Result<Vec<ToolDefinition>> {
        let mut list = self.inner.list().await?;
        for name in self.mock_outputs.keys() {
            if !list.iter().any(|t| &t.name == name) {
                list.push(ToolDefinition {
                    name: name.clone(),
                    description: "Mocked tool output for harness test".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                    supports_streaming: false,
                });
            }
        }
        Ok(list)
    }

    async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> multi_agent_core::Result<ToolOutput> {
        if let Some(val) = self.mock_outputs.get(name) {
            return Ok(ToolOutput::text(val.clone()));
        }
        self.inner.execute(name, args).await
    }
}

// =============================================================================
// Harness Runner Engine
// =============================================================================

pub struct HarnessRunner {
    llm_client: Arc<dyn LlmClient>,
    tool_registry: Arc<dyn ToolRegistry>,
    judge_llm: Option<Arc<dyn LlmClient>>,
}

impl HarnessRunner {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        tool_registry: Arc<dyn ToolRegistry>,
        judge_llm: Option<Arc<dyn LlmClient>>,
    ) -> Self {
        Self {
            llm_client,
            tool_registry,
            judge_llm,
        }
    }

    /// Run a single test case.
    pub async fn run_test_case(&self, test_case: &TestCase) -> TestCaseResult {
        let start_time = Instant::now();
        let trace_id = format!("harness-trace-{}", uuid::Uuid::new_v4());

        // 1. Prepare LLM Client
        let test_llm: Arc<dyn LlmClient> = match &test_case.mock_llm_responses {
            Some(resps) => Arc::new(HarnessMockLlm::new(resps.clone())),
            None => self.llm_client.clone(),
        };

        // 2. Prepare Tool Registry
        let test_tools: Arc<dyn ToolRegistry> = match &test_case.mock_tool_outputs {
            Some(mocks) => Arc::new(HarnessMockToolRegistry::new(
                self.tool_registry.clone(),
                mocks.clone(),
            )),
            None => self.tool_registry.clone(),
        };

        // 3. Build Controller
        let config = ReActConfig {
            max_iterations: test_case.max_iterations.unwrap_or(10),
            default_budget: test_case.token_budget.unwrap_or(50000),
            persist_state: true,
            temperature: 0.0, // Low temperature for reproducibility
            ..ReActConfig::default()
        };

        let session_store = Arc::new(InMemorySessionStore::new());
        let controller = ReActController::builder()
            .with_config(config)
            .with_llm(test_llm)
            .with_tools(test_tools)
            .with_session_store(session_store.clone())
            .build();

        let intent = UserIntent::ComplexMission {
            goal: test_case.prompt.clone(),
            context_summary: "".to_string(),
            visual_refs: vec![],
            user_id: None,
        };

        // 4. Execute ReAct Loop
        let run_outcome = controller.execute(intent, trace_id.clone()).await;

        let latency_ms = start_time.elapsed().as_millis() as u64;

        // 5. Retrieve Session History & Telemetry
        let mut steps = 0;
        let mut tokens_used = 0;
        let mut history = Vec::new();

        // Load running sessions to find history
        if let Ok(sessions) = session_store.list_sessions(None, None).await {
            let sessions: Vec<multi_agent_core::types::Session> = sessions;
            if let Some(session) = sessions.first() {
                history = session.history.clone();
                tokens_used = session.token_usage.total_tokens;
                if let Some(task_state) = &session.task_state {
                    steps = task_state.iteration;
                }
            }
        }

        // 6. Handle Execution Output
        let (actual_output, execute_status, init_fail_reason) = match run_outcome {
            Ok(AgentResult::Text(text)) => (text, RunStatus::Passed, None),
            Ok(AgentResult::File {
                ref_id, filename, ..
            }) => (
                format!("File: {} (ref: {})", filename, ref_id.as_str()),
                RunStatus::Passed,
                None,
            ),
            Ok(AgentResult::Data(val)) => (val.to_string(), RunStatus::Passed, None),
            Ok(AgentResult::UiComponent { component_type, .. }) => (
                format!("UI Component: {}", component_type),
                RunStatus::Passed,
                None,
            ),
            Ok(AgentResult::Error { message, code }) => {
                let status = if code == "BUDGET_EXCEEDED" {
                    RunStatus::Timeout
                } else {
                    RunStatus::Error
                };
                (
                    format!("Error: {} (code: {})", message, code),
                    status,
                    Some(message),
                )
            }
            Err(e) => {
                let err_msg = e.to_string();
                let status = if err_msg.contains("BudgetExceeded") || err_msg.contains("timeout") {
                    RunStatus::Timeout
                } else {
                    RunStatus::Error
                };
                (format!("Failure: {}", err_msg), status, Some(err_msg))
            }
        };

        // 7. If ReAct Loop completed, run assertion validation
        let mut final_status = execute_status;
        let mut failure_reason = init_fail_reason;

        if final_status == RunStatus::Passed {
            let history_json = serde_json::to_string(&history).ok();
            let eval_outcome = AssertionEvaluator::evaluate(
                &actual_output,
                &test_case.expected_output,
                self.judge_llm.as_ref().map(|x| x.as_ref()),
                history_json.as_deref(),
            )
            .await;

            match eval_outcome {
                Ok((passed, err_msg)) => {
                    if !passed {
                        final_status = RunStatus::Failed;
                        failure_reason = err_msg;
                    }
                }
                Err(e) => {
                    final_status = RunStatus::Error;
                    failure_reason = Some(format!("Assertion compiler error: {}", e));
                }
            }
        }

        TestCaseResult {
            test_case_id: test_case.id.clone(),
            name: test_case.name.clone(),
            status: final_status,
            actual_output,
            steps,
            latency_ms,
            tokens_used,
            history,
            failure_reason,
        }
    }

    /// Run a full suite of test cases.
    pub async fn run_suite(&self, suite: &Suite) -> SuiteResult {
        let mut results = Vec::new();
        let mut total_latency = 0;
        let mut total_tokens = 0;
        let mut passed_cases = 0;

        for case in &suite.cases {
            let res = self.run_test_case(case).await;
            if res.status == RunStatus::Passed {
                passed_cases += 1;
            }
            total_latency += res.latency_ms;
            total_tokens += res.tokens_used;
            results.push(res);
        }

        let total_cases = suite.cases.len();
        let failed_cases = total_cases - passed_cases;
        let avg_latency_ms = if total_cases > 0 {
            total_latency / total_cases as u64
        } else {
            0
        };

        SuiteResult {
            suite_id: suite.id.clone(),
            suite_name: suite.name.clone(),
            total_cases,
            passed_cases,
            failed_cases,
            avg_latency_ms,
            total_tokens,
            results,
        }
    }
}
