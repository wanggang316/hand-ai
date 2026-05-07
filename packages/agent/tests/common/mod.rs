//! Shared test utilities for the agent crate.
#![allow(dead_code)]

use hand_agent::{AgentEvent, AgentEventSink, AgentTool, ToolResult};
use model::types::Provider;
use model::{
    Api, ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Context, Cost, InputType, Message, Model, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ToolCall, Usage,
};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Models / messages
// ---------------------------------------------------------------------------

pub fn test_model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        base_url: "https://api.test.com".into(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 0.75,
        },
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

pub fn test_assistant_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    }
}

pub fn test_assistant_message_with_tool_call(
    tool_name: &str,
    tool_id: &str,
    args: serde_json::Value,
) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            tool_id, tool_name, args,
        ))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    }
}

pub fn test_assistant_message_with_tool_calls(
    calls: Vec<(&str, &str, serde_json::Value)>,
) -> AssistantMessage {
    let content = calls
        .into_iter()
        .map(|(name, id, args)| AssistantContentBlock::ToolCall(ToolCall::new(id, name, args)))
        .collect();
    AssistantMessage {
        role: "assistant".into(),
        content,
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    }
}

// ---------------------------------------------------------------------------
// Mock providers
// ---------------------------------------------------------------------------

/// Returns a fixed text response on every call.
pub struct MockTextProvider {
    pub response_text: String,
}

impl MockTextProvider {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            response_text: text.into(),
        }
    }
}

impl ApiProvider for MockTextProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let text = self.response_text.clone();
        Box::pin(async_stream::stream! {
            let partial = test_assistant_message("");
            yield AssistantMessageEvent::Start { partial: partial.clone() };
            yield AssistantMessageEvent::TextStart { content_index: 0, partial: partial.clone() };
            yield AssistantMessageEvent::TextDelta { content_index: 0, delta: text.clone(), partial: partial.clone() };
            let final_msg = test_assistant_message(&text);
            yield AssistantMessageEvent::TextEnd { content_index: 0, content: text.clone(), partial: final_msg.clone() };
            yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: final_msg };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Returns a single tool call on the first call, then plain text on subsequent calls.
pub struct MockToolProvider {
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub final_text: String,
}

impl MockToolProvider {
    pub fn new(
        tool_name: impl Into<String>,
        tool_args: serde_json::Value,
        final_text: impl Into<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            tool_args,
            final_text: final_text.into(),
        }
    }
}

impl ApiProvider for MockToolProvider {
    fn stream(
        &self,
        _model: Model,
        context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let has_tool_result = context
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(_)));

        if has_tool_result {
            let text = self.final_text.clone();
            Box::pin(async_stream::stream! {
                let msg = test_assistant_message(&text);
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg };
            })
        } else {
            let tool_name = self.tool_name.clone();
            let tool_args = self.tool_args.clone();
            Box::pin(async_stream::stream! {
                let msg = test_assistant_message_with_tool_call(&tool_name, "call_1", tool_args);
                let tc = match &msg.content[0] {
                    AssistantContentBlock::ToolCall(tc) => tc.clone(),
                    _ => unreachable!(),
                };
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::ToolCallStart { content_index: 0, partial: msg.clone() };
                yield AssistantMessageEvent::ToolCallEnd {
                    content_index: 0,
                    tool_call: tc,
                    partial: msg.clone(),
                };
                yield AssistantMessageEvent::Done { reason: StopReason::ToolUse, message: msg };
            })
        }
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Returns multiple tool calls on the first invocation, then plain text.
pub struct MockMultiToolProvider {
    pub tool_calls: Vec<(String, String, serde_json::Value)>, // (name, id, args)
    pub final_text: String,
}

impl MockMultiToolProvider {
    pub fn new(
        tool_calls: Vec<(impl Into<String>, impl Into<String>, serde_json::Value)>,
        final_text: impl Into<String>,
    ) -> Self {
        Self {
            tool_calls: tool_calls
                .into_iter()
                .map(|(n, i, a)| (n.into(), i.into(), a))
                .collect(),
            final_text: final_text.into(),
        }
    }
}

