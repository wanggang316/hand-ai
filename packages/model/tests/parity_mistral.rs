//! Parity ports of the Mistral provider tests:
//!
//! - `pi-mono/.../test/mistral-tool-schema.test.ts` — verifies that tool call
//!   IDs replayed against a Mistral target get coerced to the 9-character
//!   alphanumeric form Mistral requires.
//! - `pi-mono/.../test/mistral-reasoning-mode.test.ts` — verifies that the
//!   provider sets `prompt_mode: "reasoning"` on the outbound request body
//!   when reasoning is requested, and omits the field otherwise.
//!
//! The reasoning-mode test stands up a `tiny_http` mock server, points the
//! provider at it via `MistralProvider::with_base_url(...)`, and inspects the
//! captured request body — same pattern used by the M4 OAuth tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use model::api_registry::ApiProvider;
use model::transform::transform_messages;
use model::types::{
    Api, AssistantContentBlock, AssistantMessage, Context, Cost, InputType, Message, Model,
    Provider, SimpleStreamOptions, StopReason, ThinkingLevel, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, UserMessage,
};
use model::{MistralProvider, normalize_mistral_tool_id};

// ---------------------------------------------------------------------------
// 1. Tool-id normalization parity
// ---------------------------------------------------------------------------

#[test]
fn mistral_tool_schema_normalizes_ids() {
    // Drive a sample assistant message carrying a non-Mistral-format tool
    // call ID through `transform_messages` against a Mistral target. The
    // pipeline must emit IDs that match the 9-char alphanumeric pattern
    // required by Mistral.
    let mistral_model = mistral_test_model();

    let assistant = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall::new(
                "call_abc123",
                "calculate",
                serde_json::json!({"expression": "1+1"}),
            )),
            AssistantContentBlock::ToolCall(ToolCall::new(
                "call|with|special|chars",
                "lookup",
                serde_json::json!({"key": "x"}),
            )),
        ],
        // Source is OpenAI Completions so the cross-provider branch fires.
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "gpt-4o".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    let messages = vec![
        Message::User(UserMessage::new_text("hi")),
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage::new(
            "call_abc123",
            "calculate",
            vec![ToolResultContent::Text(model::types::TextContent::new("2"))],
        )),
        Message::ToolResult(ToolResultMessage::new(
            "call|with|special|chars",
            "lookup",
            vec![ToolResultContent::Text(model::types::TextContent::new(
                "ok",
            ))],
        )),
    ];

    let normalizer: model::transform::NormalizeToolCallIdFn =
        Box::new(|id, _model, _src_msg| normalize_mistral_tool_id(id));

    let transformed = transform_messages(&messages, &mistral_model, Some(&normalizer));

    // Pull out the rewritten assistant tool call IDs.
    let assistant_msg = transformed
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("assistant message survives transform");
    let ids: Vec<&str> = assistant_msg
        .content
        .iter()
        .filter_map(|b| match b {
            AssistantContentBlock::ToolCall(tc) => Some(tc.id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(ids.len(), 2, "both tool calls must survive");
    for id in &ids {
        assert_eq!(id.len(), 9, "id must be 9 chars: {id}");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric()),
            "id must be alphanumeric: {id}"
        );
    }

    // Tool results must reference the same normalized IDs so the pairing
    // survives.
    let tool_result_ids: Vec<String> = transformed
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult(tr) => Some(tr.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_result_ids.len(), 2);
    for (idx, tr_id) in tool_result_ids.iter().enumerate() {
        assert_eq!(
            tr_id.as_str(),
            ids[idx],
            "tool result id must match the rewritten assistant tool call id",
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Reasoning mode parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mistral_reasoning_mode_in_request_body() {
    // Capture the body Mistral sees. The mock server returns a minimal SSE
    // [DONE] frame so the provider's stream completes normally.
    let server = MockServer::start_capturing(captured_body_handler);
    let captured = server.captured.clone();

    let provider = MistralProvider::new().with_base_url(server.base_url.clone());
    let model = mistral_test_model();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: model::types::StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
        reasoning: Some(ThinkingLevel::Medium),
        thinking_budgets: None,
    };

    let stream = provider.stream_simple(model.clone(), context, Some(options));
    drain_stream(stream).await;

    let body = wait_for_capture(&captured).expect("server must capture a body");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("server captured request body must parse");

    assert_eq!(
        parsed.get("prompt_mode").and_then(|v| v.as_str()),
        Some("reasoning"),
        "prompt_mode must be 'reasoning' when reasoning level is requested",
    );
    assert!(
        parsed.get("reasoning_effort").is_none(),
        "Magistral-style models must not emit reasoning_effort",
    );
    assert_eq!(parsed["model"], "magistral-medium");
    assert_eq!(parsed["stream"], true);
}

#[tokio::test]
async fn mistral_reasoning_off_no_mode_flag() {
    let server = MockServer::start_capturing(captured_body_handler);
    let captured = server.captured.clone();

    let provider = MistralProvider::new().with_base_url(server.base_url.clone());
    let model = mistral_test_model();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: model::types::StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };

    let stream = provider.stream_simple(model.clone(), context, Some(options));
    drain_stream(stream).await;

    let body = wait_for_capture(&captured).expect("server must capture a body");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("server captured request body must parse");

    assert!(
        parsed.get("prompt_mode").is_none(),
        "prompt_mode must be omitted when no reasoning level is requested",
    );
    assert!(
        parsed.get("reasoning_effort").is_none(),
        "reasoning_effort must be omitted when no reasoning level is requested",
    );
}

// ---------------------------------------------------------------------------
// 3. End-to-end cross-provider tool-id normalization through the provider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mistral_provider_normalizes_cross_provider_tool_ids_in_request_body() {
    // Drive the provider end-to-end with an OpenAI-style assistant tool call
    // (long, contains underscores) followed by its tool result. The provider
    // must invoke `transform_messages` with a Mistral tool-id normalizer so
    // every id on the wire matches the 9-char alphanumeric pattern Mistral
    // requires — and the assistant tool call's id must match the tool
    // result's `tool_call_id` so pairing survives.
    let server = MockServer::start_capturing(captured_body_handler);
    let captured = server.captured.clone();

    let provider = MistralProvider::new().with_base_url(server.base_url.clone());
    let model = mistral_test_model();

    let cross_provider_assistant = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "call_abc123XYZdef",
            "calculate",
            serde_json::json!({"expression": "2+2"}),
        ))],
        // Source is OpenAI Completions: triggers the cross-provider branch
        // in `transform_messages`.
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "gpt-4o".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    let context = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage::new_text("calc 2+2")),
            Message::Assistant(cross_provider_assistant),
            Message::ToolResult(ToolResultMessage::new(
                "call_abc123XYZdef",
                "calculate",
                vec![ToolResultContent::Text(model::types::TextContent::new("4"))],
            )),
            Message::User(UserMessage::new_text("thanks")),
        ],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: model::types::StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };

    let stream = provider.stream_simple(model.clone(), context, Some(options));
    drain_stream(stream).await;

    let body = wait_for_capture(&captured).expect("server must capture a body");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("server captured request body must parse");

    let messages = parsed["messages"]
        .as_array()
        .expect("messages must be an array");

    // Find the assistant tool call and the tool result; assert both ids are
    // 9-char alphanumeric and match each other.
    let mut assistant_tool_call_id: Option<String> = None;
    let mut tool_result_call_id: Option<String> = None;
    for msg in messages {
        match msg.get("role").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array())
                    && let Some(first) = calls.first()
                    && let Some(id) = first.get("id").and_then(|v| v.as_str())
                {
                    assistant_tool_call_id = Some(id.to_string());
                }
            }
            Some("tool") => {
                if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                    tool_result_call_id = Some(id.to_string());
                }
            }
            _ => {}
        }
    }

    let assistant_id = assistant_tool_call_id.expect("assistant tool call id present");
    let tool_id = tool_result_call_id.expect("tool result call id present");

    assert_eq!(
        assistant_id.len(),
        9,
        "assistant tool call id must be 9 chars: {assistant_id}",
    );
    assert!(
        assistant_id.chars().all(|c| c.is_ascii_alphanumeric()),
        "assistant tool call id must be alphanumeric: {assistant_id}",
    );
    assert_eq!(
        assistant_id, tool_id,
        "tool_call_id on the tool message must match the rewritten assistant id",
    );
    assert_ne!(
        assistant_id, "call_abc123XYZdef",
        "raw OpenAI-style id must NOT appear on the wire",
    );
}

