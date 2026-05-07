//! Integration tests for the agent loop.

mod common;

use common::*;
use hand_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, ToolExecutionMode, ToolResult, run_agent_loop,
    run_agent_loop_continue,
};
use model::{
    Api, AssistantContentBlock, Client, Message, SimpleStreamOptions, StopReason, UserMessage,
};
use std::sync::{Arc, Mutex};

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
    AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Parallel,
        before_tool_call: None,
        after_tool_call: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    }
}

// ========================================================================
// A-010: Agent loop single turn text
// ========================================================================
#[tokio::test]
async fn test_agent_loop_single_turn_text() {
    let client = setup_client_with_text("Hello!");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![],
    };

    let config = default_config();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];

    let result = run_agent_loop(prompt, &mut context, &[], &config, &client, &emit).await;
    assert!(result.is_ok());

    let result = result.unwrap();
    assert!(!result.messages.is_empty());

    // Verify events
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentStart)));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnStart)));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentEnd { .. }))
    );
}

// ========================================================================
// A-011: Agent loop single turn tool call
// ========================================================================
#[tokio::test]
async fn test_agent_loop_single_turn_tool_call() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "test"}), "Done");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();
    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    let result = run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit).await;
    assert!(result.is_ok());

    let events = events.lock().unwrap();
    // Should have tool execution events
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
    );
}

// ========================================================================
// A-012: Agent loop continue until stop
// ========================================================================
#[tokio::test]
async fn test_agent_loop_continue_until_stop() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "hi"}), "All done");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();
    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Go"))];

    let result = run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit).await;
    assert!(result.is_ok());

    // The loop should have run tool call → tool result → final text response
    let result = result.unwrap();
    // At least: user msg, assistant (tool call), tool result, assistant (text)
    assert!(result.messages.len() >= 3);
}

// ========================================================================
// A-013: Agent loop with error response
// ========================================================================
#[tokio::test]
async fn test_agent_loop_error_response() {
    let client = setup_client_with_error("API rate limit exceeded");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];

    let result = run_agent_loop(prompt, &mut context, &[], &config, &client, &emit).await;
    assert!(result.is_ok()); // Loop itself succeeds, but emits error event

    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentEnd { .. }))
    );
}

// ========================================================================
// A-014: Agent loop continue validation
// ========================================================================
#[tokio::test]
async fn test_agent_loop_continue_empty_context() {
    let client = setup_client_with_text("ok");
    let (emit, _) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();

    let result = run_agent_loop_continue(&mut context, &[], &config, &client, &emit).await;
    assert!(result.is_err());
}

// ========================================================================
// A-015: Agent loop continue from assistant message fails
// ========================================================================
#[tokio::test]
async fn test_agent_loop_continue_from_assistant_fails() {
    let client = setup_client_with_text("ok");
    let (emit, _) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![Message::Assistant(test_assistant_message("hi"))],
    };

    let config = default_config();

    let result = run_agent_loop_continue(&mut context, &[], &config, &client, &emit).await;
    assert!(result.is_err());
}

// ========================================================================
// A-016: Event order
// ========================================================================
#[tokio::test]
async fn test_event_order_basic() {
    let client = setup_client_with_text("Hello!");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();
    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];

    run_agent_loop(prompt, &mut context, &[], &config, &client, &emit)
        .await
        .unwrap();

    let events = events.lock().unwrap();

    // First event should be AgentStart
    assert!(matches!(events[0], AgentEvent::AgentStart));
    // Second should be TurnStart
    assert!(matches!(events[1], AgentEvent::TurnStart));
    // Last should be AgentEnd
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::AgentEnd { .. }
    ));
}

// ========================================================================
// A-017: Tool not found emits error result
// ========================================================================
#[tokio::test]
async fn test_tool_not_found_emits_error_result() {
    let client = setup_client_with_tool("nonexistent_tool", serde_json::json!({}), "done");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = default_config();
    // No tools registered!
    let prompt = vec![Message::User(UserMessage::new_text("call tool"))];

    let result = run_agent_loop(prompt, &mut context, &[], &config, &client, &emit).await;
    assert!(result.is_ok());

    let events = events.lock().unwrap();
    // Should have a ToolExecutionEnd with is_error=true
    let has_error_tool = events.iter().any(|e| match e {
        AgentEvent::ToolExecutionEnd { is_error, .. } => *is_error,
        _ => false,
    });
    assert!(has_error_tool);
}

// ========================================================================
// A-018: Steering messages injected
// ========================================================================
#[tokio::test]
async fn test_steering_messages_injected() {
    let client = setup_client_with_text("ok");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let call_count = Arc::new(Mutex::new(0u32));
    let call_count_clone = call_count.clone();

    let config = AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Parallel,
        before_tool_call: None,
        after_tool_call: None,
        get_steering_messages: Some(Box::new(move || {
            let cc = call_count_clone.clone();
            Box::pin(async move {
                let mut count = cc.lock().unwrap();
                *count += 1;
                // Return empty — steering is checked but we don't inject
                vec![]
            })
        })),
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    };

    let prompt = vec![Message::User(UserMessage::new_text("Hi"))];
    run_agent_loop(prompt, &mut context, &[], &config, &client, &emit)
        .await
        .unwrap();

    // Steering hook should have been called at least once
    let count = *call_count.lock().unwrap();
    assert!(count >= 1);
}

