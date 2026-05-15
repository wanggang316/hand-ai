//! Azure OpenAI Responses provider — base-URL resolution coverage.
//!
//! Stands up a `tiny_http` mock server and inspects the actual request
//! line + headers + body that the provider sends — same pattern used by
//! `parity_mistral.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use model::AzureOpenAIResponsesProvider;
use model::api_registry::ApiProvider;
use model::types::{
    Api, Context, Cost, InputType, Message, Model, Provider, SimpleStreamOptions, StreamOptions,
    UserMessage,
};

// ---------------------------------------------------------------------------
// 1. Azure URL shape + api-key header parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn azure_base_url_uses_api_key_header() {
    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    // Point the provider at the mock so the captured URL/headers/body all
    // reflect what the provider would send to a real Azure endpoint.
    let provider = AzureOpenAIResponsesProvider::new().with_base_url(server.base_url.clone());
    let model = azure_test_model();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("hello"))],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test-api-key".to_string()),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };

    let stream = provider.stream_simple(model, context, Some(options));
    drain_stream(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    // ---- URL shape ----
    // The base URL is the test mock's `http://127.0.0.1:<port>` and the
    // provider must append `/responses?api-version=v1`.
    assert!(
        req.url.ends_with("/responses?api-version=v1"),
        "Azure URL must end with /responses?api-version=v1 — got {}",
        req.url,
    );

    // ---- Auth headers ----
    let api_key_header = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("api-key"))
        .map(|(_, v)| v.clone());
    assert_eq!(
        api_key_header.as_deref(),
        Some("test-api-key"),
        "api-key header must carry the API key",
    );

    let has_authorization = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
    assert!(
        !has_authorization,
        "Authorization header must NOT be sent for Azure (found: {:?})",
        req.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization")),
    );

    // ---- Body shape ----
    let body: serde_json::Value =
        serde_json::from_str(&req.body).expect("captured body must be valid JSON");
    assert_eq!(body["model"], "gpt-4o-mini");
    assert_eq!(body["stream"], true);
    let input = body["input"].as_array().expect("input must be an array");
    assert_eq!(input.len(), 1, "single user message in input");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"], "hello");
}

// ---------------------------------------------------------------------------
// 2. Start emitted before Error on early network failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn azure_emits_start_before_error_on_network_failure() {
    use futures::StreamExt;
    use model::types::AssistantMessageEvent;

    // Point at a port no one is listening on so the request fails before
    // the SSE stream opens.
    let provider =
        AzureOpenAIResponsesProvider::new().with_base_url("http://127.0.0.1:1".to_string());
    let model = azure_test_model();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
        reasoning: None,
        thinking_budgets: None,
    };

    let mut stream = provider.stream_simple(model, context, Some(options));
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

    let start_count = events
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::Start { .. }))
        .count();
    assert_eq!(start_count, 1, "exactly one Start must be emitted");
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn azure_test_model() -> Model {
    Model {
        id: "gpt-4o-mini".to_string(),
        name: "GPT-4o mini".to_string(),
        api: Api::AzureOpenAiResponses,
        provider: Provider::AzureOpenAiResponses,
        // The provider should prefer `with_base_url` over this; left non-empty
        // so the empty-base-url error path doesn't fire if a future refactor
        // ever bypasses the override.
        base_url: "https://placeholder.openai.azure.com/openai/v1".to_string(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128_000,
        max_tokens: 16_384,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

async fn drain_stream(mut stream: model::api_registry::AssistantMessageEventStream<'static>) {
    use futures::StreamExt;
    while let Some(_event) = stream.next().await {}
}

#[derive(Clone)]
struct CapturedRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Default)]
struct CapturedRequests {
    requests: std::sync::Mutex<Vec<CapturedRequest>>,
    notify: AtomicUsize,
}

impl CapturedRequests {
    fn push(&self, req: CapturedRequest) {
        self.requests.lock().unwrap().push(req);
        self.notify.fetch_add(1, Ordering::SeqCst);
    }

    fn pop(&self) -> Option<CapturedRequest> {
        let mut guard = self.requests.lock().unwrap();
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0))
        }
    }
}

struct MockServer {
    base_url: String,
    captured: Arc<CapturedRequests>,
    server: Arc<tiny_http::Server>,
    _join: thread::JoinHandle<()>,
}

type CapturedHandler = fn(tiny_http::Request, Sender<()>, Arc<CapturedRequests>);

impl MockServer {
    fn start_capturing(handler: CapturedHandler) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind mock server"));
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let base_url = format!("http://127.0.0.1:{port}");
        let captured = Arc::new(CapturedRequests::default());

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

fn captured_request_handler(
    mut req: tiny_http::Request,
    _tx: Sender<()>,
    captured: Arc<CapturedRequests>,
) {
    let url = req.url().to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
    captured.push(CapturedRequest { url, headers, body });

    // Minimal SSE response so the provider's stream completes cleanly.
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

fn wait_for_capture(captured: &Arc<CapturedRequests>) -> Option<CapturedRequest> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Some(req) = captured.pop() {
            return Some(req);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}
