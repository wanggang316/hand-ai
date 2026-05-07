//! Smoke tests for the coding-agent integration test harness.
//!
//! These tests do not perform any real HTTP calls and run without API keys.
//! They exercise the harness building blocks plus a small end-to-end round
//! trip through `SessionManager::in_memory`.
//!
//! Note: a smoke test through `AgentSession::in_memory` would require a way
//! to inject a custom `model::Client` (or its registry) so that the mock
//! provider can be wired up. The current `AgentSession::in_memory(model,
//! tools)` constructs its own `Client` internally and exposes no accessor,
//! so we test at a smaller surface here. See the implementer report for the
//! suggested follow-up.

use futures::StreamExt;
use hand_coding_agent::SessionManager;
use model::{
    ApiProvider, AssistantContentBlock, AssistantMessageEvent, Context, Message, StopReason,
    UserContent, UserMessage,
};

mod common;

use common::{mock_text_provider, mock_tool_provider, temp_session_dir, test_model};

#[tokio::test]
async fn mock_text_provider_emits_full_text_sequence() {
    let provider: Box<dyn ApiProvider> = mock_text_provider("hello world");

    let mut stream = provider.stream(test_model(), Context::default(), None);

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(
        matches!(events.first(), Some(AssistantMessageEvent::Start { .. })),
        "first event must be Start, got: {:?}",
        events.first()
    );

    let has_text_start = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextStart { .. }));
    let has_text_delta = events.iter().any(|e| {
        matches!(
            e,
            AssistantMessageEvent::TextDelta { delta, .. } if delta == "hello world"
        )
    });
    let has_text_end = events.iter().any(|e| {
        matches!(
            e,
            AssistantMessageEvent::TextEnd { content, .. } if content == "hello world"
        )
    });
    assert!(has_text_start, "expected TextStart event");
    assert!(has_text_delta, "expected TextDelta with full text");
    assert!(has_text_end, "expected TextEnd with full text");

    match events.last() {
        Some(AssistantMessageEvent::Done { reason, message }) => {
            assert_eq!(*reason, StopReason::Stop);
            assert_eq!(message.content.len(), 1);
            match &message.content[0] {
                AssistantContentBlock::Text(t) => assert_eq!(t.text, "hello world"),
                other => panic!("expected text block, got {other:?}"),
            }
        }
        other => panic!("expected Done event last, got {other:?}"),
    }
}

#[tokio::test]
async fn mock_tool_provider_emits_single_tool_call_and_stops() {
    let args = serde_json::json!({"path": "README.md"});
    let provider: Box<dyn ApiProvider> = mock_tool_provider("read_file", args.clone());

    let mut stream = provider.stream(test_model(), Context::default(), None);

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let tool_call_end = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::ToolCallEnd { tool_call, .. } => Some(tool_call.clone()),
            _ => None,
        })
        .expect("expected ToolCallEnd event");
    assert_eq!(tool_call_end.name, "read_file");
    assert_eq!(tool_call_end.arguments, args);

    match events.last() {
        Some(AssistantMessageEvent::Done { reason, message }) => {
            assert_eq!(*reason, StopReason::ToolUse);
            assert_eq!(message.stop_reason, StopReason::ToolUse);
            assert!(matches!(
                message.content[0],
                AssistantContentBlock::ToolCall(_)
            ));
        }
        other => panic!("expected Done event last, got {other:?}"),
    }
}

#[test]
fn temp_session_dir_creates_writable_directory() {
    let dir = temp_session_dir();
    let path = dir.path().to_path_buf();
    assert!(path.is_dir(), "temp dir should exist");

    let f = path.join("scratch.txt");
    std::fs::write(&f, b"ok").expect("temp dir should be writable");
    assert_eq!(std::fs::read(&f).unwrap(), b"ok");

    drop(dir);
    assert!(!path.exists(), "TempDir should clean up on drop");
}

#[test]
fn session_manager_round_trip_appends_messages() {
    // Smoke: a message appended to an in-memory session is visible via
    // `build_context` and reflected in the message count.
    let mut sm = SessionManager::in_memory();
    assert_eq!(sm.message_count(), 0);

    let user = Message::User(UserMessage::new_text("hi"));
    sm.append_message(user)
        .expect("append user message should succeed");

    assert_eq!(sm.message_count(), 1);

    let context = sm.build_context();
    assert_eq!(context.len(), 1);
    match &context[0] {
        Message::User(m) => match &m.content {
            UserContent::Text(s) => assert_eq!(s, "hi"),
            UserContent::Blocks(blocks) => {
                assert!(
                    !blocks.is_empty(),
                    "user message blocks should be non-empty"
                )
            }
        },
        other => panic!("expected user message, got {other:?}"),
    }
}
