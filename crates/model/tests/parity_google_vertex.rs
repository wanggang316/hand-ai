//! Coverage tests for the Google Vertex provider.
//!
//! - `vertex_api_key_resolution_uses_explicit_key_first` — verifies that
//!   an explicit, non-placeholder `api_key` is forwarded as
//!   `?key=<value>` and that the request does NOT carry an
//!   `Authorization: Bearer …` header (i.e. ADC is not consulted when an
//!   explicit key is provided).
//! - `vertex_emits_start_before_error_on_network_failure` — start-event
//!   uniformity check. A stream that fails before any candidate parsing
//!   must still emit exactly one `Start` event followed by `Error`.
//! - `vertex_url_includes_project_and_location` — the request URL embeds
//!   project, location, and model id in the canonical Vertex layout.
//! - `vertex_adc_token_is_used_when_no_api_key` — when no explicit key is
//!   set, the provider invokes the configured token provider and forwards
//!   the resulting token as `Authorization: Bearer …`.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use futures::future::FutureExt;
use model::api_registry::ApiProvider;
use model::providers::google_vertex::GoogleVertexProvider;
use model::types::{
    Api, Context, Cost, InputType, Message, Model, Provider, StreamOptions, UserMessage,
};

// ---------------------------------------------------------------------------
// 1. Explicit api_key wins over ADC fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vertex_api_key_resolution_uses_explicit_key_first() {
    let _env = TestEnv::scoped(Some("test-project"), Some("us-central1"), None);

    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    // Token provider that would panic if invoked. Proves the explicit api
    // key path bypasses ADC entirely.
    let token_provider: model::providers::VertexTokenProvider = Arc::new(|| {
        async {
            panic!("vertex_access_token must not be called when an explicit api_key is supplied")
        }
        .boxed()
    });

    let provider = GoogleVertexProvider::new()
        .with_base_url(server.base_url.clone())
        .with_token_provider(token_provider);

    let model = vertex_test_model();
    let context = simple_context();

    let stream = provider.stream(
        model,
        context,
        Some({
            let mut o = StreamOptions::default();
            o.api_key = Some("AIzaSyExampleRealisticLookingApiKey123456".to_string());
            o
        }),
    );
    drain_stream(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    assert!(
        req.url
            .contains("key=AIzaSyExampleRealisticLookingApiKey123456"),
        "url must include explicit ?key=…, got: {}",
        req.url,
    );

    let auth = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"));
    assert!(
        auth.is_none(),
        "explicit api_key must not produce an Authorization header, got: {:?}",
        auth,
    );
}

