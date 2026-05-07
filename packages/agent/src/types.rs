//! Core types for the agent runtime.

use model::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, SimpleStreamOptions,
    TextContent, ToolCall, ToolResultContent, ToolResultMessage,
};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

/// How tool calls from a single assistant message are executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// Each tool call is prepared, executed, and finalized before the next.
    Sequential,
    /// Tool calls are prepared sequentially, then allowed tools execute concurrently.
    /// Final tool results are still emitted in assistant source order.
    #[default]
    Parallel,
}

/// How queued steering/follow-up messages are dequeued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueDeliveryMode {
    /// Send all queued messages at once.
    All,
    /// Send one message per turn.
    #[default]
    OneAtATime,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content blocks (text/image) returned by the tool.
    pub content: Vec<ToolResultContent>,
    /// Opaque details for UI / logging.
    pub details: Option<serde_json::Value>,
    /// True when the tool indicated a semantic failure (vs. successful
    /// content that happens to describe a problem). The agent loop emits
    /// this flag to the model so it can react.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// Create a simple text result. `is_error` is `false`.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text(TextContent::new(text))],
            details: None,
            is_error: false,
        }
    }

    /// Create an error result. `is_error` is `true`.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text(TextContent::new(message))],
            details: None,
            is_error: true,
        }
    }
}

/// A boxed, sendable future for async tool execution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Per-call context handed to tool execute closures.
///
/// Carries session-level metadata that tools (especially extension-supplied
/// ones) need to anchor file paths, look up per-session state, and tag log
/// output. Callers of `run_agent_loop` populate `cwd` / `session_id` via
/// [`AgentLoopConfig`]; the agent loop fills in `call_id` per tool call.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    /// The session's working directory.
    pub cwd: PathBuf,
    /// The session's stable id (empty string when no session is associated).
    pub session_id: String,
    /// The unique id of the current tool call (matches the LLM's tool call id).
    pub call_id: String,
}

/// The async function that executes a tool.
///
/// Arguments:
/// - `tool_call_id`: The unique ID for this specific call
/// - `args`: Validated JSON arguments
/// - `cx`: Per-call session context (cwd, session id, call id)
pub type ToolExecuteFn = Box<
    dyn Fn(String, serde_json::Value, ToolExecutionContext) -> BoxFuture<'static, ToolResult>
        + Send
        + Sync,
>;

/// An executable tool registered with the agent.
pub struct AgentTool {
    /// Tool name (must match the LLM tool definition name).
    pub name: String,
    /// Human-readable description for the LLM.
    pub description: String,
    /// JSON Schema for the parameters.
    pub parameters: serde_json::Value,
    /// Human-readable label for UI display.
    pub label: String,
    /// The async execute function.
    pub execute: ToolExecuteFn,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("label", &self.label)
            .finish()
    }
}

impl AgentTool {
    /// Create a new agent tool.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        label: impl Into<String>,
        execute: ToolExecuteFn,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            label: label.into(),
            execute,
        }
    }

    /// Convert to a model::Tool for LLM context.
    pub fn to_model_tool(&self) -> model::Tool {
        model::Tool::new(
            self.name.clone(),
            self.description.clone(),
            self.parameters.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// Agent context
// ---------------------------------------------------------------------------

/// Agent-level context that may include custom message types.
///
/// Uses `Message` from the model crate directly — custom message types
/// are represented as user messages with special content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// Agent events
// ---------------------------------------------------------------------------

/// Events emitted during agent execution for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Agent execution started.
    AgentStart,
    /// Agent execution ended.
    AgentEnd { messages: Vec<Message> },
    /// A new turn started.
    TurnStart,
    /// A turn ended.
    TurnEnd {
        message: Message,
        tool_results: Vec<ToolResultMessage>,
    },
    /// A message (user/assistant/tool_result) started.
    MessageStart { message: Message },
    /// Assistant message streaming update.
    MessageUpdate {
        message: Message,
        assistant_message_event: Box<AssistantMessageEvent>,
    },
    /// A message ended (final version).
    MessageEnd { message: Message },
    /// Tool execution started.
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    /// Streaming update during tool execution.
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        update: serde_json::Value,
    },
    /// Tool execution ended.
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}

// ---------------------------------------------------------------------------
// Hook types
// ---------------------------------------------------------------------------

/// Result from `before_tool_call` hook.
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallResult {
    /// If true, the tool call is blocked and an error result is emitted.
    pub block: bool,
    /// Reason shown in the error result when blocked.
    pub reason: Option<String>,
    /// If `Some`, the tool is invoked with these arguments instead of the
    /// model's original ones. Ignored when `block` is true. The model still
    /// sees the original tool call id, so the conversation thread stays
    /// consistent — only the args fed to the tool change.
    pub replace_args: Option<serde_json::Value>,
}