// ========================================================================
// A-019: Sequential tool execution
// ========================================================================
#[tokio::test]
async fn test_sequential_tool_execution() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "test"}), "Done");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Sequential,
        before_tool_call: None,
        after_tool_call: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    };

    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    let result = run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit).await;
    assert!(result.is_ok());

    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
    );
}

// ========================================================================
// A-020: Before tool call hook
// ========================================================================
#[tokio::test]
async fn test_before_tool_call_hook_blocks() {
    let client = setup_client_with_tool("echo", serde_json::json!({"message": "test"}), "Done");
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    let config = AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Sequential,
        before_tool_call: Some(Box::new(|_ctx| {
            Box::pin(async {
                Some(hand_agent::BeforeToolCallResult {
                    block: true,
                    reason: Some("Blocked by test".into()),
                    replace_args: None,
                })
            })
        })),
        after_tool_call: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    };

    let tools = vec![echo_tool()];
    let prompt = vec![Message::User(UserMessage::new_text("Use echo"))];

    let result = run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit).await;
    assert!(result.is_ok());

    let events = events.lock().unwrap();
    // Should have a ToolExecutionEnd with is_error=true
    let has_blocked = events.iter().any(|e| match e {
        AgentEvent::ToolExecutionEnd { is_error, .. } => *is_error,
        _ => false,
    });
    assert!(has_blocked);
}

// ========================================================================
// F23 — replace_args from before-hook is forwarded to the tool execute fn.
// ========================================================================
#[tokio::test]
async fn replace_args_used_when_set() {
    use hand_agent::{BoxFuture, ToolExecutionContext};

    let client = setup_client_with_tool(
        "recorder",
        serde_json::json!({"original": true}),
        "Done",
    );
    let (emit, _events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    // The recorder tool captures the args it actually receives so the test
    // can assert that the rewritten args reached the closure.
    let recorded: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let recorded_for_tool = recorded.clone();

    let recorder_tool = hand_agent::AgentTool::new(
        "recorder",
        "Records args",
        serde_json::json!({"type": "object"}),
        "Recorder",
        Box::new(move |_id, args, _cx: ToolExecutionContext| -> BoxFuture<'static, ToolResult> {
            let recorded = recorded_for_tool.clone();
            Box::pin(async move {
                *recorded.lock().unwrap() = Some(args);
                ToolResult::text("recorded")
            })
        }),
    );

    let config = AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Sequential,
        before_tool_call: Some(Box::new(|_ctx| {
            Box::pin(async {
                Some(hand_agent::BeforeToolCallResult {
                    block: false,
                    reason: None,
                    replace_args: Some(serde_json::json!({"replaced": true})),
                })
            })
        })),
        after_tool_call: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    };

    let tools = vec![recorder_tool];
    let prompt = vec![Message::User(UserMessage::new_text("rewrite me"))];
    run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit)
        .await
        .expect("agent loop ok");

    let captured = recorded.lock().unwrap().clone();
    assert_eq!(
        captured,
        Some(serde_json::json!({"replaced": true})),
        "tool must observe the replace_args, not the model's original args"
    );
}

// ========================================================================
// F23 — ToolResult::is_error round-trips into the model-visible message.
// ========================================================================
#[tokio::test]
async fn is_error_flag_round_trips() {
    use hand_agent::{BoxFuture, ToolExecutionContext};

    let client = setup_client_with_tool(
        "fails",
        serde_json::json!({}),
        "Done",
    );
    let (emit, events) = collecting_event_sink();

    let mut context = AgentContext {
        system_prompt: String::new(),
        messages: vec![],
    };

    // Tool that returns `is_error: true` via `ToolResult::error`.
    let failing_tool = hand_agent::AgentTool::new(
        "fails",
        "Always fails",
        serde_json::json!({"type": "object"}),
        "Fails",
        Box::new(|_id, _args, _cx: ToolExecutionContext| -> BoxFuture<'static, ToolResult> {
            Box::pin(async move { ToolResult::error("kaboom") })
        }),
    );

    let config = AgentLoopConfig {
        model: test_model(),
        stream_options: SimpleStreamOptions::default(),
        cwd: std::path::PathBuf::new(),
        session_id: String::new(),
        tool_execution: ToolExecutionMode::Sequential,
        before_tool_call: None,
        after_tool_call: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        convert_to_llm: None,
        transform_context: None,
        get_api_key: None,
        steering_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        follow_up_mode: hand_agent::QueueDeliveryMode::OneAtATime,
        max_retry_delay_ms: None,
    };

    let tools = vec![failing_tool];
    let prompt = vec![Message::User(UserMessage::new_text("call fails"))];
    let result = run_agent_loop(prompt, &mut context, &tools, &config, &client, &emit)
        .await
        .expect("agent loop ok");

    // The ToolExecutionEnd event must report is_error=true.
    let events = events.lock().unwrap();
    let saw_error_event = events.iter().any(|e| match e {
        AgentEvent::ToolExecutionEnd { is_error, result, .. } => {
            *is_error && result.is_error
        }
        _ => false,
    });
    assert!(saw_error_event, "ToolExecutionEnd must carry is_error=true");

    // The tool_result message inserted into the conversation must also
    // carry is_error=true so the model sees the failure.
    let saw_error_message = result.messages.iter().any(|m| match m {
        Message::ToolResult(trm) => trm.is_error && trm.tool_name == "fails",
        _ => false,
    });
    assert!(
        saw_error_message,
        "tool_result message must carry is_error=true"
    );
}
