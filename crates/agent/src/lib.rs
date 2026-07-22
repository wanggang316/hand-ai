//! Hand Agent — core agent runtime with tool calling, event streaming, and cancellation.
//!
//! Public surface:
//!
//! - [`Agent`] owns the transcript, dispatches events, and drives the loop.
//! - [`AgentTool`] is the tool registration shape with optional schema validation.
//! - [`run_agent_loop`] / [`run_agent_loop_continue`] expose the low-level loop directly.
//! - [`AgentEvent`] is the unified event surface for UIs.
//! - [`session`] adds persistent session logs ([`SessionStore`] with JSONL and in-memory backends).
//!
//! Cancellation flows through `tokio_util::sync::CancellationToken`. Calling
//! [`Agent::abort`] cancels any in-flight run; subsequent runs use a fresh token.

pub mod agent;
pub mod agent_loop;
pub mod error;
pub mod proxy;
pub mod session;
pub mod types;

pub use agent::{AbortHandle, Agent, AgentOptions, IntoPromptInput, Listener, SubscriptionHandle};
pub use agent_loop::{
    AgentEventSink, AgentLoopResult, run_agent_loop, run_agent_loop_continue,
    run_agent_loop_with_messages,
};
pub use error::AgentError;
pub use proxy::{ProxyAssistantMessageEvent, ProxyStreamOptions, stream_fn_proxy, stream_proxy};
pub use session::{
    ContextProjection, InMemoryStore, JsonlStore, Projector, SESSION_FORMAT_VERSION, SessionEntry,
    SessionHeader, SessionStore, SessionStoreError, SessionSummary,
};
pub use types::{
    AfterToolCallContext, AfterToolCallHook, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentState, AgentTool, BeforeToolCallContext, BeforeToolCallHook,
    BeforeToolCallResult, BoxFuture, ConvertToLlmFn, GetApiKeyFn, GetFollowUpMessagesFn,
    GetSteeringMessagesFn, OnUpdate, PrepareArgumentsFn, QueueDeliveryMode,
    ShouldStopAfterTurnContext, ShouldStopAfterTurnFn, StreamFn, ToolError, ToolExecuteCtx,
    ToolExecuteFn, ToolExecutionMode, ToolResult, TransformContextFn, extract_tool_calls,
};

// Re-export the cancellation token so consumers don't need to depend on tokio-util directly.
pub use tokio_util::sync::CancellationToken;
