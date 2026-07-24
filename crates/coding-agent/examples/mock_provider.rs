//! Deterministic, API-key-free mock provider for TUI streaming tests.
//!
//! Serves canned OpenAI Completions-style SSE streams over plain HTTP so
//! `hand` can complete a streaming turn while believing it is talking to a
//! real OpenAI-compatible provider. The wire format matches what
//! `crates/model/src/providers/openai_completions.rs` (via the `openai-rust`
//! client) parses: line-delimited `data: {chunk}\n` events, each a
//! `chat.completion.chunk`, terminated by `data: [DONE]`.
//!
//! ## Endpoint
//!
//! `POST {base_url}/chat/completions` — the `openai-rust` client appends
//! `/chat/completions` to the configured `base_url`. Point `models.json` at
//! `http://127.0.0.1:<port>/v1` (see
//! `tests/fixtures/tui/mock-provider/models.json`).
//!
//! ## Scenario selection (first match wins)
//!
//! 1. `?scenario=<name>` query parameter on the request URL.
//! 2. `X-Mock-Scenario: <name>` request header.
//! 3. `MOCK_PROVIDER_SCENARIO` environment variable.
//! 4. Default: `text`.
//!
//! Scenarios:
//! - `text`: a short multi-delta text turn.
//! - `thinking`: reasoning deltas followed by a text answer.
//! - `slow`: text streamed one word at a time with a per-chunk delay
//!   (exercises the loader lifecycle).
//! - `stall`: emits an early content delta then withholds the next one for a
//!   long time (exercises the stream watchdog / stall detection), then
//!   finishes.
//! - `tool_call`: a single `read` tool call; returns terminal text once the
//!   tool result is back (does not loop).
//! - `edit_tool`: an `edit` tool call with old/new string arguments; returns
//!   terminal text on the tool-result round.
//! - `write_tool`: a `write` tool call creating a new file; returns terminal
//!   text on the tool-result round.
//! - `image_result`: a `read` tool call whose downstream result carries an
//!   image block. The image lands in the tool *result* (which the caller
//!   synthesises); this scenario emits the tool call that triggers it, then
//!   returns terminal text on the tool-result round.
//! - `streamed_fence`: a text turn that opens a fenced code block *mid-stream*
//!   (the opening ``` and body arrive across deltas, the closing fence last), so
//!   a live probe can observe mid-stream containment and settle-once behaviour.
//! - `error`: a partial text turn that ends with `finish_reason: "error"`.
//!
//! The tool-call scenarios (`tool_call`, `edit_tool`, `write_tool`,
//! `image_result`) are two-round: the first request emits the tool call, and the
//! follow-up request — which carries the tool result — returns a terminal text
//! response. This terminates the agent loop (a stateless single-response mock
//! would otherwise re-emit the same tool call forever), so the tool-call
//! streaming and rendering are probable without tripping the turn watchdog.
//!
//! ## Run
//!
//! ```text
//! cargo run --example mock_provider -p hand-coding-agent
//! MOCK_PROVIDER_PORT=39217 cargo run --example mock_provider -p hand-coding-agent
//! ```
//!
//! The server binds `127.0.0.1:<port>` (default `39217`, override with
//! `MOCK_PROVIDER_PORT`) and prints `mock-provider listening on ...` once
//! ready, so test harnesses can wait for that line. Set `MOCK_PROVIDER_PORT=0`
//! to bind an OS-assigned ephemeral port (what the test harness does to avoid
//! port collisions and TIME_WAIT on repeated runs); the printed ready line then
//! carries the real bound port, which the harness parses. Timing knobs:
//! `MOCK_PROVIDER_SLOW_MS` (per-chunk delay for `slow`, default 60) and
//! `MOCK_PROVIDER_STALL_MS` (withhold duration for `stall`, default 3000).

use std::io::Write as _;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_PORT: u16 = 39217;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("MOCK_PROVIDER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("mock-provider failed to bind {addr}: {e}"));

    // Print the address the OS actually bound. With `MOCK_PROVIDER_PORT=0`
    // (the test path) the kernel assigns an ephemeral port, so the harness must
    // learn the real port from this line rather than assuming the requested
    // one; a specific non-zero port (e.g. the documented 39217) round-trips
    // unchanged.
    let bound_addr = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("mock-provider failed to read local addr: {e}"));

    // Emit a ready line on stdout (flushed) so a spawning harness can block
    // until the socket is accepting connections instead of racing a sleep.
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "mock-provider listening on http://{bound_addr}");
    let _ = stdout.flush();

    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream).await {
                        eprintln!("mock-provider connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("mock-provider accept error: {e}"),
        }
    }
}

