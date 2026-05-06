//! Shared test utilities for the agent crate.

use hand_agent::{AgentEvent, AgentTool, BoxFuture, ToolResult};
use model::types::Provider;
use model::{
    Api, ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Context, Cost, InputType, Message, Model, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ToolCall, Usage,
};

/// Create a minimal test model.
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
    }
}

/// Create a test assistant message with text content.
pub fn test_assistant_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    }
}

/// Create a test assistant message with a tool call.
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
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
    }
}

/// Mock provider that returns a fixed text response.
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

/// Mock provider that returns a tool call, then text on the second call.
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
        // Check if there are tool results in context — if so, return text
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
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::ToolCallStart { content_index: 0, partial: msg.clone() };
                yield AssistantMessageEvent::ToolCallEnd {
                    content_index: 0,
                    tool_call: msg.content[0].clone().into_tool_call().unwrap(),
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

/// Mock provider that returns an error.
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

/// Create a simple echo tool for testing.
pub fn echo_tool() -> AgentTool {
    AgentTool::new(
        "echo",
        "Echoes back the input",
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        }),
        "Echo",
        Box::new(|_id, args| -> BoxFuture<'static, ToolResult> {
            Box::pin(async move {
                let msg = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("no message");
                ToolResult::text(format!("Echo: {msg}"))
            })
        }),
    )
}

/// Create a calculator tool for testing.
#[allow(dead_code)] // Used by some integration tests; not all targets see it.
pub fn calculator_tool() -> AgentTool {
    AgentTool::new(
        "calculate",
        "Performs basic math",
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string" }
            },
            "required": ["expression"]
        }),
        "Calculator",
        Box::new(|_id, args| -> BoxFuture<'static, ToolResult> {
            Box::pin(async move {
                let expr = args
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0");
                ToolResult::text(format!("Result: {expr} = 42"))
            })
        }),
    )
}

/// Collect events from an AgentEventSink into a shared vec.
#[allow(dead_code)] // Used by some integration tests; not all targets see it.
pub fn collecting_event_sink() -> (
    hand_agent::AgentEventSink,
    std::sync::Arc<std::sync::Mutex<Vec<AgentEvent>>>,
) {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let sink: hand_agent::AgentEventSink = Box::new(move |event: AgentEvent| {
        events_clone.lock().unwrap().push(event);
    });
    (sink, events)
}

/// Helper trait to extract ToolCall from AssistantContentBlock.
trait IntoToolCall {
    fn into_tool_call(self) -> Option<ToolCall>;
}

impl IntoToolCall for AssistantContentBlock {
    fn into_tool_call(self) -> Option<ToolCall> {
        match self {
            AssistantContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        }
    }
}
