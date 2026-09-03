#![deny(unused)]
//! L1 Controller for OpenCoordex.
//!
//! This crate provides the ReAct loop, DAG orchestration, and SOP engine
//! for executing complex tasks.

pub mod builder;
pub mod capability;
pub mod context;
pub mod dag;
pub mod delegation;
pub mod executor;
pub mod memory;
pub mod memory_writeback;
pub mod parser;
pub mod persistence;
pub mod planning;
pub mod react;
pub mod sop;
pub mod summarization;

pub use builder::ReActBuilder;
pub use capability::{
    ActiveWorkspaceCapability, AgentCapability, CompressionCapability, DelegationCapability,
    GrillingCapability, McpCapability, ReflectionCapability, SecurityCapability,
};
pub use memory::MemoryCapability;
pub use memory_writeback::MemoryWritebackCapability;
pub use multi_agent_core::traits::SessionStore;
pub use parser::{ActionParser, ReActAction};
pub use persistence::InMemorySessionStore;
pub use planning::PlanningCapability;
pub use react::{chrono_timestamp, ReActConfig, ReActController};
pub use summarization::SummarizationCapability;
