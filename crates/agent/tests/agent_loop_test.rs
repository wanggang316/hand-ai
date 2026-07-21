//! Integration tests for the low-level agent loop.

mod common;

use common::*;
use hand_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentTool, BeforeToolCallResult, CancellationToken,
    QueueDeliveryMode, ToolExecutionMode, ToolResult, run_agent_loop, run_agent_loop_continue,
};
use model::{Api, Client, Message, SimpleStreamOptions, ToolResultContent, UserMessage};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn setup_client_with_text(text: &str) -> Client {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockTextProvider::new(text)),
        Some("test".into()),
    );
    client
}

fn setup_client_with_tool(
    tool_name: &str,
    tool_args: serde_json::Value,
    final_text: &str,
) -> Client {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockToolProvider::new(tool_name, tool_args, final_text)),
        Some("test".into()),
    );
    client
}

fn setup_client_with_multi_tool(
    calls: Vec<(&'static str, &'static str, serde_json::Value)>,
    final_text: &str,
) -> Client {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockMultiToolProvider::new(calls, final_text)),
        Some("test".into()),
    );
    client
}

fn setup_client_with_error(error_msg: &str) -> Client {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockErrorProvider {
            error_message: error_msg.into(),
        }),
        Some("test".into()),
    );
    client
}

fn default_config() -> AgentLoopConfig {
    AgentLoopConfig::new(test_model(), SimpleStreamOptions::default())
}

// ---------------------------------------------------------------------------
// Single-turn happy paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_turn_text() {
    let client = setup_client_with_text("Hello!");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();

    let mut context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
    };

    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    let result = run_agent_loop(
        prompt,
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    assert!(!result.messages.is_empty());
    let kinds = event_kinds(&events.lock().unwrap());
    assert_eq!(kinds.first(), Some(&"agent_start"));
    assert_eq!(kinds.last(), Some(&"agent_end"));
}

#[tokio::test]
async fn single_turn_tool_call() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "test"}), "Done");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    assert!(
        evs.iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
    );
    assert!(evs.iter().any(|e| matches!(
        e,
        AgentEvent::ToolExecutionEnd {
            is_error: false,
            ..
        }
    )));
}

#[tokio::test]
async fn length_truncated_tool_calls_fail_without_execution() {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(MockTruncatedToolProvider::new(
            "echo",
            serde_json::json!({"message": "cut off"}),
            "Recovered",
        )),
        Some("test".into()),
    );

    let executed = Arc::new(Mutex::new(0u32));
    let executed_probe = executed.clone();
    let tool = AgentTool::simple(
        "echo",
        "Echoes back the input",
        serde_json::json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        }),
        "Echo",
        move |_id, _args| {
            let executed = executed_probe.clone();
            async move {
                *executed.lock().unwrap() += 1;
                ToolResult::text("ran")
            }
        },
    );

    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    let result = run_agent_loop(
        prompt,
        &mut context,
        &[tool],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    // The tool itself must never run for a Length-truncated message.
    assert_eq!(*executed.lock().unwrap(), 0);

    let evs = events.lock().unwrap();
    assert!(evs.iter().any(|e| matches!(
        e,
        AgentEvent::ToolExecutionEnd { is_error: true, .. }
    )));

    let tool_result = result
        .messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(tr) => Some(tr),
            _ => None,
        })
        .expect("truncated tool call produces an error tool result");
    assert!(tool_result.is_error);
    let text = tool_result
        .content
        .iter()
        .find_map(|c| match c {
            ToolResultContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    assert!(text.contains("output token limit"));

    // The loop keeps going so the model can re-issue the call.
    let final_assistant = result
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("loop continues after failing truncated calls");
    assert_eq!(final_assistant.stop_reason, model::StopReason::Stop);
}

#[tokio::test]
async fn loop_continues_until_assistant_stops() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "hi"}), "All done");
    let (emit, _events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Go"))];

    let result = run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    // user, assistant (tool call), tool result, assistant (text)
    assert!(result.messages.len() >= 4);
}

