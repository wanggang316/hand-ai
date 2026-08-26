//! Wire-format tests: pin `AssistantMessageEvent` JSON to its canonical
//! shape.
//!
//! Variant tags use `snake_case` for textual variants but `toolcall_*`
//! (single word, NOT `tool_call_*`) for the three tool-call variants.
//! All payload fields are `camelCase` (e.g. `contentIndex`, `toolCall`).

use model::types::Provider;
use model::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, StopReason, TextContent,
    ToolCall, Usage,
};
use serde_json::{Value, json};

fn empty_assistant_message() -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".to_string(),
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

fn type_tag(value: &Value) -> &str {
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
}

#[test]
fn start_serializes_with_snake_case_tag() {
    let event = AssistantMessageEvent::Start {
        partial: empty_assistant_message(),
    };
    let value = serde_json::to_value(&event).expect("serialize");
    assert_eq!(type_tag(&value), "start");
    assert!(value.get("partial").is_some());
}

#[test]
fn text_start_serializes_with_camelcase_field() {
    let event = AssistantMessageEvent::TextStart {
        content_index: 7,
        partial: empty_assistant_message(),
    };
    let value = serde_json::to_value(&event).expect("serialize");
    assert_eq!(type_tag(&value), "text_start");
    assert_eq!(
        value.get("contentIndex"),
        Some(&Value::Number(7.into())),
        "expected camelCase contentIndex, got: {value}",
    );
    assert!(
        value.get("content_index").is_none(),
        "snake_case content_index must NOT appear on the wire",
    );
}

#[test]
fn text_delta_emits_camelcase_fields_and_snake_tag() {
    // Note: full round-trip via partial AssistantMessage is blocked by an
    // unrelated pre-existing duplicate-`type`-field issue in TextContent.
    // Verify the wire shape directly.
    let event = AssistantMessageEvent::TextDelta {
        content_index: 1,
        delta: "hi".to_string(),
        partial: empty_assistant_message(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(type_tag(&value), "text_delta");
    assert_eq!(value.get("contentIndex"), Some(&Value::Number(1.into())));
    assert_eq!(value.get("delta"), Some(&Value::String("hi".to_string())));
    assert!(
        value.get("content_index").is_none(),
        "snake_case content_index leaked",
    );
}

#[test]
fn text_end_uses_snake_case_tag() {
    let event = AssistantMessageEvent::TextEnd {
        content_index: 3,
        content: "done".to_string(),
        partial: empty_assistant_message(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(type_tag(&value), "text_end");
    assert_eq!(value.get("contentIndex"), Some(&Value::Number(3.into())));
}

#[test]
fn thinking_start_delta_end_use_snake_case_tags() {
    for (event, tag) in [
        (
            AssistantMessageEvent::ThinkingStart {
                content_index: 0,
                partial: empty_assistant_message(),
            },
            "thinking_start",
        ),
        (
            AssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "x".to_string(),
                partial: empty_assistant_message(),
            },
            "thinking_delta",
        ),
        (
            AssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content: "x".to_string(),
                partial: empty_assistant_message(),
            },
            "thinking_end",
        ),
    ] {
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(type_tag(&value), tag, "wrong tag for {tag}");
        assert!(value.get("contentIndex").is_some());
    }
}

#[test]
fn toolcall_variants_use_compound_tag_not_split() {
    // Wire tag is `toolcall_start` — single word, NOT `tool_call_start`.
    let start = AssistantMessageEvent::ToolCallStart {
        content_index: 0,
        partial: empty_assistant_message(),
    };
    assert_eq!(
        type_tag(&serde_json::to_value(&start).unwrap()),
        "toolcall_start"
    );

    let delta = AssistantMessageEvent::ToolCallDelta {
        content_index: 0,
        delta: "{}".to_string(),
        partial: empty_assistant_message(),
    };
    assert_eq!(
        type_tag(&serde_json::to_value(&delta).unwrap()),
        "toolcall_delta"
    );

    let end = AssistantMessageEvent::ToolCallEnd {
        content_index: 0,
        tool_call: ToolCall::new("call_123", "echo", json!({"text": "hi"})),
        partial: empty_assistant_message(),
    };
    let end_value = serde_json::to_value(&end).unwrap();
    assert_eq!(type_tag(&end_value), "toolcall_end");
    assert!(
        end_value.get("toolCall").is_some(),
        "expected camelCase toolCall field, got: {end_value}",
    );
    assert!(
        end_value.get("tool_call").is_none(),
        "snake_case tool_call must NOT appear on the wire",
    );
}

#[test]
fn done_and_error_have_camelcase_fields() {
    let done = AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: empty_assistant_message(),
    };
    let value = serde_json::to_value(&done).unwrap();
    assert_eq!(type_tag(&value), "done");
    assert_eq!(
        value.get("reason"),
        Some(&Value::String("stop".to_string()))
    );

    let err = AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: empty_assistant_message(),
    };
    assert_eq!(type_tag(&serde_json::to_value(&err).unwrap()), "error");
}

#[test]
fn deserializes_canonical_wire_shaped_json() {
    // A canonical SSE frame. Use empty
    // content array to sidestep the pre-existing TextContent dup-tag bug.
    let payload = json!({
        "type": "toolcall_end",
        "contentIndex": 2,
        "toolCall": {
            "type": "toolCall",
            "id": "call_abc",
            "name": "bash",
            "arguments": {"cmd": "ls"}
        },
        "partial": {
            "content": [],
            "api": "openai-completions",
            "provider": "openai",
            "model": "gpt-4",
            "usage": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": {
                    "input": 0.0,
                    "output": 0.0,
                    "cacheRead": 0.0,
                    "cacheWrite": 0.0,
                    "total": 0.0
                }
            },
            "stopReason": "stop",
            "timestamp": 1700000000000u64
        }
    });
    let parsed: AssistantMessageEvent = serde_json::from_value(payload).expect("parse");
    match parsed {
        AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            ..
        } => {
            assert_eq!(content_index, 2);
            assert_eq!(tool_call.id, "call_abc");
            assert_eq!(tool_call.name, "bash");
        }
        other => panic!("expected ToolCallEnd, got {other:?}"),
    }
}

/// The raw stop reason is additive on the wire: it serializes under
/// `rawStopReason` when present and disappears entirely when absent, so
/// a consumer written against the previous shape sees no change and a
/// payload written by one keeps parsing.
#[test]
fn raw_stop_reason_is_additive_on_the_wire() {
    use model::{Api, AssistantMessage, StopReason, Usage, types::Provider};

    let mut msg = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::AnthropicMessages,
        provider: Provider::Anthropic,
        model: "claude-test".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::Length,
        raw_stop_reason: None,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    let without = serde_json::to_value(&msg).expect("serialize");
    assert!(
        without.get("rawStopReason").is_none(),
        "absent reason must not reach the wire: {without}"
    );

    msg.raw_stop_reason = Some("max_tokens".to_string());
    let with = serde_json::to_value(&msg).expect("serialize");
    assert_eq!(
        with.get("rawStopReason").and_then(|v| v.as_str()),
        Some("max_tokens")
    );

    // A payload from before the field existed still parses.
    let legacy: AssistantMessage = serde_json::from_value(without).expect("parse legacy payload");
    assert!(legacy.raw_stop_reason.is_none());
    assert_eq!(legacy.stop_reason, StopReason::Length);
}
