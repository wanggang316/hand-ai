//! Integration tests for `model::utils`.
//!
//! Each module is exercised directly through the crate's public surface to
//! confirm that re-exports remain stable.

use std::collections::HashMap;

use futures::StreamExt;
use model::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, ImageContent,
    Message, Provider, StopReason, TextContent, Tool, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, UserContentBlock, UserMessage,
};
use model::utils::sanitize_unicode::{sanitize, sanitize_bytes};
use model::utils::{
    AssistantMessageDiagnostic, DiagnosticKind, EventStream, Provenance, ValidationIssueKind,
    merge_headers, safe_parse_partial, sha256_hex, try_parse_strict, uuid_v7, validate_context,
};

// -----------------------------------------------------------------------------
// json_parse
// -----------------------------------------------------------------------------

#[test]
fn json_parse_strict_roundtrip() {
    let value = try_parse_strict(r#"{"a":1,"b":"x"}"#).expect("strict parse");
    assert_eq!(value["a"], serde_json::json!(1));
    assert_eq!(value["b"], serde_json::json!("x"));
}

#[test]
fn json_parse_partial_incomplete_object() {
    let value = safe_parse_partial(r#"{"a":1, "b":2"#).expect("partial parse");
    assert_eq!(value["a"], serde_json::json!(1));
    assert_eq!(value["b"], serde_json::json!(2));
}

#[test]
fn json_parse_partial_incomplete_array() {
    let value = safe_parse_partial("[1, 2, 3").expect("partial parse");
    assert_eq!(value, serde_json::json!([1, 2, 3]));
}

#[test]
fn json_parse_partial_trailing_comma() {
    let value = safe_parse_partial(r#"{"a":1,"b":2,}"#).expect("partial parse");
    assert_eq!(value["a"], serde_json::json!(1));
    assert_eq!(value["b"], serde_json::json!(2));
}

#[test]
fn json_parse_partial_unterminated_string() {
    let value = safe_parse_partial(r#"{"name":"alice"#).expect("partial parse");
    assert_eq!(value["name"], serde_json::json!("alice"));
}

#[test]
fn json_parse_partial_empty_returns_none() {
    assert!(safe_parse_partial("").is_none());
    assert!(safe_parse_partial("   ").is_none());
}

#[test]
fn json_parse_strict_rejects_garbage() {
    assert!(try_parse_strict("not json").is_none());
}

// -----------------------------------------------------------------------------
// sanitize_unicode
// -----------------------------------------------------------------------------

#[test]
fn sanitize_bytes_replaces_lone_surrogates() {
    let wtf8 = b"hi\xED\xA0\x80world"; // U+D800 high surrogate
    assert_eq!(sanitize_bytes(wtf8), "hi\u{FFFD}world");
}

#[test]
fn sanitize_bytes_preserves_valid_utf8() {
    assert_eq!(sanitize_bytes("héllo".as_bytes()), "héllo");
    assert_eq!(sanitize_bytes("中文".as_bytes()), "中文");
}

#[test]
fn sanitize_str_passthrough() {
    assert_eq!(sanitize("abc"), "abc");
    assert_eq!(sanitize("中文"), "中文");
}

// -----------------------------------------------------------------------------
// validation
// -----------------------------------------------------------------------------

fn user_text(text: &str) -> Message {
    Message::User(UserMessage::new_text(text))
}

fn user_blocks(blocks: Vec<UserContentBlock>) -> Message {
    Message::User(UserMessage::new_blocks(blocks))
}

fn assistant_with(content: Vec<AssistantContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        role: "assistant".to_string(),
        content,
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        raw_stop_reason: None,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    })
}

fn tool_result(id: &str, name: &str) -> Message {
    Message::ToolResult(ToolResultMessage::new(
        id,
        name,
        vec![ToolResultContent::Text(TextContent::new("ok"))],
    ))
}

#[test]
fn validate_orphan_tool_call() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![assistant_with(vec![AssistantContentBlock::ToolCall(
            ToolCall::new("call_1", "search", serde_json::json!({})),
        )])],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::OrphanToolCall),
        "expected OrphanToolCall issue, got {issues:?}"
    );
}

#[test]
fn validate_empty_content() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![user_text("")],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::EmptyContent)
    );
}

#[test]
fn validate_invalid_image() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![user_blocks(vec![UserContentBlock::Image(
            ImageContent::new("", ""),
        )])],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::InvalidImage)
    );
}

#[test]
fn validate_empty_tool_name() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![user_text("hi")],
        tools: Some(vec![Tool::new("", "no name", serde_json::json!({}))]),
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::EmptyToolName)
    );
}

