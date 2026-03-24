//! Hand Agent — core agent runtime with tool calling and state management.
//!
//! This crate provides the agent loop that orchestrates LLM calls,
//! tool execution, and conversation state management.

pub mod agent;
pub mod agent_loop;
pub mod error;
pub mod types;

// Re-export commonly used items
pub use agent::Agent;
pub use agent_loop::{AgentEventSink, AgentLoopResult, run_agent_loop, run_agent_loop_continue};
pub use error::AgentError;
pub use types::{
    AfterToolCallContext, AfterToolCallHook, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentState, AgentTool, BeforeToolCallContext, BeforeToolCallHook,
    BeforeToolCallResult, BoxFuture, ToolExecuteFn, ToolExecutionMode, ToolResult,
};
