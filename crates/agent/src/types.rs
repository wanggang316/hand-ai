//! Core types for the agent runtime.
//!
//! These types mirror `pi-agent-core` (TypeScript) with idiomatic Rust shapes:
//! tagged enums for unions, traits/Fn-objects for callbacks, `Result` for errors,
//! and a `CancellationToken` threaded through every async boundary.

use jsonschema::JSONSchema;
use model::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, SimpleStreamOptions,
    TextContent, ToolCall, ToolResultContent, ToolResultMessage,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;

/// Lazily-compiled JSON Schema for a tool's `parameters`.
///
/// Stored on `AgentTool` so the compile cost is paid once per tool, not once
/// per tool call. `Ok(None)` means the tool's schema is empty/non-object and
/// validation is a no-op; `Err` carries the compile error message.
pub(crate) type CompiledSchemaCell = Arc<OnceLock<Result<Option<Arc<JSONSchema>>, String>>>;

/// A boxed, sendable future used by async hooks and tool executors.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed error returned by tool executors.
pub type ToolError = Box<dyn std::error::Error + Send + Sync>;

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

/// How queued steering / follow-up messages are dequeued.
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content blocks (text/image) returned to the model.
    pub content: Vec<ToolResultContent>,
    /// Opaque structured details for UI / logging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Hint that the agent should stop after the current tool batch.
    /// Early termination only happens when *every* finalized tool result in the batch sets this to true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl ToolResult {
    /// Create a simple text result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text(TextContent::new(text))],
            details: None,
            terminate: None,
        }
    }

    /// Create an error result (still returned via `Ok`; the loop marks it as `is_error`).
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::Text(TextContent::new(message))],
            details: None,
            terminate: None,
        }
    }

    /// Builder helper that sets `terminate = true`.
    pub fn with_terminate(mut self, terminate: bool) -> Self {
        self.terminate = Some(terminate);
        self
    }

    /// Builder helper that attaches a structured `details` payload to
    /// the result. Hosts (the TUI, CLI loggers, RPC clients) read this
    /// field for non-text affordances — truncation metadata,
    /// rate-limit info, side-channel hints — without parsing the
    /// human-readable content blocks. The value is treated opaquely
    /// by the agent loop and forwarded to the consumer verbatim.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Callback used by tools to stream partial execution updates.
/// The agent loop forwards each call as `AgentEvent::ToolExecutionUpdate`.
pub type OnUpdate = Arc<dyn Fn(ToolResult) + Send + Sync>;

/// Context passed to a tool's `execute` closure.
pub struct ToolExecuteCtx {
    pub tool_call_id: String,
    pub args: serde_json::Value,
    pub cancel: CancellationToken,
    pub on_update: OnUpdate,
}

/// Async function that executes a tool call.
///
/// Returning `Err` is wrapped by the loop into an error `ToolResult` (mirrors TS try/catch).
pub type ToolExecuteFn =
    Box<dyn Fn(ToolExecuteCtx) -> BoxFuture<'static, Result<ToolResult, ToolError>> + Send + Sync>;

/// Optional shim that pre-processes raw arguments before schema validation.
pub type PrepareArgumentsFn = Box<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>;

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
    /// Per-tool execution mode override. `None` means "use the loop default".
    pub execution_mode: Option<ToolExecutionMode>,
    /// Optional argument-preparation shim.
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    /// The async execute function.
    pub execute: ToolExecuteFn,
    /// Cached compiled schema; populated on first tool call.
    pub(crate) compiled_schema: CompiledSchemaCell,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("label", &self.label)
            .field("execution_mode", &self.execution_mode)
            .finish()
    }
}