/// Parse the request head, choose a scenario, and stream the canned SSE.
async fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let request = read_request_head(&mut stream).await?;

    // Only the completions endpoint is meaningful; anything else gets a 404
    // so misconfiguration surfaces loudly rather than hanging.
    if !request.path.contains("/chat/completions") {
        let body = b"not found";
        let head = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        return Ok(());
    }

    let scenario = resolve_scenario(&request);

    // SSE response head. `Connection: close` keeps the framing simple — one
    // request per connection, matching how `reqwest`'s streaming body reads.
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache\r\n\
                Connection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;

    stream_scenario(&mut stream, &scenario, request.has_tool_result).await?;

    stream.write_all(b"data: [DONE]\n\n").await?;
    stream.flush().await?;
    stream.shutdown().await.ok();
    Ok(())
}

/// Minimal parsed request head — only the pieces the mock needs.
struct RequestHead {
    path: String,
    scenario_header: Option<String>,
    /// Whether the request body already carries a tool result (a `"role":"tool"`
    /// message, or an Anthropic `tool_result` content block). True on the
    /// *second* round of a tool-call scenario — after the agent ran the tool and
    /// sent the result back — so the provider can return a terminal text
    /// response instead of re-emitting the same tool call and looping forever.
    has_tool_result: bool,
}

/// Read the request head, then drain the request body.
///
/// Draining matters: a streaming HTTP client (`reqwest` via `openai-rust`)
/// writes the JSON body after the headers. If we send our response and close
/// the socket before consuming that body, the client's in-flight write is
/// reset and it reports "error decoding response body" instead of parsing the
/// stream. So we parse `Content-Length` and read exactly that many body bytes
/// (counting any already buffered past the header terminator) before we reply.
async fn read_request_head(stream: &mut TcpStream) -> std::io::Result<RequestHead> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break Some(pos);
        }
        if buf.len() > 64 * 1024 {
            break None; // guard against unbounded headers
        }
    };

    let head_text = match header_end {
        Some(pos) => String::from_utf8_lossy(&buf[..pos]).into_owned(),
        None => String::from_utf8_lossy(&buf).into_owned(),
    };
    let mut lines = head_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    // "POST /v1/chat/completions?scenario=text HTTP/1.1"
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let mut scenario_header = None;
    let mut content_length: usize = 0;
    for l in lines {
        if let Some((name, value)) = l.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("x-mock-scenario") {
                scenario_header = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    // Drain the body so the client finishes its write cleanly, accumulating it
    // so we can detect a tool-result round (see `has_tool_result`).
    let mut body = Vec::new();
    if let Some(pos) = header_end {
        let body_start = pos + 4;
        body.extend_from_slice(&buf[body_start..]);
        let already = buf.len() - body_start;
        let mut remaining = content_length.saturating_sub(already);
        while remaining > 0 {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            remaining = remaining.saturating_sub(n);
        }
    }
    let body_text = String::from_utf8_lossy(&body);
    // A tool-result round is present when the messages carry a `"role":"tool"`
    // entry (OpenAI shape) or a `tool_result` content block (Anthropic shape).
    // Whitespace between the key and value is tolerated so a pretty-printed body
    // still matches.
    let has_tool_result = contains_role_tool(&body_text) || body_text.contains("tool_result");

    Ok(RequestHead {
        path,
        scenario_header,
        has_tool_result,
    })
}

/// Whether the request body carries an OpenAI-shape `"role": "tool"` message,
/// tolerating optional whitespace after the colon (pretty-printed bodies).
fn contains_role_tool(body: &str) -> bool {
    body.match_indices("\"role\"").any(|(idx, _)| {
        let rest = &body[idx + "\"role\"".len()..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            return false;
        };
        rest.trim_start().starts_with("\"tool\"")
    })
}

/// Byte offset of the start of the `\r\n\r\n` header terminator, if present.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Resolve the scenario name from (in order) query param, header, env, default.
fn resolve_scenario(request: &RequestHead) -> String {
    if let Some((_, query)) = request.path.split_once('?')
        && let Some(scenario) = query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "scenario").then(|| v.to_string())
        })
    {
        return scenario;
    }
    if let Some(h) = &request.scenario_header {
        return h.clone();
    }
    if let Ok(env) = std::env::var("MOCK_PROVIDER_SCENARIO") {
        return env;
    }
    "text".to_string()
}