#[tokio::test]
async fn error_response_emits_terminal_events() {
    let client = setup_client_with_error("API rate limit exceeded");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];

    run_agent_loop(
        prompt,
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
    // error message lands as a final assistant message in the transcript
    let final_assistant_has_error = context.messages.iter().rev().find_map(|m| match m {
        Message::Assistant(a) => Some(a.error_message.is_some()),
        _ => None,
    });
    assert_eq!(final_assistant_has_error, Some(true));
}

// ---------------------------------------------------------------------------
// Continue validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn continue_with_empty_context_errors() {
    let client = setup_client_with_text("ok");
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let result = run_agent_loop_continue(
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn continue_from_assistant_errors() {
    let client = setup_client_with_text("ok");
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![Message::Assistant(test_assistant_message("hi"))],
    };

    let result = run_agent_loop_continue(
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Tool not found / before-hook block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_not_found_emits_error_result() {
    let client = setup_client_with_tool("nonexistent_tool", serde_json::json!({}), "done");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("call tool"))];

    run_agent_loop(
        prompt,
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let has_error = evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { is_error: true, .. }));
    assert!(has_error);
}

#[tokio::test]
async fn before_tool_call_blocks() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "test"}), "Done");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let mut config = default_config();
    config.before_tool_call = Some(Arc::new(|_ctx, _cancel| {
        Box::pin(async {
            Some(BeforeToolCallResult {
                block: true,
                reason: Some("Blocked by test".into()),
            })
        })
    }));

    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let has_blocked = evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { is_error: true, .. }));
    assert!(has_blocked);
}

// ---------------------------------------------------------------------------
// Steering / follow-up
// ---------------------------------------------------------------------------

#[tokio::test]
async fn steering_hook_polled() {
    let client = setup_client_with_text("ok");
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = counter.clone();

    let mut config = default_config();
    config.get_steering_messages = Some(Arc::new(move || {
        let cc = counter_clone.clone();
        Box::pin(async move {
            *cc.lock().unwrap() += 1;
            vec![]
        })
    }));

    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    run_agent_loop(prompt, &mut context, &[], &config, &client, &emit, &cancel)
        .await
        .unwrap();

    assert!(*counter.lock().unwrap() >= 1);
}

#[tokio::test]
async fn follow_up_extends_run() {
    let client = setup_client_with_text("ok");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let calls = Arc::new(Mutex::new(0u32));
    let calls_clone = calls.clone();
    let mut config = default_config();
    config.get_follow_up_messages = Some(Arc::new(move || {
        let cc = calls_clone.clone();
        Box::pin(async move {
            let mut c = cc.lock().unwrap();
            *c += 1;
            if *c == 1 {
                vec![Message::User(UserMessage::new_text("follow"))]
            } else {
                vec![]
            }
        })
    }));

    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    run_agent_loop(prompt, &mut context, &[], &config, &client, &emit, &cancel)
        .await
        .unwrap();

    let evs = events.lock().unwrap();
    let turn_starts = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStart))
        .count();
    // Initial turn + follow-up turn
    assert!(turn_starts >= 2);
}

// ---------------------------------------------------------------------------
// Real parallelism
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parallel_tools_overlap_in_wall_clock() {
    let client = setup_client_with_multi_tool(
        vec![
            ("slow_a", "id_a", serde_json::json!({})),
            ("slow_b", "id_b", serde_json::json!({})),
        ],
        "done",
    );
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![sleep_tool("slow_a", 60), sleep_tool("slow_b", 60)];

    let mut config = default_config();
    config.tool_execution = ToolExecutionMode::Parallel;

    let prompt = vec![Message::User(UserMessage::new_text("go"))];

    let start = Instant::now();
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();

    // Two 60ms sleeps overlapping should finish in ~60ms; allow generous margin.
    assert!(
        elapsed < Duration::from_millis(110),
        "parallel tools took {elapsed:?}; expected < 110ms"
    );
}

