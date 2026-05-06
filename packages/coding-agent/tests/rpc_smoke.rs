//! End-to-end RPC dispatcher integration test.
//!
//! Drives `run_rpc_server` through a scripted multi-turn session over an
//! in-memory `tokio::io::duplex` pair, with a mocked provider so the test
//! requires no API keys and no network. This is the Phase 1 acceptance gate
//! for the RPC mode port.

use std::time::Duration;

use futures::StreamExt;
use hand_coding_agent::AgentSession;
use hand_coding_agent::rpc::{read_jsonl, run_rpc_server};
use model::Api;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader, duplex};

mod common;

use common::{mock_text_provider, test_model};

/// Drive the dispatcher through three commands (prompt, get_state, prompt)
/// and assert the wire shape, ordering, and state progression.
#[tokio::test]
async fn three_turn_rpc_session() {
    // 1. Build a `model::Client` whose registry has a mock text provider
    //    wired to the test model's API. The mock's `stream` body is
    //    re-evaluated on every call (it's an `async_stream::stream!`
    //    closure rebuilt from `self.text`), so a single registration
    //    serves all three turns.
    let client = model::Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        mock_text_provider("hello"),
        Some("test".into()),
    );

    // 2. Build the session with the mock-equipped client.
    let session = AgentSession::in_memory_with_client(test_model(), Vec::new(), client);

    // 3. Wire stdin/stdout to in-memory duplex streams.
    let (mut writer_to_dispatcher, dispatcher_in) = duplex(8192);
    let (dispatcher_out, reader_from_dispatcher) = duplex(8192);

    // 4. Spawn the dispatcher.
    let dispatcher_handle = tokio::spawn(async move {
        run_rpc_server(BufReader::new(dispatcher_in), dispatcher_out, session).await
    });

    // 5. Drive three turns then close stdin.
    writer_to_dispatcher
        .write_all(b"{\"type\":\"prompt\",\"message\":\"hi\",\"id\":\"1\"}\n")
        .await
        .unwrap();
    writer_to_dispatcher
        .write_all(b"{\"type\":\"get_state\",\"id\":\"2\"}\n")
        .await
        .unwrap();
    writer_to_dispatcher
        .write_all(b"{\"type\":\"prompt\",\"message\":\"again\",\"id\":\"3\"}\n")
        .await
        .unwrap();
    drop(writer_to_dispatcher);

    // 6. Drain all frames within a wall-clock budget. The dispatcher will
    //    EOF its writer once the input stream closes; that's our signal
    //    the stream is done.
    let frames: Vec<Value> = tokio::time::timeout(Duration::from_secs(5), async move {
        let reader = BufReader::new(reader_from_dispatcher);
        let mut stream = Box::pin(read_jsonl::<_, Value>(reader));
        let mut frames = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("invalid frame from dispatcher"));
        }
        frames
    })
    .await
    .expect("timed out waiting for dispatcher to drain");

    // 7. Wait for the dispatcher task to finish (with its own budget).
    let join = tokio::time::timeout(Duration::from_secs(2), dispatcher_handle)
        .await
        .expect("dispatcher did not exit within budget");
    join.expect("dispatcher task panicked")
        .expect("dispatcher returned an error");

    // 8. Wire-shape assertions.
    let responses: Vec<&Value> = frames
        .iter()
        .filter(|f| f["type"] == "response")
        .collect();
    assert!(
        responses.len() >= 3,
        "expected at least 3 responses, got {}: {frames:#?}",
        responses.len()
    );

    let r1 = frames
        .iter()
        .find(|f| f["type"] == "response" && f["id"] == "1")
        .expect("no response with id=1");
    assert_eq!(r1["command"], "prompt");
    assert_eq!(r1["success"], true);

    let r2 = frames
        .iter()
        .find(|f| f["type"] == "response" && f["id"] == "2")
        .expect("no response with id=2");
    assert_eq!(r2["command"], "get_state");
    assert_eq!(r2["success"], true);
    // After turn 1, the session has appended at least the user prompt and
    // the assistant reply (2 messages). `get_state` is dispatched before
    // turn 3 begins, so the count reflects exactly turn-1's contribution.
    let msg_count = r2["data"]["messageCount"].as_u64().unwrap_or(0);
    assert!(
        msg_count >= 2,
        "expected messageCount >= 2 after turn 1, got {:?}; frames: {frames:#?}",
        r2["data"]["messageCount"]
    );

    let r3 = frames
        .iter()
        .find(|f| f["type"] == "response" && f["id"] == "3")
        .expect("no response with id=3");
    assert_eq!(r3["command"], "prompt");
    assert_eq!(r3["success"], true);

    // 9. Event-vs-response ordering. Each `prompt` turn must emit at
    //    least one `event` frame BEFORE its success response (the
    //    single-task dispatcher streams events through the mpsc channel
    //    during the turn, then queues the response after the turn
    //    completes — so events always precede the response in the
    //    serialized output).
    let r1_idx = frames
        .iter()
        .position(|f| f["type"] == "response" && f["id"] == "1")
        .unwrap();
    let event_before_r1 = frames[..r1_idx].iter().any(|f| f["type"] == "event");
    assert!(
        event_before_r1,
        "expected at least one event before response id=1; frames: {frames:#?}"
    );

    let r3_idx = frames
        .iter()
        .position(|f| f["type"] == "response" && f["id"] == "3")
        .unwrap();
    let event_between = frames[r1_idx + 1..r3_idx]
        .iter()
        .any(|f| f["type"] == "event");
    assert!(
        event_between,
        "expected at least one event between response id=1 and response id=3; frames: {frames:#?}"
    );
}