/// Write the canned SSE chunk sequence for a scenario. Each chunk is a
/// serialized `chat.completion.chunk` JSON object framed as `data: {...}\n`.
///
/// `has_tool_result` is true on the second round of a tool-call scenario (the
/// agent has run the tool and sent the result back). The tool-call scenarios
/// key off it to return a terminal text response instead of re-emitting the same
/// tool call — without it the stateless provider would loop forever, and the
/// tool-call streaming / rendering could only be probed by tripping the
/// watchdog.
async fn stream_scenario(
    stream: &mut TcpStream,
    scenario: &str,
    has_tool_result: bool,
) -> std::io::Result<()> {
    let slow_ms: u64 = env_u64("MOCK_PROVIDER_SLOW_MS", 60);
    let stall_ms: u64 = env_u64("MOCK_PROVIDER_STALL_MS", 3000);

    match scenario {
        "text" => {
            for piece in ["Hello", " from", " the", " mock", " provider."] {
                write_chunk(stream, &text_delta_chunk(piece)).await?;
            }
            write_chunk(stream, &finish_chunk("stop")).await?;
            write_chunk(stream, &usage_chunk(12, 6)).await?;
        }
        "thinking" => {
            for piece in ["Let me ", "think about ", "this. "] {
                write_chunk(stream, &reasoning_delta_chunk(piece)).await?;
            }
            for piece in ["The answer ", "is 42."] {
                write_chunk(stream, &text_delta_chunk(piece)).await?;
            }
            write_chunk(stream, &finish_chunk("stop")).await?;
            write_chunk(stream, &usage_chunk(20, 10)).await?;
        }
        "slow" => {
            for piece in [
                "Streaming ",
                "one ",
                "word ",
                "at ",
                "a ",
                "time ",
                "slowly.",
            ] {
                write_chunk(stream, &text_delta_chunk(piece)).await?;
                tokio::time::sleep(Duration::from_millis(slow_ms)).await;
            }
            write_chunk(stream, &finish_chunk("stop")).await?;
            write_chunk(stream, &usage_chunk(14, 7)).await?;
        }
        "stall" => {
            // First content delta arrives, then a long silence with no bytes
            // to trip a stream watchdog, then the stream recovers and ends.
            write_chunk(stream, &text_delta_chunk("Working")).await?;
            tokio::time::sleep(Duration::from_millis(stall_ms)).await;
            write_chunk(stream, &text_delta_chunk("... done.")).await?;
            write_chunk(stream, &finish_chunk("stop")).await?;
            write_chunk(stream, &usage_chunk(8, 4)).await?;
        }
        "tool_call" => {
            // First round: request the tool. Second round (the tool result is
            // back): answer with terminal text so the turn ends and does not
            // loop — the tool-call streaming / rendering is probable without a
            // watchdog timeout.
            if has_tool_result {
                emit_final_text(stream, "The file has been read.").await?;
            } else {
                emit_tool_call(
                    stream,
                    "call_mock_read_0001",
                    "read",
                    r#"{"path":"/tmp/mock.txt"}"#,
                )
                .await?;
            }
        }
        "edit_tool" => {
            if has_tool_result {
                emit_final_text(stream, "The edit has been applied.").await?;
            } else {
                emit_tool_call(
                    stream,
                    "call_mock_edit_0001",
                    "edit",
                    r#"{"path":"/tmp/mock.txt","oldString":"foo","newString":"bar"}"#,
                )
                .await?;
            }
        }
        "write_tool" => {
            if has_tool_result {
                emit_final_text(stream, "The file has been written.").await?;
            } else {
                emit_tool_call(
                    stream,
                    "call_mock_write_0001",
                    "write",
                    r#"{"path":"/tmp/mock-new.txt","content":"created by mock\n"}"#,
                )
                .await?;
            }
        }
        "image_result" => {
            // The image itself lands in the *tool result* the caller
            // synthesises; the assistant turn only needs to request a tool
            // whose result is an image block (e.g. a screenshot reader).
            if has_tool_result {
                emit_final_text(stream, "Here is what the screenshot shows.").await?;
            } else {
                emit_tool_call(
                    stream,
                    "call_mock_image_0001",
                    "read",
                    r#"{"path":"/tmp/mock-image.png"}"#,
                )
                .await?;
            }
        }
        "streamed_fence" => {
            // A code fence that OPENS mid-stream: intro text, the opening ```rust
            // and body arrive across several deltas while the closing fence lands
            // only at the end. This lets a live probe observe the mid-stream
            // containment (the open fence is closed defensively in the preview)
            // and the settle-once behaviour when the final closed snapshot
            // commits (VAL-CHAT-033).
            for piece in [
                "Here is the code:\n\n",
                "```rust\n",
                "fn main() {\n",
                "    println!(\"hi\");\n",
                "}\n",
                "```\n",
            ] {
                write_chunk(stream, &text_delta_chunk(piece)).await?;
                tokio::time::sleep(Duration::from_millis(slow_ms)).await;
            }
            write_chunk(stream, &finish_chunk("stop")).await?;
            write_chunk(stream, &usage_chunk(16, 24)).await?;
        }
        "error" => {
            write_chunk(stream, &text_delta_chunk("partial before error")).await?;
            write_chunk(stream, &finish_chunk("error")).await?;
        }
        other => {
            // Unknown scenario: fall back to a one-line text turn naming the
            // requested scenario, so a typo is visible in the transcript.
            write_chunk(
                stream,
                &text_delta_chunk(&format!("unknown scenario: {other}")),
            )
            .await?;
            write_chunk(stream, &finish_chunk("stop")).await?;
        }
    }
    Ok(())
}