// ---------------------------------------------------------------------------
// 4. Start emitted before Error on early network failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mistral_emits_start_before_error_on_network_failure() {
    use futures::StreamExt;
    use model::types::AssistantMessageEvent;

    // Point the provider at a port no one is listening on so the request
    // fails before the SSE stream opens.
    let provider = MistralProvider::new().with_base_url("http://127.0.0.1:1".to_string());
    let model = mistral_test_model();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: model::types::StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };

    let mut stream = provider.stream_simple(model.clone(), context, Some(options));
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(!events.is_empty(), "stream must produce at least one event");
    assert!(
        matches!(events.first(), Some(AssistantMessageEvent::Start { .. })),
        "first event must be Start, got: {:?}",
        events.first(),
    );
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Error { .. })),
        "last event must be Error, got: {:?}",
        events.last(),
    );

    // No second `Start` should be emitted (that would mean both the outer
    // wrapper and `parse_sse_stream` produced one).
    let start_count = events
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::Start { .. }))
        .count();
    assert_eq!(start_count, 1, "exactly one Start must be emitted");
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn mistral_test_model() -> Model {
    Model {
        id: "magistral-medium".to_string(),
        name: "Magistral Medium".to_string(),
        api: Api::MistralConversations,
        provider: Provider::Mistral,
        base_url: "https://api.mistral.ai".to_string(),
        reasoning: true,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 32_000,
        max_tokens: 8_192,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

/// Drain an `AssistantMessageEventStream` to completion.
async fn drain_stream(mut stream: model::api_registry::AssistantMessageEventStream<'static>) {
    use futures::StreamExt;
    while let Some(_event) = stream.next().await {}
}

/// Mock HTTP server that records each incoming request body.
struct MockServer {
    base_url: String,
    captured: Arc<CapturedBodies>,
    server: Arc<tiny_http::Server>,
    _join: thread::JoinHandle<()>,
}

#[derive(Default)]
struct CapturedBodies {
    bodies: std::sync::Mutex<Vec<String>>,
    notify: AtomicUsize,
}

impl CapturedBodies {
    fn push(&self, body: String) {
        self.bodies.lock().unwrap().push(body);
        self.notify.fetch_add(1, Ordering::SeqCst);
    }

    fn pop(&self) -> Option<String> {
        let mut guard = self.bodies.lock().unwrap();
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0))
        }
    }
}