#[tokio::test]
async fn parallel_tool_end_arrives_in_completion_order() {
    // Source order: slow_a (80ms), then slow_b (10ms).
    // Expected event order:
    //   - tool_execution_end for slow_b first (completes first)
    //   - tool_execution_end for slow_a second
    //   - message_start{ToolResult, slow_a} before message_start{ToolResult, slow_b}
    //     (tool-result messages emit in source order to satisfy the LLM contract)
    let client = setup_client_with_multi_tool(
        vec![
            ("slow_a", "id_a", serde_json::json!({})),
            ("slow_b", "id_b", serde_json::json!({})),
        ],
        "done",
    );
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![sleep_tool("slow_a", 80), sleep_tool("slow_b", 10)];

    let mut config = default_config();
    config.tool_execution = ToolExecutionMode::Parallel;

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let end_order: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_name, .. } => Some(tool_name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        end_order,
        vec!["slow_b", "slow_a"],
        "tool_execution_end must arrive in completion order"
    );

    let result_msg_order: Vec<String> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart {
                message: Message::ToolResult(tr),
            } => Some(tr.tool_name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        result_msg_order,
        vec!["slow_a".to_string(), "slow_b".to_string()],
        "tool-result messages must emit in source order"
    );
}

#[tokio::test]
async fn sequential_tools_run_in_series() {
    let client = setup_client_with_multi_tool(
        vec![
            ("slow_a", "id_a", serde_json::json!({})),
            ("slow_b", "id_b", serde_json::json!({})),
        ],
        "done",
    );
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![sleep_tool("slow_a", 50), sleep_tool("slow_b", 50)];

    let mut config = default_config();
    config.tool_execution = ToolExecutionMode::Sequential;
    let prompt = vec![Message::User(UserMessage::new_text("go"))];

    let start = Instant::now();
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();

    // Sequential 2x50ms ≥ 100ms. Allow up to 250ms for CI slack.
    assert!(
        elapsed >= Duration::from_millis(95),
        "sequential tools took {elapsed:?}; expected ≥ 95ms"
    );
}

#[tokio::test]
async fn per_tool_sequential_downgrades_batch() {
    let client = setup_client_with_multi_tool(
        vec![
            ("slow_a", "id_a", serde_json::json!({})),
            ("slow_b", "id_b", serde_json::json!({})),
        ],
        "done",
    );
    let (emit, _) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    // slow_a marked sequential — whole batch should run sequentially.
    let tools = vec![
        sleep_tool("slow_a", 40).with_execution_mode(ToolExecutionMode::Sequential),
        sleep_tool("slow_b", 40),
    ];

    let mut config = default_config();
    config.tool_execution = ToolExecutionMode::Parallel;
    let prompt = vec![Message::User(UserMessage::new_text("go"))];

    let start = Instant::now();
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(75),
        "downgraded batch took {elapsed:?}; expected ≥ 75ms (sequential)"
    );
}

// ---------------------------------------------------------------------------
// Terminate / shouldStopAfterTurn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_tools_terminate_breaks_loop() {
    let client = setup_client_with_multi_tool(
        vec![
            ("term_a", "id_a", serde_json::json!({})),
            ("term_b", "id_b", serde_json::json!({})),
        ],
        "done",
    );
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![terminate_tool("term_a"), terminate_tool("term_b")];

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    // The "done" assistant turn should NOT have been requested, because all
    // tools terminated. So we should see exactly ONE assistant message in the
    // event stream (the tool-call message), no second assistant message.
    let evs = events.lock().unwrap();
    let assistant_message_ends = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(_)
                }
            )
        })
        .count();
    assert_eq!(
        assistant_message_ends, 1,
        "after terminate, expected only the tool-call assistant message; got {assistant_message_ends}"
    );
}

#[tokio::test]
async fn partial_terminate_does_not_break_loop() {
    // First turn issues two tool calls — one terminate, one regular echo.
    // The loop must continue (not all results have terminate=true), produce
    // a second assistant turn, and finalize with a follow-up text response.
    let client = setup_client_with_multi_tool(
        vec![
            ("term_a", "id_a", serde_json::json!({})),
            ("echo", "id_b", serde_json::json!({"message": "hi"})),
        ],
        "after",
    );
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![
        terminate_tool("term_a"),
        echo_tool().with_execution_mode(ToolExecutionMode::Parallel),
    ];

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();

    // Both tools must have run: one terminate event each.
    let tool_ends: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                tool_name, result, ..
            } => Some((tool_name.clone(), result.terminate)),
            _ => None,
        })
        .collect();
    assert_eq!(tool_ends.len(), 2, "expected two tool_execution_end events");
    let terminate_count = tool_ends.iter().filter(|(_, t)| *t == Some(true)).count();
    assert_eq!(
        terminate_count, 1,
        "exactly one tool result should have terminate=true"
    );

    // The loop must have produced two assistant messages: the tool-call turn
    // and the post-tool follow-up turn.
    let assistant_ends = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(_)
                }
            )
        })
        .count();
    assert_eq!(
        assistant_ends, 2,
        "partial-terminate should not break the loop; expected 2 assistant message_ends, got {assistant_ends}"
    );
}