#[test]
fn validate_duplicate_tool_name() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![user_text("hi")],
        tools: Some(vec![
            Tool::new("search", "first", serde_json::json!({})),
            Tool::new("search", "second", serde_json::json!({})),
        ]),
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::DuplicateToolName)
    );
}

#[test]
fn validate_orphan_tool_result() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![tool_result("does_not_exist", "search")],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::OrphanToolResult)
    );
}

#[test]
fn validate_missing_assistant_between_tool_result_and_user() {
    let asst = assistant_with(vec![AssistantContentBlock::ToolCall(ToolCall::new(
        "call_1",
        "search",
        serde_json::json!({}),
    ))]);
    let ctx = Context {
        system_prompt: None,
        messages: vec![asst, tool_result("call_1", "search"), user_text("next")],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::MissingAssistantBetweenToolResultAndUser),
        "expected the missing-assistant issue, got {issues:?}"
    );
}

#[test]
fn validate_clean_context_has_no_issues() {
    let asst = assistant_with(vec![AssistantContentBlock::ToolCall(ToolCall::new(
        "call_1",
        "search",
        serde_json::json!({}),
    ))]);
    let ctx = Context {
        system_prompt: None,
        messages: vec![user_text("hi"), asst, tool_result("call_1", "search")],
        tools: Some(vec![Tool::new("search", "doc", serde_json::json!({}))]),
    };
    let issues = validate_context(&ctx);
    assert!(issues.is_empty(), "unexpected issues: {issues:?}");
}

// -----------------------------------------------------------------------------
// headers
// -----------------------------------------------------------------------------

#[test]
fn merge_headers_case_insensitive_override_wins() {
    let mut default = HashMap::new();
    default.insert("Content-Type".to_string(), "application/json".to_string());
    default.insert("X-Trace".to_string(), "default".to_string());

    let mut overrides = HashMap::new();
    overrides.insert("content-type".to_string(), "text/plain".to_string());

    let merged = merge_headers(&default, Some(&overrides));
    // Override casing wins for the key it provided.
    assert_eq!(merged.get("content-type"), Some(&"text/plain".to_string()));
    // The original `Content-Type` casing must be gone.
    assert!(!merged.contains_key("Content-Type"));
    // Untouched header preserves its casing and value.
    assert_eq!(merged.get("X-Trace"), Some(&"default".to_string()));
}

#[test]
fn merge_headers_no_override_returns_clone() {
    let mut default = HashMap::new();
    default.insert("A".to_string(), "1".to_string());
    let merged = merge_headers(&default, None);
    assert_eq!(merged, default);
}

// -----------------------------------------------------------------------------
// hash
// -----------------------------------------------------------------------------

#[test]
fn sha256_hex_known_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn sha256_hex_empty_input() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

// -----------------------------------------------------------------------------
// uuid
// -----------------------------------------------------------------------------

#[test]
fn uuid_v7_has_rfc_shape() {
    let id = uuid_v7();
    assert_eq!(id.len(), 36, "hyphenated form: {id}");
    let groups: Vec<&str> = id.split('-').collect();
    let lens: Vec<usize> = groups.iter().map(|g| g.len()).collect();
    assert_eq!(lens, vec![8, 4, 4, 4, 12], "group layout: {id}");
    assert!(
        groups.iter().all(|g| g
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
        "lowercase hex only: {id}"
    );
    // Version nibble: first char of the third group.
    assert_eq!(groups[2].as_bytes()[0], b'7', "version nibble: {id}");
    // RFC 4122 variant: top two bits of the fourth group's first byte are 0b10.
    let variant = u8::from_str_radix(&groups[3][..2], 16).unwrap();
    assert_eq!(variant & 0xc0, 0x80, "variant bits: {id}");
}

#[test]
fn uuid_v7_timestamps_are_non_decreasing_and_ids_unique() {
    let a = uuid_v7();
    let b = uuid_v7();
    assert_ne!(a, b, "random tail must differentiate ids");
    // First 48 bits (chars 0..13 including the hyphen) are the unix-ms
    // timestamp; lexicographic order on the hex prefix matches numeric order.
    assert!(
        a[..13] <= b[..13],
        "time prefix must be non-decreasing: {a} then {b}"
    );
}

// -----------------------------------------------------------------------------
// event_stream
// -----------------------------------------------------------------------------

fn make_message(model: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: model.to_string(),
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

#[tokio::test]
async fn event_stream_collect_done() {
    let msg = make_message("done-model");
    let stream = EventStream::with_default_provenance(futures::stream::iter([
        AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg.clone(),
        },
    ]));
    let result = stream.collect_to_message().await;
    let ok = result.expect("expected Done");
    assert_eq!(ok.model, "done-model");
}

#[tokio::test]
async fn event_stream_collect_error() {
    let mut err_msg = make_message("err-model");
    err_msg.stop_reason = StopReason::Error;
    err_msg.error_message = Some("boom".to_string());

    let stream = EventStream::with_default_provenance(futures::stream::iter([
        AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: err_msg,
        },
    ]));
    let result = stream.collect_to_message().await;
    let err = result.expect_err("expected Error");
    assert_eq!(err.error_message.as_deref(), Some("boom"));
}

