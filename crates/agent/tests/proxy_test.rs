//! Integration tests for `hand_agent::proxy::stream_proxy` against a
//! mocked proxy server. Uses `wiremock` to stand up an HTTP endpoint
//! at runtime and drives the full SSE -> AssistantMessageEvent pipeline.

mod common;

use common::test_model;
use futures::StreamExt;
use hand_agent::proxy::{ProxyStreamOptions, stream_proxy};
use model::{AssistantContentBlock, AssistantMessageEvent, Context, StopReason};
use std::time::Duration;

/// Assert that `partial.content[0]` is a `Text` block with the expected text.
fn assert_text_at_index_0(partial: &model::AssistantMessage, expected: &str) {
    match partial.content.first() {
        Some(AssistantContentBlock::Text(t)) => {
            assert_eq!(t.text, expected, "unexpected text at content[0]");
        }
        other => panic!("expected Text at content[0], got {other:?}"),
    }
}

#[tokio::test]
async fn proxy_emits_full_event_arc() {
    let server = wiremock::MockServer::start().await;
    let url = server.uri();

    // Each line is one SSE record terminated by `\n`. The driver only requires
    // `\n` per-line; trailing `\n` ensures the last line is fully drained.
    let sse_body = [
        r#"data: {"type":"start"}"#,
        r#"data: {"type":"text_start","contentIndex":0}"#,
        r#"data: {"type":"text_delta","contentIndex":0,"delta":"Hello"}"#,
        r#"data: {"type":"text_delta","contentIndex":0,"delta":" world"}"#,
        r#"data: {"type":"text_end","contentIndex":0}"#,
        r#"data: {"type":"done","reason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
        "",
    ]
    .join("\n");

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/stream"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let model = test_model();
    let opts = ProxyStreamOptions {
        auth_token: "test-token".into(),
        proxy_url: url,
        ..Default::default()
    };

    let mut stream = stream_proxy(&model, Context::default(), opts);
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert_eq!(events.len(), 6, "expected 6 events, got {}", events.len());

    match &events[0] {
        AssistantMessageEvent::Start { partial } => {
            assert!(
                partial.content.is_empty(),
                "Start partial should have no content"
            );
        }
        other => panic!("events[0] expected Start, got {other:?}"),
    }

    match &events[1] {
        AssistantMessageEvent::TextStart {
            content_index,
            partial,
        } => {
            assert_eq!(*content_index, 0);
            assert_text_at_index_0(partial, "");
        }
        other => panic!("events[1] expected TextStart, got {other:?}"),
    }

    match &events[2] {
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
            partial,
        } => {
            assert_eq!(*content_index, 0);
            assert_eq!(delta, "Hello");
            assert_text_at_index_0(partial, "Hello");
        }
        other => panic!("events[2] expected TextDelta, got {other:?}"),
    }

    match &events[3] {
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
            partial,
        } => {
            assert_eq!(*content_index, 0);
            assert_eq!(delta, " world");
            assert_text_at_index_0(partial, "Hello world");
        }
        other => panic!("events[3] expected TextDelta, got {other:?}"),
    }

    match &events[4] {
        AssistantMessageEvent::TextEnd {
            content_index,
            content,
            partial,
        } => {
            assert_eq!(*content_index, 0);
            assert_eq!(content, "Hello world");
            assert_text_at_index_0(partial, "Hello world");
        }
        other => panic!("events[4] expected TextEnd, got {other:?}"),
    }

    match &events[5] {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(*reason, StopReason::Stop);
            assert_text_at_index_0(message, "Hello world");
        }
        other => panic!("events[5] expected Done, got {other:?}"),
    }
}