/// Emit a full tool call across a start chunk (id + name + first args frag)
/// and an argument-completion chunk, then the tool-use finish chunk. This
/// mirrors how real providers stream `tool_calls` deltas: the `id`/`name`
/// arrive first, argument fragments follow, keyed by `index`.
async fn emit_tool_call(
    stream: &mut TcpStream,
    id: &str,
    name: &str,
    arguments_json: &str,
) -> std::io::Result<()> {
    // Split the arguments in two to exercise the streaming-accumulation path
    // in the delta handler.
    let mid = arguments_json.len() / 2;
    let (head, tail) = arguments_json.split_at(mid);

    write_chunk(stream, &tool_call_start_chunk(id, name, head)).await?;
    write_chunk(stream, &tool_call_args_chunk(tail)).await?;
    write_chunk(stream, &finish_chunk("tool_calls")).await?;
    write_chunk(stream, &usage_chunk(30, 15)).await?;
    Ok(())
}

/// Emit a terminal, single-delta text turn: one content delta, a `stop` finish,
/// and a usage chunk. Used by the tool-call scenarios on the second round (once
/// the tool result is back) so the turn ends with a real answer instead of
/// re-requesting the same tool and looping.
async fn emit_final_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    write_chunk(stream, &text_delta_chunk(text)).await?;
    write_chunk(stream, &finish_chunk("stop")).await?;
    write_chunk(stream, &usage_chunk(40, 12)).await?;
    Ok(())
}

/// Frame one chunk JSON value as an SSE `data:` line and flush it so the
/// per-chunk timing of `slow`/`stall` is observable on the wire.
async fn write_chunk(stream: &mut TcpStream, chunk: &str) -> std::io::Result<()> {
    stream.write_all(b"data: ").await?;
    stream.write_all(chunk.as_bytes()).await?;
    stream.write_all(b"\n\n").await?;
    stream.flush().await
}

fn text_delta_chunk(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[{{"index":0,"delta":{{"content":{}}},"finish_reason":null}}]}}"#,
        json_string(text)
    )
}

fn reasoning_delta_chunk(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[{{"index":0,"delta":{{"reasoning":{}}},"finish_reason":null}}]}}"#,
        json_string(text)
    )
}

fn tool_call_start_chunk(id: &str, name: &str, args_fragment: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":{},"type":"function","function":{{"name":{},"arguments":{}}}}}]}},"finish_reason":null}}]}}"#,
        json_string(id),
        json_string(name),
        json_string(args_fragment)
    )
}

fn tool_call_args_chunk(args_fragment: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"function":{{"arguments":{}}}}}]}},"finish_reason":null}}]}}"#,
        json_string(args_fragment)
    )
}

fn finish_chunk(reason: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[{{"index":0,"delta":{{}},"finish_reason":{}}}]}}"#,
        json_string(reason)
    )
}

fn usage_chunk(prompt: u32, completion: u32) -> String {
    format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion.chunk","created":1700000000,"model":"mock-model","choices":[],"usage":{{"prompt_tokens":{prompt},"completion_tokens":{completion},"total_tokens":{}}}}}"#,
        prompt + completion
    )
}

/// Escape a string as a JSON string literal (with surrounding quotes). Kept
/// dependency-free: the example intentionally links only `tokio`.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
