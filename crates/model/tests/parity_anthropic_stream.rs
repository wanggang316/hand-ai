//! Wire-level streaming coverage for the Anthropic Messages provider.
//!
//! The provider must consume the SSE body incrementally: every event has
//! to reach the caller as its bytes land, not in one burst once the
//! upstream closes the connection. A provider that buffers the whole body
//! first still produces the right events in the right order, so only a
//! timing-aware test can tell the two apart.
//!
//! Both tests script a raw HTTP/1.1 server (chunked transfer encoding, the
//! framing every real SSE endpoint uses) so the test controls exactly when
//! each byte hits the socket.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::StreamExt;
use model::AnthropicMessagesProvider;
use model::api_registry::ApiProvider;
use model::types::{
    Api, AssistantMessageEvent, Context, Cost, InputType, Message, Model, Provider, StreamOptions,
    UserMessage,
};

/// `message_start` through the first `text_delta` — everything the caller
/// should see before the turn finishes.
const HEAD: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"usage\":{\"input_tokens\":10},\"model\":\"claude-test\"}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
);

/// The terminal half of the same stream.
const TAIL: &str = concat!(
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// The regression the buffering implementation cannot pass: the server
/// holds the tail back until the client reports the first `TextDelta`, so
/// the delta provably left the provider before the final bytes were
/// written. A provider that awaits the whole body deadlocks against that
/// gate until the server gives up, and then fails the ordering assert.
#[tokio::test]
async fn text_delta_arrives_before_the_tail_is_written() {
    let delta_seen = Arc::new(AtomicBool::new(false));
    let tail_written_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let server_flag = delta_seen.clone();
    let server_clock = tail_written_at.clone();
    let server = SseServer::spawn(move |socket| {
        write_chunk(socket, HEAD);
        // Give up after 5s so a buffering provider fails the assertion
        // below instead of hanging the suite.
        wait_for(&server_flag, Duration::from_secs(5));
        *server_clock.lock().unwrap() = Some(Instant::now());
        write_chunk(socket, TAIL);
    });

    let mut events = provider_stream(&server.base_url());
    let mut first_delta_at: Option<Instant> = None;
    let mut text = String::new();

    let drain = async {
        while let Some(event) = events.next().await {
            if let AssistantMessageEvent::TextDelta { delta, .. } = &event {
                if first_delta_at.is_none() {
                    first_delta_at = Some(Instant::now());
                    delta_seen.store(true, Ordering::SeqCst);
                }
                text.push_str(delta);
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), drain)
        .await
        .expect("stream must terminate");

    let first_delta_at = first_delta_at.expect("provider must emit a TextDelta");
    let tail_written_at = tail_written_at
        .lock()
        .unwrap()
        .expect("server must have written the tail");
    assert!(
        first_delta_at < tail_written_at,
        "TextDelta must reach the caller before the final chunk is written \
         (delta at {first_delta_at:?}, tail written at {tail_written_at:?})",
    );
    assert_eq!(text, "Hello");
}

/// A turn abandoned mid-flight keeps whatever was already delivered.
/// With a buffered body there is nothing to keep: no event has been
/// yielded when the caller drops the stream.
#[tokio::test]
async fn cancelling_mid_stream_keeps_the_delivered_deltas() {
    let cancelled = Arc::new(AtomicBool::new(false));

    let server_flag = cancelled.clone();
    let server = SseServer::spawn(move |socket| {
        write_chunk(socket, HEAD);
        // Hold the turn open — the client walks away before the tail.
        wait_for(&server_flag, Duration::from_secs(10));
    });

    let mut events = provider_stream(&server.base_url());
    let delta = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = events.next().await {
            if let AssistantMessageEvent::TextDelta { delta, .. } = event {
                return Some(delta);
            }
        }
        None
    })
    .await
    .expect("a delta must arrive while the turn is still open");

    // Cancellation: drop the stream mid-turn.
    drop(events);
    cancelled.store(true, Ordering::SeqCst);

    assert_eq!(
        delta.as_deref(),
        Some("Hello"),
        "the text delivered before cancellation must survive it",
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn provider_stream(base_url: &str) -> model::api_registry::AssistantMessageEventStream<'static> {
    let mut options = StreamOptions::default();
    options.api_key = Some("test-key".to_string());
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("hi"))],
        tools: None,
    };
    AnthropicMessagesProvider::new().stream(test_model(base_url), context, Some(options))
}

fn test_model(base_url: &str) -> Model {
    Model {
        id: "claude-sonnet-4-20250514".to_string(),
        name: "Claude Sonnet 4".to_string(),
        api: Api::AnthropicMessages,
        provider: Provider::Anthropic,
        base_url: base_url.to_string(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 200_000,
        max_tokens: 8_192,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

/// Block until `flag` flips or `deadline` elapses.
fn wait_for(flag: &AtomicBool, deadline: Duration) {
    let stop_at = Instant::now() + deadline;
    while !flag.load(Ordering::SeqCst) && Instant::now() < stop_at {
        thread::sleep(Duration::from_millis(5));
    }
}

/// Single-connection HTTP server whose response body is scripted by the
/// caller, one chunk at a time.
struct SseServer {
    port: u16,
    _join: thread::JoinHandle<()>,
}

impl SseServer {
    fn spawn<F>(script: F) -> Self
    where
        F: FnOnce(&mut TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().expect("local addr").port();
        let join = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            read_request(&mut socket);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Type: text/event-stream\r\n\
                      Cache-Control: no-cache\r\n\
                      Transfer-Encoding: chunked\r\n\r\n",
                )
                .expect("write response headers");
            socket.flush().expect("flush headers");

            script(&mut socket);

            let _ = socket.write_all(b"0\r\n\r\n");
            let _ = socket.flush();
            let _ = socket.shutdown(Shutdown::Both);
        });
        SseServer { port, _join: join }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Write one HTTP chunk and push it onto the wire immediately.
fn write_chunk(socket: &mut TcpStream, payload: &str) {
    socket
        .write_all(format!("{:X}\r\n{}\r\n", payload.len(), payload).as_bytes())
        .expect("write chunk");
    socket.flush().expect("flush chunk");
}

/// Consume the request head and body so the client never sees a reset.
fn read_request(socket: &mut TcpStream) {
    let mut reader = BufReader::new(socket.try_clone().expect("clone socket"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read request line") == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read request body");
}
