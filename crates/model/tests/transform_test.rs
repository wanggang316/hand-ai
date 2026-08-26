//! Integration tests for `model::transform`.
//!
//! Covers the cross-provider refresh introduced in M3: image-bearing tool
//! result downgrade, Gemini-3 cross-API thought-signature drop semantics
//! (see ExecPlan D-07), response-id normalization on cross-API handoffs,
//! and the eager tool-input-streaming compat helper.

use model::transform::{supports_eager_tool_input_streaming, transform_messages};
use model::types::{
    AnthropicMessagesCompat, Api, AssistantContentBlock, AssistantMessage, Compat, Cost,
    ImageContent, InputType, Message, Model, Provider, StopReason, TextContent, ToolCall,
    ToolResultContent, ToolResultMessage, Usage,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn anthropic_target() -> Model {
    Model {
        id: "claude-sonnet-4-20250514".into(),
        name: "Claude Sonnet 4".into(),
        api: Api::AnthropicMessages,
        provider: Provider::Anthropic,
        base_url: String::new(),
        reasoning: true,
        input: vec![InputType::Text],
        cost: Cost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        },
        context_window: 200_000,
        max_tokens: 16384,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

fn anthropic_target_with_image() -> Model {
    let mut m = anthropic_target();
    m.input = vec![InputType::Text, InputType::Image];
    m
}

fn gemini_target() -> Model {
    Model {
        id: "gemini-3-pro".into(),
        name: "Gemini 3 Pro".into(),
        api: Api::GoogleGenerativeAi,
        provider: Provider::Google,
        base_url: String::new(),
        reasoning: true,
        input: vec![InputType::Text, InputType::Image],
        cost: Cost {
            input: 1.0,
            output: 4.0,
            cache_read: 0.1,
            cache_write: 1.0,
        },
        context_window: 1_000_000,
        max_tokens: 32_000,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

fn make_assistant(
    content: Vec<AssistantContentBlock>,
    provider: Provider,
    api: Api,
    model_id: &str,
) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content,
        api,
        provider,
        model: model_id.into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        raw_stop_reason: None,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

// ---------------------------------------------------------------------------
// Image-bearing tool-result routing
// ---------------------------------------------------------------------------

#[test]
fn image_tool_result_downgrades_for_text_only_target() {
    let model = anthropic_target();
    let messages = vec![Message::ToolResult(ToolResultMessage::new(
        "tc1",
        "screenshot",
        vec![
            ToolResultContent::Text(TextContent::new("here is the screenshot:")),
            ToolResultContent::Image(ImageContent::new("base64data", "image/png")),
        ],
    ))];

    let result = transform_messages(&messages, &model, None);
    assert_eq!(result.len(), 1);
    let Message::ToolResult(tr) = &result[0] else {
        panic!("expected tool result");
    };
    assert_eq!(tr.content.len(), 2);
    assert!(matches!(
        &tr.content[0],
        ToolResultContent::Text(t) if t.text == "here is the screenshot:"
    ));
    assert!(matches!(
        &tr.content[1],
        ToolResultContent::Text(t) if t.text == "(tool image omitted: model does not support images)"
    ));
}

#[test]
fn image_tool_result_passthrough_for_image_capable_target() {
    let model = anthropic_target_with_image();
    let messages = vec![Message::ToolResult(ToolResultMessage::new(
        "tc1",
        "screenshot",
        vec![ToolResultContent::Image(ImageContent::new(
            "base64data",
            "image/png",
        ))],
    ))];

    let result = transform_messages(&messages, &model, None);
    let Message::ToolResult(tr) = &result[0] else {
        panic!("expected tool result");
    };
    assert_eq!(tr.content.len(), 1);
    assert!(matches!(&tr.content[0], ToolResultContent::Image(_)));
}

// ---------------------------------------------------------------------------
// Gemini-3 cross-API thought-signature handling (see ExecPlan D-07)
// ---------------------------------------------------------------------------

#[test]
fn gemini3_cross_api_drops_thought_signature() {
    // Cross-API: a tool call from a non-Google source with no
    // `thought_signature` must remain `None` after transformation. Vertex/
    // Gemini-3 reject fabricated sentinels (CHANGELOG 4032), so the pipeline
    // simply drops foreign signatures rather than replacing them.
    let model = gemini_target();
    let mut tc = ToolCall::new("call_42", "read", serde_json::json!({"path": "/x"}));
    tc.thought_signature = None;

    let messages = vec![Message::Assistant(make_assistant(
        vec![AssistantContentBlock::ToolCall(tc)],
        Provider::OpenAI,
        Api::OpenAIResponses,
        "gpt-4o",
    ))];

    let result = transform_messages(&messages, &model, None);
    let Message::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    let AssistantContentBlock::ToolCall(tc) = &a.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(tc.thought_signature, None);
}

#[test]
fn gemini3_same_model_preserves_signature() {
    // Same-model Google replay: the source signature is real and valid for
    // this target, so the pipeline must leave it untouched (no legacy strip).
    let model = gemini_target();
    let mut tc = ToolCall::new("call_99", "read", serde_json::json!({}));
    tc.thought_signature = Some("origin-sig".into());

    let messages = vec![Message::Assistant(make_assistant(
        vec![AssistantContentBlock::ToolCall(tc)],
        model.provider,
        model.api,
        &model.id,
    ))];

    let result = transform_messages(&messages, &model, None);
    let Message::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    let AssistantContentBlock::ToolCall(tc) = &a.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(tc.thought_signature.as_deref(), Some("origin-sig"));
}

#[test]
fn gemini3_foreign_signature_dropped() {
    // Cross-API: an OpenAI assistant whose tool call already carries a
    // foreign signature is replayed against Google. The opaque OpenAI value
    // is meaningless to Gemini and must be dropped to None — never replaced
    // with a fabricated sentinel (see ExecPlan D-07).
    let model = gemini_target();
    let mut tc = ToolCall::new("call_1", "search", serde_json::json!({}));
    tc.thought_signature = Some("openai-sig-xyz".into());

    let messages = vec![Message::Assistant(make_assistant(
        vec![AssistantContentBlock::ToolCall(tc)],
        Provider::OpenAI,
        Api::OpenAIResponses,
        "gpt-4o",
    ))];

    let result = transform_messages(&messages, &model, None);
    let Message::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    let AssistantContentBlock::ToolCall(tc) = &a.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(
        tc.thought_signature, None,
        "foreign Gemini signature must be dropped to None, not replaced with a fabricated sentinel",
    );
}

// ---------------------------------------------------------------------------
// Response-id normalization
// ---------------------------------------------------------------------------

#[test]
fn response_id_dropped_on_cross_api_handoff() {
    let model = anthropic_target();
    let mut assistant = make_assistant(
        vec![AssistantContentBlock::Text(TextContent::new("hi"))],
        Provider::OpenAI,
        Api::OpenAIResponses,
        "gpt-4o",
    );
    assistant.response_id = Some("resp_abc".into());

    let messages = vec![Message::Assistant(assistant)];

    let result = transform_messages(&messages, &model, None);
    let Message::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    assert!(a.response_id.is_none());
}

#[test]
fn response_id_preserved_on_same_api() {
    let model = anthropic_target();
    let mut assistant = make_assistant(
        vec![AssistantContentBlock::Text(TextContent::new("hi"))],
        Provider::Anthropic,
        Api::AnthropicMessages,
        "claude-sonnet-4-20250514",
    );
    assistant.response_id = Some("msg_keep".into());

    let messages = vec![Message::Assistant(assistant)];

    let result = transform_messages(&messages, &model, None);
    let Message::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    assert_eq!(a.response_id.as_deref(), Some("msg_keep"));
}

// ---------------------------------------------------------------------------
// Eager tool-input streaming compat helper
// ---------------------------------------------------------------------------

#[test]
fn supports_eager_tool_input_streaming_default_true_for_anthropic() {
    let model = anthropic_target();
    assert!(supports_eager_tool_input_streaming(&model));
}

#[test]
fn supports_eager_tool_input_streaming_explicit_false() {
    let mut model = anthropic_target();
    model.compat = Some(Compat::AnthropicMessages(AnthropicMessagesCompat {
        supports_eager_tool_input_streaming: Some(false),
        supports_long_cache_retention: None,
    }));
    assert!(!supports_eager_tool_input_streaming(&model));
}