// ---------------------------------------------------------------------------
// 2. Start-event uniformity on early network failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vertex_emits_start_before_error_on_network_failure() {
    use futures::StreamExt;
    use model::types::AssistantMessageEvent;

    let _env = TestEnv::scoped(Some("test-project"), Some("us-central1"), None);

    // Point the provider at a port no one is listening on so the request
    // fails before the SSE stream opens.
    let provider = GoogleVertexProvider::new().with_base_url("http://127.0.0.1:1".to_string());
    let model = vertex_test_model();
    let context = simple_context();

    let mut stream = provider.stream(
        model,
        context,
        Some({
            let mut o = StreamOptions::default();
            o.api_key = Some("AIzaSyExample".to_string());
            o
        }),
    );

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
// 3. URL composition
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vertex_url_includes_project_and_location() {
    let _env = TestEnv::scoped(Some("alpha-project"), Some("europe-west4"), None);

    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    let provider = GoogleVertexProvider::new().with_base_url(server.base_url.clone());

    let model = vertex_test_model();
    let context = simple_context();

    let stream = provider.stream(
        model,
        context,
        Some({
            let mut o = StreamOptions::default();
            o.api_key = Some("AIzaSyExample".to_string());
            o
        }),
    );
    drain_stream(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    let expected_path = "/v1/projects/alpha-project/locations/europe-west4/publishers/google/models/gemini-2.5-flash:streamGenerateContent";
    assert!(
        req.url.contains(expected_path),
        "url must contain canonical Vertex path, got: {}",
        req.url,
    );
    assert!(
        req.url.contains("alt=sse"),
        "url must request SSE streaming, got: {}",
        req.url,
    );
}

// ---------------------------------------------------------------------------
// 4. ADC token via the test seam when no explicit api_key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vertex_adc_token_is_used_when_no_api_key() {
    let _env = TestEnv::scoped(Some("test-project"), Some("us-central1"), None);

    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    let token_provider: model::providers::VertexTokenProvider =
        Arc::new(|| async { Ok("ya29.fake-access-token".to_string()) }.boxed());

    let provider = GoogleVertexProvider::new()
        .with_base_url(server.base_url.clone())
        .with_token_provider(token_provider);

    let model = vertex_test_model();
    let context = simple_context();

    let stream = provider.stream(model, context, Some(StreamOptions::default()));
    drain_stream(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    assert!(
        !req.url.contains("key="),
        "url must not include api key when using ADC, got: {}",
        req.url,
    );

    let auth = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str().to_string());
    assert_eq!(
        auth.as_deref(),
        Some("Bearer ya29.fake-access-token"),
        "ADC token must be forwarded as Bearer auth, got: {:?}",
        auth,
    );
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn vertex_test_model() -> Model {
    Model {
        id: "gemini-2.5-flash".to_string(),
        name: "Gemini 2.5 Flash".to_string(),
        api: Api::GoogleVertex,
        provider: Provider::GoogleVertex,
        base_url: String::new(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 1_000_000,
        max_tokens: 8_192,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

fn simple_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    }
}

/// Drain an `AssistantMessageEventStream` to completion.
async fn drain_stream(mut stream: model::api_registry::AssistantMessageEventStream<'static>) {
    use futures::StreamExt;
    while let Some(_event) = stream.next().await {}
}

/// RAII guard that scopes process-env mutations to a single test and
/// serializes the mutation across tests in this binary so they do not race.
struct TestEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    saved_project: Option<String>,
    saved_location: Option<String>,
    saved_api_key: Option<String>,
}

impl TestEnv {
    fn scoped(project: Option<&str>, location: Option<&str>, api_key: Option<&str>) -> Self {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let saved_project = std::env::var("GOOGLE_CLOUD_PROJECT").ok();
        let saved_location = std::env::var("GOOGLE_CLOUD_LOCATION").ok();
        let saved_api_key = std::env::var("GOOGLE_CLOUD_API_KEY").ok();

        // SAFETY: the lock above ensures no other test in this binary is
        // mutating these env vars concurrently. Other tests in different
        // binaries that touch the same vars are out of scope.
        unsafe {
            match project {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_PROJECT", v),
                None => std::env::remove_var("GOOGLE_CLOUD_PROJECT"),
            }
            match location {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_LOCATION", v),
                None => std::env::remove_var("GOOGLE_CLOUD_LOCATION"),
            }
            match api_key {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_API_KEY", v),
                None => std::env::remove_var("GOOGLE_CLOUD_API_KEY"),
            }
        }

        Self {
            _guard: guard,
            saved_project,
            saved_location,
            saved_api_key,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // SAFETY: the lock held in `_guard` is released after this block.
        unsafe {
            match &self.saved_project {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_PROJECT", v),
                None => std::env::remove_var("GOOGLE_CLOUD_PROJECT"),
            }
            match &self.saved_location {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_LOCATION", v),
                None => std::env::remove_var("GOOGLE_CLOUD_LOCATION"),
            }
            match &self.saved_api_key {
                Some(v) => std::env::set_var("GOOGLE_CLOUD_API_KEY", v),
                None => std::env::remove_var("GOOGLE_CLOUD_API_KEY"),
            }
        }
    }
}

/// Captured request from the mock server.
#[derive(Debug, Clone)]
struct CapturedRequest {
    url: String,
    headers: Vec<(String, String)>,
    #[allow(dead_code)]
    body: String,
}

/// Mock HTTP server that records each incoming request URL + headers + body.
struct MockServer {
    base_url: String,
    captured: Arc<CapturedRequests>,
    server: Arc<tiny_http::Server>,
    _join: thread::JoinHandle<()>,
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
    let headers = req
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect::<Vec<_>>();

    let mut body = String::new();
    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);

    captured.push(CapturedRequest { url, headers, body });

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

/// Wait up to ~2s for the server to record at least one request.
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