impl ApiProvider for MockMultiToolProvider {
    fn stream(
        &self,
        _model: Model,
        context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let has_tool_result = context
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(_)));

        if has_tool_result {
            let text = self.final_text.clone();
            Box::pin(async_stream::stream! {
                let msg = test_assistant_message(&text);
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg };
            })
        } else {
            let calls = self.tool_calls.clone();
            Box::pin(async_stream::stream! {
                let blocks: Vec<AssistantContentBlock> = calls
                    .iter()
                    .map(|(n, i, a)| AssistantContentBlock::ToolCall(ToolCall::new(i.clone(), n.clone(), a.clone())))
                    .collect();
                let mut msg = test_assistant_message("");
                msg.content = blocks;
                msg.stop_reason = StopReason::ToolUse;
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done { reason: StopReason::ToolUse, message: msg };
            })
        }
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

pub struct MockErrorProvider {
    pub error_message: String,
}

impl ApiProvider for MockErrorProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let error_msg = self.error_message.clone();
        Box::pin(async_stream::stream! {
            let mut msg = test_assistant_message("");
            msg.stop_reason = StopReason::Error;
            msg.error_message = Some(error_msg);
            yield AssistantMessageEvent::Error { reason: StopReason::Error, error: msg };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Provider that closes the stream after `Start` without ever producing `Done`/`Error`.
/// Closes the stream after `Start` without `Done`/`Error`; the loop must
/// synthesize an error assistant in place and emit a balanced `MessageEnd`.
pub struct TruncatedStreamProvider;

impl ApiProvider for TruncatedStreamProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        Box::pin(async_stream::stream! {
            let partial = test_assistant_message("");
            yield AssistantMessageEvent::Start { partial };
            // Stream ends here without Done/Error.
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Provider that sleeps before producing a (non-tool) text response. Used to test cancellation.
pub struct SlowTextProvider {
    pub delay_ms: u64,
    pub response_text: String,
}

impl ApiProvider for SlowTextProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let delay_ms = self.delay_ms;
        let text = self.response_text.clone();
        Box::pin(async_stream::stream! {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let msg = test_assistant_message(&text);
            yield AssistantMessageEvent::Start { partial: msg.clone() };
            yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// A simple echo tool using the `simple` constructor.
pub fn echo_tool() -> AgentTool {
    AgentTool::simple(
        "echo",
        "Echoes back the input",
        serde_json::json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        }),
        "Echo",
        |_id, args| async move {
            let msg = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("no message");
            ToolResult::text(format!("Echo: {msg}"))
        },
    )
}

/// A tool that sleeps for `delay_ms` and returns a label. Used for parallelism tests.
pub fn sleep_tool(name: &'static str, delay_ms: u64) -> AgentTool {
    AgentTool::simple(
        name,
        "Sleeps for a fixed duration and returns a label",
        serde_json::json!({ "type": "object", "properties": {} }),
        "Sleep",
        move |_id, _args| async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            ToolResult::text(format!("done: {name}"))
        },
    )
}

/// Tool that always returns terminate=true.
pub fn terminate_tool(name: &'static str) -> AgentTool {
    AgentTool::simple(
        name,
        "Returns terminate=true",
        serde_json::json!({ "type": "object", "properties": {} }),
        "Terminate",
        move |_id, _args| async move { ToolResult::text("stopping").with_terminate(true) },
    )
}

// ---------------------------------------------------------------------------
// Event collection helpers
// ---------------------------------------------------------------------------

pub fn collecting_event_sink() -> (AgentEventSink, Arc<Mutex<Vec<AgentEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let sink: AgentEventSink = Arc::new(move |event: AgentEvent| {
        events_clone.lock().unwrap().push(event);
    });
    (sink, events)
}

pub fn event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

pub fn event_kinds(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(event_kind).collect()
}