#[tokio::test]
async fn event_stream_aborted_when_no_terminal() {
    let stream = EventStream::with_default_provenance(futures::stream::iter([
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hi".to_string(),
            partial: make_message("partial-model"),
        },
    ]));
    let err = stream
        .collect_to_message()
        .await
        .expect_err("expected aborted Err");
    assert_eq!(err.stop_reason, StopReason::Aborted);
    assert_eq!(err.model, "partial-model");
}

#[tokio::test]
async fn event_stream_aborted_uses_provenance_when_no_partial() {
    // Empty stream: no partial ever arrives, so the aborted message must
    // be filled from the provenance the caller supplied — not from any
    // hardcoded OpenAI default.
    let provenance = Provenance {
        api: Api::AnthropicMessages,
        provider: Provider::Anthropic,
        model: "claude-sonnet-4".to_string(),
    };
    let stream = EventStream::new(
        provenance,
        futures::stream::iter::<[AssistantMessageEvent; 0]>([]),
    );
    let err = stream
        .collect_to_message()
        .await
        .expect_err("expected aborted Err");
    assert_eq!(err.stop_reason, StopReason::Aborted);
    assert_eq!(err.api, Api::AnthropicMessages);
    assert_eq!(err.provider, Provider::Anthropic);
    assert_eq!(err.model, "claude-sonnet-4");
    assert_eq!(
        err.error_message.as_deref(),
        Some("stream ended without terminal event")
    );
}

#[tokio::test]
async fn event_stream_text_deltas_filters() {
    let events = vec![
        AssistantMessageEvent::Start {
            partial: make_message("m"),
        },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hello ".to_string(),
            partial: make_message("m"),
        },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "world".to_string(),
            partial: make_message("m"),
        },
        AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: make_message("m"),
        },
    ];
    let stream = EventStream::with_default_provenance(futures::stream::iter(events));
    let collected: Vec<String> = stream.text_deltas().collect().await;
    assert_eq!(collected, vec!["hello ".to_string(), "world".to_string()]);
}

#[tokio::test]
async fn event_stream_tool_calls_filters() {
    let tc = ToolCall::new("call_1", "search", serde_json::json!({"q": "rust"}));
    let events = vec![
        AssistantMessageEvent::Start {
            partial: make_message("m"),
        },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "ignored".to_string(),
            partial: make_message("m"),
        },
        AssistantMessageEvent::ToolCallEnd {
            content_index: 1,
            tool_call: tc.clone(),
            partial: make_message("m"),
        },
        AssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            message: make_message("m"),
        },
    ];
    let stream = EventStream::with_default_provenance(futures::stream::iter(events));
    let collected: Vec<ToolCall> = stream.tool_calls().collect().await;
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].id, tc.id);
    assert_eq!(collected[0].name, tc.name);
    assert_eq!(collected[0].arguments, tc.arguments);
}

// -----------------------------------------------------------------------------
// diagnostics
// -----------------------------------------------------------------------------

#[test]
fn diagnostic_serde_kebab_case_and_timestamp_rename() {
    let diag = AssistantMessageDiagnostic {
        kind: DiagnosticKind::PayloadDowngraded,
        message: "dropped unsupported field".to_string(),
        details: Some(serde_json::json!({ "field": "tools" })),
        timestamp_ms: 1_700_000_000_000,
    };
    let json = serde_json::to_value(&diag).expect("serialize");
    assert_eq!(json["kind"], serde_json::json!("payload-downgraded"));
    assert_eq!(json["timestampMs"], serde_json::json!(1_700_000_000_000u64));
    assert_eq!(
        json["message"],
        serde_json::json!("dropped unsupported field")
    );

    let round: AssistantMessageDiagnostic = serde_json::from_value(json).expect("deserialize");
    assert!(matches!(round.kind, DiagnosticKind::PayloadDowngraded));
    assert_eq!(round.timestamp_ms, 1_700_000_000_000);
}

#[test]
fn diagnostic_omits_details_when_none() {
    let diag = AssistantMessageDiagnostic::new(DiagnosticKind::Retry, "retrying");
    let json = serde_json::to_value(&diag).expect("serialize");
    assert!(json.get("details").is_none());
    assert_eq!(json["kind"], serde_json::json!("retry"));
}