type CapturedHandler = fn(tiny_http::Request, Sender<()>, Arc<CapturedBodies>);

impl MockServer {
    fn start_capturing(handler: CapturedHandler) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind mock server"));
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let base_url = format!("http://127.0.0.1:{port}");
        let captured = Arc::new(CapturedBodies::default());

        let captured_clone = captured.clone();
        let server_clone = Arc::clone(&server);
        let (tx, _rx) = channel::<()>();
        let join = thread::spawn(move || {
            for req in server_clone.incoming_requests() {
                handler(req, tx.clone(), captured_clone.clone());
            }
        });

        MockServer {
            base_url,
            captured,
            server,
            _join: join,
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn captured_body_handler(
    mut req: tiny_http::Request,
    _tx: Sender<()>,
    captured: Arc<CapturedBodies>,
) {
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
    captured.push(body);

    // Minimal SSE response: a [DONE] sentinel so the provider exits cleanly.
    let payload = "data: [DONE]\n\n".as_bytes().to_vec();
    let len = payload.len();
    let response = tiny_http::Response::new(
        tiny_http::StatusCode(200),
        vec![
            tiny_http::Header::from_bytes(b"Content-Type".as_ref(), b"text/event-stream".as_ref())
                .unwrap(),
        ],
        std::io::Cursor::new(payload),
        Some(len),
        None,
    );
    let _ = req.respond(response);
}

/// Wait up to ~2s for the server to record at least one body, then return it.
fn wait_for_capture(captured: &Arc<CapturedBodies>) -> Option<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Some(body) = captured.pop() {
            return Some(body);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}
