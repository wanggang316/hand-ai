//! Integration tests for the high-level `Agent` struct.

mod common;

use common::*;
use hand_agent::{
    Agent, AgentEvent, AgentTool, CancellationToken, QueueDeliveryMode, ToolExecutionMode,
    ToolResult,
};
use model::{Api, Client, Message, StopReason, UserMessage};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn setup_text_agent(response: &str) -> Agent {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockTextProvider::new(response)),
        Some("test".into()),
    );
    Agent::new(client, test_model())
}

fn setup_tool_agent(
    tool_name: &str,
    tool_args: serde_json::Value,
    final_text: &str,
) -> (Agent, AgentTool) {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockToolProvider::new(tool_name, tool_args, final_text)),
        Some("test".into()),
    );
    (Agent::new(client, test_model()), echo_tool())
}

// ---------------------------------------------------------------------------
// Construction & accessors
// ---------------------------------------------------------------------------

#[test]
fn default_state_is_empty() {
    let agent = Agent::new(Client::new(), test_model());
    let state = agent.state();
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.error.is_none());
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
}

#[test]
fn set_system_prompt_round_trip() {
    let mut agent = Agent::new(Client::new(), test_model());
    agent.set_system_prompt("You are a helpful assistant.");
    assert_eq!(agent.system_prompt(), "You are a helpful assistant.");
}

#[test]
fn replace_and_clear_messages() {
    let mut agent = Agent::new(Client::new(), test_model());
    agent.replace_messages(vec![
        Message::User(UserMessage::new_text("a")),
        Message::User(UserMessage::new_text("b")),
    ]);
    assert_eq!(agent.messages().len(), 2);
    agent.clear_messages();
    assert!(agent.messages().is_empty());
}

#[test]
fn set_thinking_level_round_trip() {
    let mut agent = Agent::new(Client::new(), test_model());
    assert!(agent.thinking_level().is_none());
    agent.set_thinking_level(Some(model::ThinkingLevel::High));
    assert_eq!(agent.thinking_level(), Some(model::ThinkingLevel::High));
    agent.set_thinking_level(None);
    assert!(agent.thinking_level().is_none());
}

#[test]
fn debug_format_includes_model_id() {
    let agent = Agent::new(Client::new(), test_model());
    let debug = format!("{:?}", agent);
    assert!(debug.contains("Agent"));
    assert!(debug.contains("test-model"));
}

#[test]
fn execution_mode_default_is_parallel() {
    assert_eq!(ToolExecutionMode::default(), ToolExecutionMode::Parallel);
}

#[test]
fn queue_delivery_mode_default_is_one_at_a_time() {
    assert_eq!(QueueDeliveryMode::default(), QueueDeliveryMode::OneAtATime);
}

// ---------------------------------------------------------------------------
// Prompt + transcript
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prompt_text_round_trip() {
    let mut agent = setup_text_agent("Hello world!");
    agent.prompt("Hi").await.unwrap();
    assert!(agent.messages().len() >= 2);
}

#[tokio::test]
async fn prompt_with_message_batch() {
    let mut agent = setup_text_agent("reply");
    let messages = vec![
        Message::User(UserMessage::new_text("msg1")),
        Message::User(UserMessage::new_text("msg2")),
    ];
    agent.prompt(messages).await.unwrap();
    assert!(agent.messages().len() >= 3);
}

#[tokio::test]
async fn prompt_tool_call_run() {
    let (mut agent, tool) = setup_tool_agent(
        "echo",
        serde_json::json!({"message": "test"}),
        "Done with tools",
    );
    agent.add_tool(tool);
    agent.prompt("Use the echo tool").await.unwrap();
    // user, assistant (tool call), tool result, assistant (final text)
    assert!(agent.messages().len() >= 4);
}

// ---------------------------------------------------------------------------
// Listener wiring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_receives_events_and_unsubscribes_on_drop() {
    let mut agent = setup_text_agent("Hello");
    let received = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let received_clone = received.clone();
    let handle = agent.subscribe(move |event, _cancel| {
        received_clone.lock().unwrap().push(event_kind(event));
    });
    agent.prompt("Hi").await.unwrap();
    drop(handle);

    let kinds = received.lock().unwrap().clone();
    assert_eq!(kinds.first(), Some(&"agent_start"));
    assert_eq!(kinds.last(), Some(&"agent_end"));

    // Second run with handle dropped — no new events recorded.
    let len_before = kinds.len();
    agent.prompt("Hi again").await.unwrap();
    assert_eq!(received.lock().unwrap().len(), len_before);
}