impl AgentTool {
    /// Create a new agent tool with a fully featured executor.
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
            execution_mode: None,
            prepare_arguments: None,
            execute,
            compiled_schema: Arc::new(OnceLock::new()),
        }
    }

    /// Lazily compile and cache the JSON Schema. Returns `None` when the schema
    /// is empty or non-object (validation skipped).
    pub(crate) fn compiled_schema(&self) -> Result<Option<Arc<JSONSchema>>, String> {
        self.compiled_schema
            .get_or_init(|| {
                let schema = &self.parameters;
                let Some(obj) = schema.as_object() else {
                    return Ok(None);
                };
                if obj.is_empty() {
                    return Ok(None);
                }
                JSONSchema::options()
                    .with_draft(jsonschema::Draft::Draft7)
                    .compile(schema)
                    .map(|c| Some(Arc::new(c)))
                    .map_err(|e| format!("schema compile error: {e}"))
            })
            .clone()
    }

    /// Convenience constructor for tools that don't need cancellation or progress updates,
    /// and that report failure via `ToolResult::error(...)` instead of returning `Err`.
    ///
    /// The closure receives `(tool_call_id, args)` and returns a future of `ToolResult`.
    pub fn simple<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        label: impl Into<String>,
        f: F,
    ) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let execute: ToolExecuteFn = Box::new(move |ctx: ToolExecuteCtx| {
            let fut = f(ctx.tool_call_id, ctx.args);
            Box::pin(async move { Ok(fut.await) })
        });
        Self::new(name, description, parameters, label, execute)
    }

    /// Builder: set the per-tool execution mode.
    pub fn with_execution_mode(mut self, mode: ToolExecutionMode) -> Self {
        self.execution_mode = Some(mode);
        self
    }

    /// Builder: install a `prepare_arguments` shim.
    pub fn with_prepare_arguments(mut self, prepare: PrepareArgumentsFn) -> Self {
        self.prepare_arguments = Some(prepare);
        self
    }

    /// Convert to a `model::Tool` for inclusion in the LLM context.
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

/// Snapshot passed into the low-level agent loop.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// Agent events
// ---------------------------------------------------------------------------

/// Events emitted during agent execution for UI updates.
///
/// `agent_end` is the final event for a run; observers may rely on seeing it for
/// every run including failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: Message,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: Message,
        assistant_message_event: Box<AssistantMessageEvent>,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
        partial_result: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}

// ---------------------------------------------------------------------------
// Hook contexts and types
// ---------------------------------------------------------------------------

/// Result returned from `before_tool_call`.
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallResult {
    /// If true, the tool call is blocked and an error result is emitted.
    pub block: bool,
    /// Reason shown in the error result when blocked.
    pub reason: Option<String>,
}

/// Partial override returned from `after_tool_call`.
///
/// Field-by-field merge: `Some(_)` replaces, `None` keeps the original value.
#[derive(Debug, Clone, Default)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<ToolResultContent>>,
    pub details: Option<serde_json::Value>,
    pub is_error: Option<bool>,
    pub terminate: Option<bool>,
}

/// Context passed to `before_tool_call`.
pub struct BeforeToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCall,
    pub args: &'a serde_json::Value,
    pub context: &'a AgentContext,
}

/// Context passed to `after_tool_call`.
pub struct AfterToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCall,
    pub args: &'a serde_json::Value,
    pub result: &'a ToolResult,
    pub is_error: bool,
    pub context: &'a AgentContext,
}

/// Context passed to `should_stop_after_turn`.
pub struct ShouldStopAfterTurnContext<'a> {
    pub message: &'a AssistantMessage,
    pub tool_results: &'a [ToolResultMessage],
    pub context: &'a AgentContext,
    pub new_messages: &'a [Message],
}

/// Async hook called before each tool execution.
pub type BeforeToolCallHook = Arc<
    dyn for<'a> Fn(
            BeforeToolCallContext<'a>,
            CancellationToken,
        ) -> BoxFuture<'a, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;

/// Async hook called after each tool execution.
pub type AfterToolCallHook = Arc<
    dyn for<'a> Fn(
            AfterToolCallContext<'a>,
            CancellationToken,
        ) -> BoxFuture<'a, Option<AfterToolCallResult>>
        + Send
        + Sync,
>;

/// Async hook that decides whether to stop after a turn.
pub type ShouldStopAfterTurnFn = Arc<
    dyn for<'a> Fn(ShouldStopAfterTurnContext<'a>, CancellationToken) -> BoxFuture<'a, bool>
        + Send
        + Sync,
