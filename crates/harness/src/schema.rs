use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single evaluation test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Unique test case ID.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what is being tested.
    pub description: String,
    /// The prompt/input to send to the Agent.
    pub prompt: String,
    /// Assertion criteria for validating the output.
    pub expected_output: OutputAssertion,
    /// Tags for categorization (e.g. "math", "pii", "tool-use").
    pub tags: Vec<String>,
    /// Predefined LLM completions for deterministic evaluation.
    /// If provided, LLM calls are mocked and return these responses in order.
    pub mock_llm_responses: Option<Vec<String>>,
    /// Predefined tool outputs mapped by tool name.
    /// If provided, tool execution will be mocked.
    pub mock_tool_outputs: Option<HashMap<String, String>>,
    /// Custom override for maximum ReAct iterations.
    pub max_iterations: Option<usize>,
    /// Custom override for token budget.
    pub token_budget: Option<u64>,
}

/// Assertions supported by the validation engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum OutputAssertion {
    /// The output must exactly match this string (whitespace-trimmed).
    ExactMatch(String),
    /// The output must contain this substring.
    Contains(String),
    /// The output must match this regular expression pattern.
    Regex(String),
    /// The output must parse as JSON and match this JSON Schema.
    JsonSchema(serde_json::Value),
    /// The output is evaluated by another LLM based on criteria.
    LlmJudge { criteria: String },
}

/// A suite of related test cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suite {
    pub id: String,
    pub name: String,
    pub description: String,
    pub cases: Vec<TestCase>,
}

/// The result of running a single test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseResult {
    pub test_case_id: String,
    pub name: String,
    pub status: RunStatus,
    pub actual_output: String,
    pub steps: usize,
    pub latency_ms: u64,
    pub tokens_used: u64,
    pub history: Vec<multi_agent_core::types::HistoryEntry>,
    pub failure_reason: Option<String>,
}

/// Status of the test case execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Passed,
    Failed,
    Timeout,
    Error,
}

/// The aggregated result of running a test suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub suite_id: String,
    pub suite_name: String,
    pub total_cases: usize,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub avg_latency_ms: u64,
    pub total_tokens: u64,
    pub results: Vec<TestCaseResult>,
}
