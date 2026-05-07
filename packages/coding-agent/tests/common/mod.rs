//! Shared test utilities for the coding-agent crate.
//!
//! Mirrors the conventions used in `packages/agent/tests/common/mod.rs` but
//! is scoped to coding-agent's needs. The agent crate's `common` module is
//! private to that crate's tests, so we re-implement the equivalents here.

// Cargo compiles `tests/common/mod.rs` once per integration test binary; any
// helper that's unused by a particular binary triggers `dead_code` warnings.
// The blanket allow is the standard pattern for shared test helpers.
#![allow(dead_code)]

use model::types::Provider;
use model::{
    Api, ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Context, Cost, InputType, Model, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, ToolCall, Usage,
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
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

/// Build a fully formed assistant message containing a single text block.
fn assistant_text_message(text: &str) -> AssistantMessage {
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
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

/// Build an assistant message containing a single tool-call block.
fn assistant_tool_call_message(
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
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

/// Mock provider that streams a single text response.
///
/// Emits the full `Start` -> `TextStart` -> `TextDelta` -> `TextEnd` -> `Done`
/// sequence ending with `StopReason::Stop`.
struct MockTextProvider {
    text: String,
}

impl ApiProvider for MockTextProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let text = self.text.clone();
        Box::pin(async_stream::stream! {
            let partial = assistant_text_message("");
            yield AssistantMessageEvent::Start { partial: partial.clone() };
            yield AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: partial.clone(),
            };
            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: text.clone(),
                partial: partial.clone(),
            };

            let final_msg = assistant_text_message(&text);
            yield AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: text.clone(),
                partial: final_msg.clone(),
            };
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: final_msg,
            };
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

/// Mock provider that streams a single tool-call response and stops with
/// `StopReason::ToolUse`.
struct MockToolProvider {
    tool_name: String,
    args: serde_json::Value,
}

impl ApiProvider for MockToolProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let tool_name = self.tool_name.clone();
        let args = self.args.clone();
        Box::pin(async_stream::stream! {
            let msg = assistant_tool_call_message(&tool_name, "call_1", args);
            let tool_call = match &msg.content[0] {
                AssistantContentBlock::ToolCall(tc) => tc.clone(),
                _ => unreachable!("constructed with ToolCall block above"),
            };
            yield AssistantMessageEvent::Start { partial: msg.clone() };
            yield AssistantMessageEvent::ToolCallStart {
                content_index: 0,
                partial: msg.clone(),
            };
            yield AssistantMessageEvent::ToolCallEnd {
                content_index: 0,
                tool_call,
                partial: msg.clone(),
            };
            yield AssistantMessageEvent::Done {
                reason: StopReason::ToolUse,
                message: msg,
            };
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

/// Build a streaming `ApiProvider` that emits a single text turn ending in
/// `StopReason::Stop`.
pub fn mock_text_provider(text: &str) -> Box<dyn ApiProvider> {
    Box::new(MockTextProvider { text: text.into() })
}

/// Build a streaming `ApiProvider` that emits a single tool-call turn ending
/// in `StopReason::ToolUse`.
pub fn mock_tool_provider(tool_name: &str, args: serde_json::Value) -> Box<dyn ApiProvider> {
    Box::new(MockToolProvider {
        tool_name: tool_name.into(),
        args,
    })
}

/// Create a temporary directory suitable for use as an `AgentSession` cwd.
///
/// Returns the `TempDir` so callers control its lifetime; dropping it cleans
/// up the directory automatically (including on test panic).
pub fn temp_session_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("failed to create temp session dir")
}