#[tokio::test]
async fn proxy_surfaces_http_error() {
    let server = wiremock::MockServer::start().await;
    let url = server.uri();

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/stream"))
        .respond_with(
            wiremock::ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"error":"bad token"}"#),
        )
        .mount(&server)
        .await;

    let model = test_model();
    let opts = ProxyStreamOptions {
        auth_token: "test-token".into(),
        proxy_url: url,
        ..Default::default()
    };

    let mut stream = stream_proxy(&model, Context::default(), opts);
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert_eq!(
        events.len(),
        1,
        "expected exactly 1 event, got {}",
        events.len()
    );

    match &events[0] {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Error);
            assert_eq!(
                error.error_message.as_deref(),
                Some("Proxy error: bad token"),
                "error_message should mirror TS proxy.ts:166-177 substitution",
            );
        }
        other => panic!("expected Error event, got {other:?}"),
    }
}

#[tokio::test]
async fn proxy_aborts_when_token_cancelled() {
    let server = wiremock::MockServer::start().await;
    let url = server.uri();

    // Server holds the response open for 5s; cancellation must short-circuit.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/stream"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"type\":\"start\"}\n")
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let opts = ProxyStreamOptions {
        auth_token: "test-token".into(),
        proxy_url: url,
        cancel: Some(cancel),
        ..Default::default()
    };

    let collect = async {
        let mut events = Vec::new();
        let mut stream = stream_proxy(&test_model(), Context::default(), opts);
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
        events
    };

    // Budget covers debug-build TCP/HTTP overhead on heavily loaded
    // machines (CI under contention has been observed sitting at 8-9s for
    // the cold-start request, even though cancel itself fires at 50 ms).
    // Any failure to honour the cancel would still hit the upstream
    // wiremock 5-s delay and trip a timeout, so the budget can be loose.
    let events = tokio::time::timeout(Duration::from_secs(30), collect)
        .await
        .expect("stream_proxy did not respect cancel within 30s");

    assert!(!events.is_empty(), "expected at least one event");
    match events.last().expect("non-empty") {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.error_message.as_deref(), Some("Aborted"));
        }
        other => panic!("expected last event to be Error(Aborted), got {other:?}"),
    }
}

/// End-to-end integration: `Agent` configured with `stream_fn_proxy` drives a
/// real HTTP exchange against a wiremock proxy and reconstructs the assistant
/// message. Covers the seam between [`Agent::with_options`] and
/// [`stream_fn_proxy`] that the unit tests don't exercise.
#[tokio::test]
async fn agent_with_stream_fn_proxy_runs_end_to_end() {
    use hand_agent::{Agent, AgentOptions, ProxyStreamOptions, stream_fn_proxy};
    use model::Client;

    let server = wiremock::MockServer::start().await;
    let url = server.uri();

    let sse_body = [
        r#"data: {"type":"start"}"#,
        r#"data: {"type":"text_start","contentIndex":0}"#,
        r#"data: {"type":"text_delta","contentIndex":0,"delta":"Hello"}"#,
        r#"data: {"type":"text_delta","contentIndex":0,"delta":" from agent"}"#,
        r#"data: {"type":"text_end","contentIndex":0}"#,
        r#"data: {"type":"done","reason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
    ]
    .join("\n")
        + "\n";

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/stream"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    // No provider registered on the client. If the loop ever falls back to
    // `client.stream_simple`, it would error out with `ProviderNotFound`,
    // which would surface as an Error assistant message rather than the
    // reconstructed text we assert on below.
    let client = Client::new();

    let stream_fn = stream_fn_proxy(ProxyStreamOptions {
        auth_token: "test-token".into(),
        proxy_url: url,
        ..Default::default()
    });

    let mut agent = Agent::with_options(
        client,
        test_model(),
        AgentOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        },
    );

    let result = agent
        .prompt("hi")
        .await
        .expect("agent run should complete via proxy");

    let last = result
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            model::Message::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("at least one assistant message");

    assert_eq!(last.stop_reason, StopReason::Stop);
    let text = match &last.content[0] {
        AssistantContentBlock::Text(t) => &t.text,
        other => panic!("expected text content, got {other:?}"),
    };
    assert_eq!(text, "Hello from agent");
}