#[tokio::test]
async fn should_stop_after_turn_exits_after_turn_end() {
    let client = setup_client_with_text("Hello!");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();

    let mut config = default_config();
    config.should_stop_after_turn = Some(Arc::new(|_ctx, _cancel| Box::pin(async { true })));

    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    run_agent_loop(prompt, &mut context, &[], &config, &client, &emit, &cancel)
        .await
        .unwrap();

    let evs = events.lock().unwrap();
    let kinds: Vec<&str> = event_kinds(&evs);
    let last_two = &kinds[kinds.len() - 2..];
    assert_eq!(last_two, &["turn_end", "agent_end"]);
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancellation_during_stream_emits_aborted() {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(SlowTextProvider {
            delay_ms: 500,
            response_text: "would never arrive".into(),
        }),
        Some("test".into()),
    );

    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];

    let cancel_clone = cancel.clone();
    let cancel_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel_clone.cancel();
    });

    let start = Instant::now();
    run_agent_loop(
        prompt,
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    cancel_handle.await.unwrap();

    // We bailed out long before the 500ms provider delay
    assert!(
        elapsed < Duration::from_millis(300),
        "cancellation took {elapsed:?}; expected < 300ms"
    );

    let evs = events.lock().unwrap();
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
    let last_assistant_aborted = context.messages.iter().rev().find_map(|m| match m {
        Message::Assistant(a) => Some(a.stop_reason == model::StopReason::Aborted),
        _ => None,
    });
    assert_eq!(last_assistant_aborted, Some(true));
}

#[tokio::test]
async fn pre_stream_cancel_emits_message_start_and_end() {
    // Cancelling before `client.stream_simple` is invoked still has to emit
    // MessageStart / MessageEnd for the synthesized aborted message so that
    // every transcript-tracked assistant message also appears in the event
    // stream (UIs / persisters depend on this invariant).
    let client = setup_client_with_text("never reached");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    cancel.cancel(); // cancel up front

    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    run_agent_loop(
        prompt,
        &mut context,
        &[],
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let assistant_starts = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageStart {
                    message: Message::Assistant(_)
                }
            )
        })
        .count();
    let assistant_ends = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(_)
                }
            )
        })
        .count();
    assert_eq!(
        assistant_starts, 1,
        "expected exactly one MessageStart for the synthesized aborted assistant message"
    );
    assert_eq!(
        assistant_ends, 1,
        "expected exactly one MessageEnd for the synthesized aborted assistant message"
    );

    let last_assistant_aborted = context.messages.iter().rev().find_map(|m| match m {
        Message::Assistant(a) => Some(a.stop_reason == model::StopReason::Aborted),
        _ => None,
    });
    assert_eq!(last_assistant_aborted, Some(true));
}

// ---------------------------------------------------------------------------
// JSON Schema validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_validation_rejects_bad_args() {
    let client = setup_client_with_tool(
        "echo",
        serde_json::json!({"message": 42}), // wrong type
        "done",
    );
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![echo_tool()];

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let invalid = evs.iter().any(|e| match e {
        AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        } => {
            *is_error
                && matches!(&result.content[..], [model::ToolResultContent::Text(t)]
                    if t.text.contains("Invalid arguments"))
        }
        _ => false,
    });
    assert!(invalid, "expected schema validation error");
}

// ---------------------------------------------------------------------------
// Tool execution updates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_execution_updates_propagate() {
    use hand_agent::ToolExecuteCtx;
    let updating_tool = hand_agent::AgentTool::new(
        "updater",
        "Streams partial updates",
        serde_json::json!({ "type": "object", "properties": {} }),
        "Updater",
        Box::new(|ctx: ToolExecuteCtx| {
            Box::pin(async move {
                (ctx.on_update)(ToolResult::text("partial 1"));
                (ctx.on_update)(ToolResult::text("partial 2"));
                Ok(ToolResult::text("final"))
            })
        }),
    );

    let client = setup_client_with_tool("updater", serde_json::json!({}), "done");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![updating_tool];

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &default_config(),
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let updates: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
        .collect();
    assert_eq!(updates.len(), 2);
}