/// Result from `after_tool_call` hook — partial override of the tool result.
#[derive(Debug, Clone, Default)]
pub struct AfterToolCallResult {
    /// If provided, replaces the full content array.
    pub content: Option<Vec<ToolResultContent>>,
    /// If provided, replaces the details.
    pub details: Option<serde_json::Value>,
    /// If provided, replaces the error flag.
    pub is_error: Option<bool>,
}

/// Context passed to `before_tool_call`.
pub struct BeforeToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCall,
    pub args: &'a serde_json::Value,
}

/// Context passed to `after_tool_call`.
pub struct AfterToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCall,
    pub args: &'a serde_json::Value,
    pub result: &'a ToolResult,
    pub is_error: bool,
}

/// Async hook called before tool execution.
pub type BeforeToolCallHook = Box<
    dyn Fn(BeforeToolCallContext<'_>) -> BoxFuture<'_, Option<BeforeToolCallResult>> + Send + Sync,
>;

/// Async hook called after tool execution.
pub type AfterToolCallHook = Box<
    dyn Fn(AfterToolCallContext<'_>) -> BoxFuture<'_, Option<AfterToolCallResult>> + Send + Sync,
>;

/// Async hook that returns steering messages.
pub type GetSteeringMessagesFn = Box<dyn Fn() -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Async hook that returns follow-up messages.
pub type GetFollowUpMessagesFn = Box<dyn Fn() -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Convert agent messages to LLM-compatible messages.
pub type ConvertToLlmFn = Box<dyn Fn(&[Message]) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Transform context before LLM call (applied before convertToLlm).
pub type TransformContextFn =
    Box<dyn Fn(Vec<Message>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Dynamic API key resolver (for OAuth tokens, etc.).
pub type GetApiKeyFn = Box<dyn Fn(&str) -> BoxFuture<'static, Option<String>> + Send + Sync>;

// ---------------------------------------------------------------------------
// Agent loop configuration
// ---------------------------------------------------------------------------

/// Configuration for the agent loop.
pub struct AgentLoopConfig {
    /// The model to use.
    pub model: model::Model,
    /// Stream options (temperature, max_tokens, reasoning, etc.).
    pub stream_options: SimpleStreamOptions,
    /// Working directory the loop reports to tools via
    /// [`ToolExecutionContext::cwd`]. Defaults to `.` if empty.
    pub cwd: PathBuf,
    /// Session id the loop reports to tools via
    /// [`ToolExecutionContext::session_id`]. Empty string is allowed and
    /// signals "no associated session".
    pub session_id: String,
    /// Tool execution mode.
    pub tool_execution: ToolExecutionMode,
    /// Hook: called before each tool execution.
    pub before_tool_call: Option<BeforeToolCallHook>,
    /// Hook: called after each tool execution.
    pub after_tool_call: Option<AfterToolCallHook>,
    /// Hook: returns steering messages to inject mid-run.
    pub get_steering_messages: Option<GetSteeringMessagesFn>,
    /// Hook: returns follow-up messages after agent would stop.
    pub get_follow_up_messages: Option<GetFollowUpMessagesFn>,
    /// Convert messages before sending to LLM.
    pub convert_to_llm: Option<ConvertToLlmFn>,
    /// Transform context before convertToLlm (two-phase transformation).
    pub transform_context: Option<TransformContextFn>,
    /// Dynamic API key resolver for OAuth tokens.
    pub get_api_key: Option<GetApiKeyFn>,
    /// How steering messages are dequeued.
    pub steering_mode: QueueDeliveryMode,
    /// How follow-up messages are dequeued.
    pub follow_up_mode: QueueDeliveryMode,
    /// Maximum retry delay in ms (for server-requested delays).
    pub max_retry_delay_ms: Option<u64>,
}

impl std::fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("tool_execution", &self.tool_execution)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Agent state
// ---------------------------------------------------------------------------

/// Current agent state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentState {
    pub system_prompt: String,
    pub model_id: String,
    pub messages: Vec<Message>,
    pub is_streaming: bool,
    pub error: Option<String>,
    /// Current thinking level for reasoning models.
    pub thinking_level: Option<model::ThinkingLevel>,
}

/// Extract tool calls from an assistant message's content blocks.
pub fn extract_tool_calls(message: &AssistantMessage) -> Vec<&ToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .collect()
}
