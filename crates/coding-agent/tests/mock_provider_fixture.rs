//! Verifies the mock-provider test fixture: the canned-SSE HTTP server
//! (`examples/mock_provider.rs`), the `models.json` that points `hand` at it,
//! and the `--resume` session fixtures.
//!
//! These tests need no API key and make no real network calls. The mock
//! server is spawned as a subprocess (`cargo run --example mock_provider`)
//! bound to an OS-assigned ephemeral port (read back from its ready line);
//! each scenario endpoint is curled with `reqwest` and its SSE shape asserted.
//! The fixtures live at the workspace root under `tests/fixtures/tui/`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hand_coding_agent::core::auth_storage::AuthStorage;
use hand_coding_agent::core::model_registry::ModelRegistry;
use hand_coding_agent::core::session_manager::{SessionEntry, load_entries_from_file};
use model::Message;

/// Workspace root = two levels up from this crate's manifest dir
/// (`crates/coding-agent`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests/fixtures/tui")
}

/// Spawned mock server, killed on drop so a failed assertion never leaves an
/// orphan listening socket.
struct MockServer {
    child: Child,
    base_url: String,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the mock server on an OS-assigned ephemeral port and block until it
/// prints its ready line, from which the real port is parsed. Binding port 0
/// avoids collisions and TIME_WAIT on repeated or overlapping runs.
///
/// The readiness wait is bounded by a hard wall-clock timeout: the blocking
/// `read_line` loop runs on a background thread that reports the parsed port (or
/// a failure) over a channel, so a child that never prints a ready line (a build
/// error that only writes stderr, say) fails the test instead of hanging CI.
fn start_mock_server() -> MockServer {
    let mut child = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "--example",
            "mock_provider",
            "-p",
            "hand-coding-agent",
        ])
        // Bind an ephemeral port; the real one is read back from the ready line.
        .env("MOCK_PROVIDER_PORT", "0")
        // Keep the stall short so the CI run of the stall scenario is quick;
        // the watchdog wiring lives in the driver (next feature), not here.
        .env("MOCK_PROVIDER_STALL_MS", "300")
        .env("MOCK_PROVIDER_SLOW_MS", "10")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn mock_provider example");

    let stdout = child.stdout.take().expect("child stdout");

    // Read the ready line on a background thread and hand back the parsed port
    // (or an error string) over a channel. `recv_timeout` on the main thread
    // then enforces a hard ceiling: a silent or failed child fails the test
    // within the timeout instead of blocking `read_line` forever.
    let (tx, rx) = std::sync::mpsc::channel::<Result<u16, String>>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err(
                        "mock_provider exited before printing ready line".to_string()
                    ));
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(Err(format!("read ready line failed: {e}")));
                    return;
                }
            }
            // "mock-provider listening on http://127.0.0.1:<port>"
            if let Some(port) = line
                .split(':')
                .next_back()
                .and_then(|p| p.trim().parse::<u16>().ok())
                .filter(|_| line.contains("listening on"))
            {
                let _ = tx.send(Ok(port));
                return;
            }
        }
    });

    let port = match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(port)) => port,
        Ok(Err(e)) => {
            let _ = child.kill();
            panic!("{e}");
        }
        Err(_) => {
            let _ = child.kill();
            panic!("timed out waiting for mock_provider ready line");
        }
    };

    MockServer {
        child,
        base_url: format!("http://127.0.0.1:{port}/v1"),
    }
}

/// POST an empty completions request for `scenario` and return the raw SSE
/// body. The mock ignores the request body; the scenario query param drives
/// the response.
async fn fetch_scenario(base_url: &str, scenario: &str) -> String {
    let client = reqwest::Client::new();
    let url = format!("{base_url}/chat/completions?scenario={scenario}");
    let resp = client
        .post(&url)
        .bearer_auth("mock-key-no-real-auth")
        .json(&serde_json::json!({ "model": "mock-model", "stream": true }))
        .send()
        .await
        .expect("request mock server");
    assert!(resp.status().is_success(), "scenario {scenario} status");
    resp.text().await.expect("read SSE body")
}