// ---------------------------------------------------------------------------
// Lifecycle error fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn truncated_stream_synthesizes_error_message() {
    // Provider closes the stream after `Start` without ever sending `Done` or
    // `Error`. The loop must (1) replace the partial with a synthesized error
    // assistant in the transcript so there are no orphan partials, (2) emit a
    // matched `MessageEnd` so subscribers see balanced lifecycle events, and
    // (3) record the failure on runtime state and emit exactly one `AgentEnd`.
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(TruncatedStreamProvider),
        Some("test".into()),
    );
    let mut agent = Agent::new(client, test_model());

    let agent_end_count = Arc::new(Mutex::new(0u32));
    let count_clone = agent_end_count.clone();
    let _handle = agent.subscribe(move |event, _cancel| {
        if let AgentEvent::AgentEnd { .. } = event {
            *count_clone.lock().unwrap() += 1;
        }
    });

    let result = agent.prompt("hi").await;
    // Provider-level failure surfaces as Ok(stop_reason=Error), matching the
    // `AssistantMessageEvent::Error` path. Callers detect failure via
    // `stop_reason` / runtime `error`, not via Err.
    assert!(
        result.is_ok(),
        "truncated stream should be handled in-place, got {result:?}"
    );
    assert_eq!(*agent_end_count.lock().unwrap(), 1);
    assert!(agent.state().error.is_some());

    // Transcript must contain exactly one assistant message (no orphan partial).
    let assistants: Vec<_> = agent
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant(_)))
        .collect();
    assert_eq!(
        assistants.len(),
        1,
        "expected a single closed assistant message, got {}",
        assistants.len()
    );
    assert!(matches!(
        assistants.last(),
        Some(Message::Assistant(a)) if a.stop_reason == StopReason::Error
    ));
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn abort_cancels_in_flight_run() {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(SlowTextProvider {
            delay_ms: 500,
            response_text: "would never arrive".into(),
        }),
        Some("test".into()),
    );
    let mut agent = Agent::new(client, test_model());
    let abort_handle = agent.abort_handle();

    let abort_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        abort_handle.abort();
    });

    let start = std::time::Instant::now();
    agent.prompt("Hi").await.unwrap();
    let elapsed = start.elapsed();
    abort_task.await.unwrap();

    assert!(
        elapsed < Duration::from_millis(300),
        "abort took {elapsed:?}; expected < 300ms"
    );

    let last = agent.messages().last().unwrap();
    let stop_reason = match last {
        Message::Assistant(a) => a.stop_reason,
        _ => panic!("expected last message to be assistant"),
    };
    assert_eq!(stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn abort_drops_long_running_tool_future() {
    // Provider returns a tool call quickly, the tool would otherwise block for
    // 500ms. abort() must race the tool future against `cancel.cancelled()`
    // so the run unwinds well before the tool's natural completion. Tools
    // built via `AgentTool::simple` cannot observe the cancel token directly,
    // so this is a load-bearing guarantee of the loop itself.
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockToolProvider::new(
            "slow",
            serde_json::json!({}),
            "after",
        )),
        Some("test".into()),
    );
    let mut agent = Agent::new(client, test_model());
    agent.add_tool(sleep_tool("slow", 500));

    let abort_handle = agent.abort_handle();
    let abort_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        abort_handle.abort();
    });

    let start = std::time::Instant::now();
    agent.prompt("Hi").await.unwrap();
    let elapsed = start.elapsed();
    abort_task.await.unwrap();

    assert!(
        elapsed < Duration::from_millis(300),
        "abort during tool execution took {elapsed:?}; expected < 300ms"
    );

    // Last message must be the synthesized aborted tool result; the run did
    // not wait for the natural 500ms tool completion.
    let last = agent.messages().last().unwrap();
    match last {
        Message::ToolResult(tr) => {
            assert!(tr.is_error, "aborted tool result must be flagged is_error");
            let body = tr
                .content
                .iter()
                .filter_map(|c| match c {
                    model::ToolResultContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                body.contains("aborted"),
                "expected tool-result body to mention abort, got: {body:?}"
            );
        }
        other => panic!("expected last message to be ToolResult, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Continue queue behavior
// ---------------------------------------------------------------------------

#[tokio::test]
async fn continue_drains_steering_when_last_is_assistant() {
    let mut agent = setup_text_agent("ok");
    // Seed transcript: user, assistant
    agent.replace_messages(vec![
        Message::User(UserMessage::new_text("hi")),
        Message::Assistant(test_assistant_message("hello")),
    ]);
    agent.steer(Message::User(UserMessage::new_text("steered")));

    agent.r#continue().await.unwrap();

    // Steering message should now be in the transcript followed by an assistant reply.
    let texts: Vec<String> = agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(format!("{:?}", u.content)),
            _ => None,
        })
        .collect();
    assert!(texts.iter().any(|t| t.contains("steered")));
}

#[tokio::test]
async fn continue_errors_when_no_messages() {
    let mut agent = Agent::new(Client::new(), test_model());
    let result = agent.r#continue().await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Queue management (sync)
// ---------------------------------------------------------------------------

#[test]
fn queues_can_be_steered_and_cleared() {
    let agent = Agent::new(Client::new(), test_model());
    assert!(!agent.has_queued_messages());
    agent.steer(Message::User(UserMessage::new_text("s1")));
    agent.follow_up(Message::User(UserMessage::new_text("f1")));
    assert!(agent.has_queued_messages());
    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

#[test]
fn reset_clears_state_and_queues() {
    let mut agent = Agent::new(Client::new(), test_model());
    agent.replace_messages(vec![Message::User(UserMessage::new_text("x"))]);
    agent.steer(Message::User(UserMessage::new_text("s")));
    agent.reset();
    assert!(agent.messages().is_empty());
    assert!(!agent.has_queued_messages());
    assert!(!agent.is_streaming());
}

// ---------------------------------------------------------------------------
// Tool result helpers
// ---------------------------------------------------------------------------

#[test]
fn tool_result_helpers() {
    let r = ToolResult::error("bad");
    assert_eq!(r.content.len(), 1);
    let r2 = ToolResult::text("ok").with_terminate(true);
    assert_eq!(r2.terminate, Some(true));
}

#[test]
fn agent_tool_to_model_tool() {
    let tool = echo_tool();
    let mt = tool.to_model_tool();
    assert_eq!(mt.name, "echo");
}

// ---------------------------------------------------------------------------
// Sanity: cancellation token snapshot
// ---------------------------------------------------------------------------

#[test]
fn cancellation_token_can_be_cloned() {
    let agent = Agent::new(Client::new(), test_model());
    let _token: CancellationToken = agent.cancellation_token();
}