>;

/// Async hook returning steering messages to inject mid-run.
pub type GetSteeringMessagesFn = Arc<dyn Fn() -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Async hook returning follow-up messages to process after the agent would stop.
pub type GetFollowUpMessagesFn = Arc<dyn Fn() -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Convert agent messages to LLM-compatible messages.
pub type ConvertToLlmFn =
    Arc<dyn Fn(Vec<Message>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Transform context before LLM call (applied before `convert_to_llm`).
pub type TransformContextFn =
    Arc<dyn Fn(Vec<Message>, CancellationToken) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Dynamic API key resolver (e.g. for short-lived OAuth tokens).
pub type GetApiKeyFn = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// Custom streaming transport. When set on `AgentLoopConfig` (or
/// `AgentOptions`), the loop calls this in place of
/// `model::Client::stream_simple`. The closure must be `'static + Send + Sync`
/// because the loop captures it across `.await` points and may share it
/// across tasks.
pub type StreamFn = Arc<
    dyn Fn(
            &model::Model,
            model::Context,
            model::SimpleStreamOptions,
            tokio_util::sync::CancellationToken,
        ) -> model::AssistantMessageEventStream<'static>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Agent loop configuration
// ---------------------------------------------------------------------------

/// Configuration for the agent loop.
#[derive(Clone)]
pub struct AgentLoopConfig {
    /// Active model.
    pub model: model::Model,
    /// Stream options (temperature, max_tokens, reasoning, etc.).
    pub stream_options: SimpleStreamOptions,
    /// Default tool execution mode. May be downgraded by per-tool overrides.
    pub tool_execution: ToolExecutionMode,
    pub before_tool_call: Option<BeforeToolCallHook>,
    pub after_tool_call: Option<AfterToolCallHook>,
    pub should_stop_after_turn: Option<ShouldStopAfterTurnFn>,
    pub get_steering_messages: Option<GetSteeringMessagesFn>,
    pub get_follow_up_messages: Option<GetFollowUpMessagesFn>,
    pub convert_to_llm: Option<ConvertToLlmFn>,
    pub transform_context: Option<TransformContextFn>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub steering_mode: QueueDeliveryMode,
    pub follow_up_mode: QueueDeliveryMode,
    pub max_retry_delay_ms: Option<u64>,
    /// Optional custom streaming transport. When `None`, the loop calls
    /// `client.stream_simple(...)`; when `Some`, it calls the closure with
    /// the same arguments and uses the returned stream.
    pub stream_fn: Option<StreamFn>,
}

impl AgentLoopConfig {
    /// Create a config with sensible defaults for `model` and `stream_options`.
    pub fn new(model: model::Model, stream_options: SimpleStreamOptions) -> Self {
        Self {
            model,
            stream_options,
            tool_execution: ToolExecutionMode::default(),
            before_tool_call: None,
            after_tool_call: None,
            should_stop_after_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            convert_to_llm: None,
            transform_context: None,
            get_api_key: None,
            steering_mode: QueueDeliveryMode::default(),
            follow_up_mode: QueueDeliveryMode::default(),
            max_retry_delay_ms: None,
            stream_fn: None,
        }
    }
}

impl std::fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("tool_execution", &self.tool_execution)
            .field("steering_mode", &self.steering_mode)
            .field("follow_up_mode", &self.follow_up_mode)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Agent state
// ---------------------------------------------------------------------------

/// Snapshot of the agent's runtime state.
///
/// Returned by `Agent::state()` as a cheap clone of the live state. The fields
/// mirror the TypeScript `AgentState` reactively-tracked surface.
#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub system_prompt: String,
    pub model_id: String,
    pub messages: Vec<Message>,
    pub thinking_level: Option<model::ThinkingLevel>,
    pub is_streaming: bool,
    pub error: Option<String>,
    /// Partial assistant message currently being streamed, if any.
    pub streaming_message: Option<Message>,
    /// IDs of tool calls currently executing.
    pub pending_tool_calls: HashSet<String>,
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
