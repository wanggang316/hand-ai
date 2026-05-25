//! Coverage tests for the OpenAI Codex Responses provider.
//!
//! 1. OAuth credentials are loaded from the `OAuthRegistry` when no
//!    explicit `api_key` is supplied; the resulting bearer token shows
//!    up on the wire as `Authorization: Bearer <token>`.
//! 2. The default transport (SSE) routes to the configured Codex URL.
//! 3. The WebSocket transport at minimum surfaces a clear error rather
//!    than silently falling back. (Actual frame handling is a follow-up.)
//! 4. With no OAuth credentials and no `api_key`, the stream emits
//!    Start before Error and the error message is actionable.
//! 5. The `session_id` option flows through to the SSE request as a
//!    header (`session_id` + `x-client-request-id`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use futures::StreamExt;
use model::api_registry::{ApiProvider, AssistantMessageEventStream};
use model::oauth::{OAuthAuthInfo, OAuthCredentials, OAuthProviderId, OAuthRegistry};
use model::providers::openai_codex_responses::OpenAICodexResponsesProvider;
use model::types::{
    Api, AssistantMessageEvent, Context, Cost, InputType, Message, Model, Provider, StreamOptions,
    Transport, UserMessage,
};

// ---------------------------------------------------------------------------
// Test 1 — OAuth credentials feed the Authorization header
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_oauth_credentials_loaded_for_request() {
    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    // Stand up an OAuth registry on a tempdir, save credentials with a
    // fixed access token, and hand the registry to the provider.
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = Arc::new(OAuthRegistry::with_storage_path(
        tmp.path().join("oauth.json"),
    ));
    registry
        .save(&OAuthAuthInfo {
            provider_id: OAuthProviderId::OpenAICodex,
            credentials: OAuthCredentials {
                access_token: "test-oauth-token".to_string(),
                refresh_token: Some("rt".to_string()),
                // Far in the future so `is_expired` returns false and we
                // exercise the no-refresh path.
                expires_at: Some(u64::MAX / 2),
                scope: None,
                extra: None,
            },
            created_at_ms: 0,
        })
        .await
        .expect("save credentials");

    let provider = OpenAICodexResponsesProvider::new()
        .with_base_url(server.base_url.clone())
        .with_oauth_registry(registry);

    let stream = provider.stream(test_model(), test_context(), None);
    drain(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    let auth = header_value(&req.headers, "authorization").expect("Authorization header");
    assert_eq!(
        auth, "Bearer test-oauth-token",
        "Authorization header must carry the OAuth bearer token"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — Default SSE transport routes to /codex/responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_sse_transport_routes_to_correct_url() {
    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    let provider = OpenAICodexResponsesProvider::new().with_base_url(server.base_url.clone());

    let mut options = StreamOptions::default();
    options.api_key = Some("test-key".to_string());
    options.transport = Some(Transport::Sse);

    let stream = provider.stream(test_model(), test_context(), Some(options));
    drain(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");
    assert!(
        req.url.ends_with("/codex/responses"),
        "Codex SSE URL must end with /codex/responses — got {}",
        req.url,
    );

    // OpenAI-Beta header for the responses experimental flag is required
    // by the upstream API.
    let beta = header_value(&req.headers, "openai-beta");
    assert_eq!(beta.as_deref(), Some("responses=experimental"));

    let originator = header_value(&req.headers, "originator");
    assert_eq!(originator.as_deref(), Some("pi"));
}

// ---------------------------------------------------------------------------
// Test 3 — WebSocket transport (currently surfaces a clear error)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_websocket_transport_uses_ws_endpoint() {
    // The Rust port stubs the WebSocket transport. The minimum we can
    // assert today is that selecting `Transport::Websocket` does not
    // silently fall back to SSE — the stream surfaces an error whose
    // message names the would-be `wss://` URL. The full frame protocol
    // lives in an M9 follow-up.

    let provider = OpenAICodexResponsesProvider::new()
        .with_base_url("https://codex.example.test/backend-api".to_string());

    let mut options = StreamOptions::default();
    options.api_key = Some("test-key".to_string());
    options.transport = Some(Transport::Websocket);

    let mut stream = provider.stream(test_model(), test_context(), Some(options));
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(
        matches!(events.first(), Some(AssistantMessageEvent::Start { .. })),
        "first event must be Start, got: {:?}",
        events.first(),
    );
    let last = events.last().expect("at least one event");
    match last {
        AssistantMessageEvent::Error { error, .. } => {
            let msg = error.error_message.clone().unwrap_or_default();
            assert!(
                msg.contains("wss://codex.example.test/backend-api/codex/responses"),
                "error message should reference the WebSocket URL — got: {msg}",
            );
        }
        other => panic!("last event must be Error, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 4 — Start before Error when OAuth credentials are missing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_emits_start_before_error_on_oauth_missing() {
    // Empty registry, no api_key — the provider must fail cleanly with
    // an actionable error after emitting Start.
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = Arc::new(OAuthRegistry::with_storage_path(
        tmp.path().join("oauth.json"),
    ));

    let provider = OpenAICodexResponsesProvider::new().with_oauth_registry(registry);

    let mut stream = provider.stream(test_model(), test_context(), None);
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    assert!(
        matches!(events.first(), Some(AssistantMessageEvent::Start { .. })),
        "first event must be Start, got: {:?}",
        events.first(),
    );
    match events.last().expect("at least one event") {
        AssistantMessageEvent::Error { error, .. } => {
            let msg = error.error_message.clone().unwrap_or_default();
            assert!(
                msg.contains("OAuth"),
                "error message should mention OAuth — got: {msg}",
            );
        }
        other => panic!("last event must be Error, got: {other:?}"),
    }

    let start_count = events
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::Start { .. }))
        .count();
    assert_eq!(start_count, 1, "exactly one Start must be emitted");
}

// ---------------------------------------------------------------------------
// Test 5 — `session_id` flows through as request headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_cache_affinity_session_header() {
    let server = MockServer::start_capturing(captured_request_handler);
    let captured = server.captured.clone();

    let provider = OpenAICodexResponsesProvider::new().with_base_url(server.base_url.clone());

    let mut options = StreamOptions::default();
    options.api_key = Some("test-key".to_string());
    options.session_id = Some("sess_abc".to_string());

    let stream = provider.stream(test_model(), test_context(), Some(options));
    drain(stream).await;

    let req = wait_for_capture(&captured).expect("server must capture a request");

    let session_header = header_value(&req.headers, "session_id");
    assert_eq!(
        session_header.as_deref(),
        Some("sess_abc"),
        "session_id header must carry the configured session id",
    );

    let request_id = header_value(&req.headers, "x-client-request-id");
    assert_eq!(
        request_id.as_deref(),
        Some("sess_abc"),
        "x-client-request-id mirrors session_id",
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_model() -> Model {
    Model {
        id: "gpt-5-codex".to_string(),
        name: "GPT-5 Codex".to_string(),
        api: Api::OpenAICodexResponses,
        provider: Provider::OpenAICodex,
        base_url: String::new(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 200_000,
        max_tokens: 32_000,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

fn test_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("hello"))],
        tools: None,
    }
}

async fn drain(mut stream: AssistantMessageEventStream<'static>) {
    while let Some(_event) = stream.next().await {}
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

#[derive(Clone)]
struct CapturedRequest {
    url: String,
    headers: Vec<(String, String)>,
    #[allow(dead_code)]
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

    let payload = b"data: [DONE]\n\n".to_vec();
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