/// Collect the JSON payloads of every `data:` line except the `[DONE]`
/// terminator.
fn sse_chunks(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).expect("chunk is valid json"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn text_scenario_emits_content_deltas_and_done() {
    let server = start_mock_server();
    let body = fetch_scenario(&server.base_url, "text").await;

    assert!(body.trim_end().ends_with("[DONE]"), "must end with [DONE]");
    let chunks = sse_chunks(&body);

    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Hello from the mock provider.");

    let finished = chunks
        .iter()
        .any(|c| c["choices"][0]["finish_reason"] == "stop");
    assert!(finished, "expected a finish_reason: stop chunk");

    let has_usage = chunks.iter().any(|c| c["usage"]["total_tokens"] == 18);
    assert!(has_usage, "expected terminal usage chunk");
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_scenario_emits_reasoning_then_text() {
    let server = start_mock_server();
    let body = fetch_scenario(&server.base_url, "thinking").await;
    let chunks = sse_chunks(&body);

    let reasoning: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["reasoning"].as_str())
        .collect();
    assert_eq!(reasoning, "Let me think about this. ");

    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "The answer is 42.");
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_scenario_streams_multiple_deltas() {
    let server = start_mock_server();
    let body = fetch_scenario(&server.base_url, "slow").await;
    let chunks = sse_chunks(&body);
    let content_deltas = chunks
        .iter()
        .filter(|c| c["choices"][0]["delta"]["content"].is_string())
        .count();
    assert!(
        content_deltas >= 5,
        "slow scenario should stream several deltas, got {content_deltas}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stall_scenario_completes_after_a_gap() {
    let server = start_mock_server();
    // With MOCK_PROVIDER_STALL_MS=300 the stream completes; the point is that
    // a gap occurs between the first and second content delta. We assert the
    // stream is well-formed and finishes.
    let body = fetch_scenario(&server.base_url, "stall").await;
    assert!(body.trim_end().ends_with("[DONE]"));
    let chunks = sse_chunks(&body);
    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Working... done.");
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_scenario_emits_a_read_call() {
    let server = start_mock_server();
    let body = fetch_scenario(&server.base_url, "tool_call").await;
    let chunks = sse_chunks(&body);

    // First tool-call chunk carries id + name.
    let name = chunks
        .iter()
        .find_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str());
    assert_eq!(name, Some("read"));

    // Reassemble the streamed arguments fragments.
    let args: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    let parsed: serde_json::Value = serde_json::from_str(&args).expect("args json");
    assert_eq!(parsed["path"], "/tmp/mock.txt");

    let finished = chunks
        .iter()
        .any(|c| c["choices"][0]["finish_reason"] == "tool_calls");
    assert!(finished, "expected finish_reason: tool_calls");
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_and_write_tool_scenarios_carry_expected_arguments() {
    let server = start_mock_server();

    let edit = sse_chunks(&fetch_scenario(&server.base_url, "edit_tool").await);
    let edit_name = edit
        .iter()
        .find_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str());
    assert_eq!(edit_name, Some("edit"));

    let edit_args: String = edit
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    let edit_parsed: serde_json::Value = serde_json::from_str(&edit_args).unwrap();
    // The edit tool's schema requires `file_path` / `old_string` / `new_string`;
    // the earlier `path` / `oldString` / `newString` keys made the tool error
    // with `"file_path" is a required property` instead of rendering a diff. The
    // distinct old/new strings give the diff renderer real +/- content
    // (VAL-CHAT-039).
    assert_eq!(
        edit_parsed["file_path"], "/tmp/mock.txt",
        "edit args must use the schema key `file_path`, not `path`"
    );
    assert_eq!(edit_parsed["old_string"], "foo");
    assert_eq!(edit_parsed["new_string"], "bar");
    assert_ne!(
        edit_parsed["old_string"], edit_parsed["new_string"],
        "old/new strings must differ so the diff has +/- rows"
    );

    let write = sse_chunks(&fetch_scenario(&server.base_url, "write_tool").await);
    let write_name = write
        .iter()
        .find_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str());
    assert_eq!(write_name, Some("write"));
}

#[tokio::test(flavor = "multi_thread")]
async fn image_result_scenario_requests_an_image_read() {
    let server = start_mock_server();
    let chunks = sse_chunks(&fetch_scenario(&server.base_url, "image_result").await);
    let args: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    let parsed: serde_json::Value = serde_json::from_str(&args).unwrap();
    assert_eq!(parsed["path"], "/tmp/mock-image.png");
}

#[tokio::test(flavor = "multi_thread")]
async fn error_scenario_emits_partial_text_then_finish_error() {
    let server = start_mock_server();
    let chunks = sse_chunks(&fetch_scenario(&server.base_url, "error").await);

    // A partial assistant text delta arrives before the stream errors out.
    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "partial before error");

    // The turn ends with a `finish_reason: error` chunk rather than `stop`.
    let errored = chunks
        .iter()
        .any(|c| c["choices"][0]["finish_reason"] == "error");
    assert!(errored, "expected a finish_reason: error chunk");

    // An OpenAI-shape error object is streamed as a `data:` line. This is the
    // real failure signal: the `openai-rust` client fails to deserialize it as a
    // `chat.completion.chunk`, so the model layer surfaces a `StopReason::Error`
    // assistant message and the TUI's OSC 9;4 error state fires (VAL-CHAT-018). A
    // bare `finish_reason: "error"` chunk alone maps to `StopReason::Stop`
    // client-side and would never signal a failure.
    let has_error_object = chunks.iter().any(|c| c["error"]["message"].is_string());
    assert!(
        has_error_object,
        "expected an OpenAI error object `data:` line so the client observes a failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_fence_scenario_opens_and_closes_a_fence_across_deltas() {
    let server = start_mock_server();
    let chunks = sse_chunks(&fetch_scenario(&server.base_url, "streamed_fence").await);

    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();

    // The opening fence carries a language tag and both the opening and closing
    // fences appear across the streamed deltas (two ``` occurrences).
    assert!(
        text.contains("```rust"),
        "expected an opening fence with a language tag, got: {text}"
    );
    assert!(
        text.matches("```").count() >= 2,
        "expected both an opening and a closing fence, got: {text}"
    );
}

#[test]
fn models_json_registers_the_mock_model() {
    // Load the fixture models.json through the real registry so we prove
    // `hand` would resolve a `mock-model` on the `openai` provider pointing
    // at the local mock server. AuthStorage is an isolated temp so no user
    // credentials leak in.
    let tmp = tempfile::tempdir().unwrap();
    let auth = AuthStorage::at(tmp.path().join("auth.json"));
    let models_json = fixtures_dir().join("mock-provider/models.json");
    assert!(models_json.exists(), "fixture models.json missing");

    let registry = ModelRegistry::with_path(auth, Some(models_json));
    assert!(
        registry.error().is_none(),
        "models.json load error: {:?}",
        registry.error()
    );

    let mock = registry
        .all()
        .iter()
        .find(|m| m.provider.as_str() == "openai" && m.id == "mock-model")
        .expect("mock-model should be registered");
    assert!(
        mock.base_url.starts_with("http://127.0.0.1:"),
        "mock-model base_url must point at localhost, got {}",
        mock.base_url
    );
    assert_eq!(mock.api, model::types::Api::OpenAICompletions);
}

#[test]
fn thinking_session_fixture_loads_with_a_thinking_block() {
    let path = fixtures_dir().join("sessions/thinking-blocks.jsonl");
    let entries = load_entries_from_file(&path).expect("load thinking fixture");

    let header = matches!(entries.first(), Some(SessionEntry::Session(_)));
    assert!(header, "first entry must be the session header");

    let has_thinking = entries.iter().any(|e| match e {
        SessionEntry::Message { message, .. } => matches!(
            message.as_ref(),
            Message::Assistant(a) if a.content.iter().any(|b|
                matches!(b, model::AssistantContentBlock::Thinking(_)))
        ),
        _ => false,
    });
    assert!(has_thinking, "expected an assistant thinking block");
}

#[test]
fn error_ended_session_fixture_loads_with_an_error_message() {
    let path = fixtures_dir().join("sessions/error-ended.jsonl");
    let entries = load_entries_from_file(&path).expect("load error fixture");

    let has_error = entries.iter().any(|e| match e {
        SessionEntry::Message { message, .. } => matches!(
            message.as_ref(),
            Message::Assistant(a)
                if a.stop_reason == model::StopReason::Error && a.error_message.is_some()
        ),
        _ => false,
    });
    assert!(has_error, "expected an error-ended assistant message");
}

#[test]
fn multi_message_session_fixture_loads_tool_call_and_image_result() {
    let path = fixtures_dir().join("sessions/multi-message-resume.jsonl");
    let entries = load_entries_from_file(&path).expect("load multi fixture");

    // A model_change entry.
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, SessionEntry::ModelChange { .. })),
        "expected a model_change entry"
    );

    // At least one tool call in an assistant message.
    let has_tool_call = entries.iter().any(|e| match e {
        SessionEntry::Message { message, .. } => matches!(
            message.as_ref(),
            Message::Assistant(a) if a.content.iter().any(|b|
                matches!(b, model::AssistantContentBlock::ToolCall(_)))
        ),
        _ => false,
    });
    assert!(has_tool_call, "expected an assistant tool call");

    // A tool result carrying an image block.
    let has_image_result = entries.iter().any(|e| match e {
        SessionEntry::Message { message, .. } => matches!(
            message.as_ref(),
            Message::ToolResult(tr) if tr.content.iter().any(|c|
                matches!(c, model::ToolResultContent::Image(_)))
        ),
        _ => false,
    });
    assert!(has_image_result, "expected an image-block tool result");

    // Several user/assistant messages — a real multi-turn resume.
    let message_count = entries
        .iter()
        .filter(|e| matches!(e, SessionEntry::Message { .. }))
        .count();
    assert!(
        message_count >= 6,
        "expected a multi-turn conversation, got {message_count} messages"
    );
}