// ---------------------------------------------------------------------------
// AfterToolCall override
// ---------------------------------------------------------------------------

#[tokio::test]
async fn after_tool_call_overrides_result() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "hi"}), "done");
    let (emit, events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let tools = vec![echo_tool()];

    let mut config = default_config();
    config.after_tool_call = Some(Arc::new(|_ctx, _cancel| {
        Box::pin(async {
            Some(hand_agent::AfterToolCallResult {
                content: Some(vec![model::ToolResultContent::Text(
                    model::TextContent::new("OVERRIDDEN"),
                )]),
                ..Default::default()
            })
        })
    }));

    let prompt = vec![Message::User(UserMessage::new_text("go"))];
    run_agent_loop(
        prompt,
        &mut context,
        &tools,
        &config,
        &client,
        &emit,
        &cancel,
    )
    .await
    .unwrap();

    let evs = events.lock().unwrap();
    let saw_override = evs.iter().any(|e| match e {
        AgentEvent::ToolExecutionEnd { result, .. } => {
            matches!(&result.content[..], [model::ToolResultContent::Text(t)] if t.text == "OVERRIDDEN")
        }
        _ => false,
    });
    assert!(saw_override);
}

// ---------------------------------------------------------------------------
// Queue delivery default
// ---------------------------------------------------------------------------

#[test]
fn queue_delivery_mode_default_is_one_at_a_time() {
    assert_eq!(QueueDeliveryMode::default(), QueueDeliveryMode::OneAtATime);
}

// ---------------------------------------------------------------------------
// stream_fn injection
// ---------------------------------------------------------------------------

/// Asserts that when `AgentLoopConfig::stream_fn` is set, the loop drives the
/// custom transport instead of `client.stream_simple`. The client is wired to
/// a `PanickingProvider`, so any fallthrough to the default branch would
/// panic; the injected closure flips an atomic flag we can read back.
#[tokio::test]
async fn stream_fn_injection_bypasses_client_stream_simple() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();

    let stream_fn = Arc::new(
        move |_model: &model::Model,
              _ctx: model::Context,
              _opts: model::SimpleStreamOptions,
              _cancel: tokio_util::sync::CancellationToken|
              -> model::AssistantMessageEventStream<'static> {
            called_clone.store(true, Ordering::SeqCst);
            let msg = test_assistant_message("from-injected-stream");
            Box::pin(async_stream::stream! {
                yield model::AssistantMessageEvent::Start { partial: msg.clone() };
                yield model::AssistantMessageEvent::Done {
                    reason: model::StopReason::Stop,
                    message: msg,
                };
            })
        },
    );

    let client = Client::new();
    // If the loop ever falls through to `client.stream_simple`, this provider
    // panics, failing the test.
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(PanickingProvider),
        Some("test".into()),
    );

    let mut config = default_config();
    config.stream_fn = Some(stream_fn);

    let (emit, _events) = collecting_event_sink();
    let cancel = CancellationToken::new();
    let mut context = AgentContext::default();
    let prompt = vec![Message::User(UserMessage::new_text("hi"))];

    let result = run_agent_loop(prompt, &mut context, &[], &config, &client, &emit, &cancel)
        .await
        .expect("loop completes via injected stream_fn");

    assert!(
        called.load(Ordering::SeqCst),
        "injected stream_fn must have been invoked"
    );

    // The transcript's last message should be the assistant produced by the
    // injected stream.
    let last = context
        .messages
        .last()
        .expect("at least one message in transcript");
    match last {
        Message::Assistant(am) => {
            let text = am
                .content
                .iter()
                .find_map(|b| match b {
                    model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            assert_eq!(text, "from-injected-stream");
        }
        other => panic!("expected last message to be Assistant, got {other:?}"),
    }

    // Sanity: the loop returned the same final assistant message.
    let final_assistant = result
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("loop result contains an assistant message");
    assert_eq!(final_assistant.stop_reason, model::StopReason::Stop);
}
