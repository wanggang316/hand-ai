//! Orphan tool-call coverage.
//!
//! When an assistant emits a tool call that never receives a `ToolResult`,
//! the user follows up with a fresh prompt, and we replay the conversation
//! against a target model, `transform_messages` must synthesize a synthetic
//! "no result provided" tool result so the request is well-formed.
//!
//! We exercise this via the faux provider for the first turn (it emits a tool
//! call), then run the captured assistant message through `transform_messages`.

use model::{
    Api, AssistantContentBlock, Context, EventStream, FauxProvider, FauxScriptStep, StopReason,
    ToolCall, Usage, UserMessage,
    api_registry::ApiProvider,
    faux_model,
    transform::transform_messages,
    types::{Message, Provider},
};

#[tokio::test]
async fn orphan_tool_call_gets_synthetic_result_on_replay() {
    let tool_args = serde_json::json!({"expression": "25 * 18"});
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::ToolCall {
                id: "call_1".to_string(),
                name: "calculate".to_string(),
                arguments: tool_args.clone(),
            },
            FauxScriptStep::Done(StopReason::ToolUse, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("calculate 25 * 18"))],
        tools: None,
    };

    // Drive the faux turn and capture the assistant message.
    let raw = provider.stream(model.clone(), context.clone(), None);
    let assistant_msg = EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("done should produce Ok");

    // Verify it actually contains a tool call.
    assert_eq!(assistant_msg.content.len(), 1);
    let captured_tc: ToolCall = match &assistant_msg.content[0] {
        AssistantContentBlock::ToolCall(tc) => tc.clone(),
        other => panic!("expected tool call, got {other:?}"),
    };
    assert_eq!(captured_tc.id, "call_1");

    // Build a follow-up conversation: user -> assistant(toolcall) -> user
    // (without a ToolResult in between). The user wants to abandon the tool
    // call.
    let mut messages = context.messages.clone();
    messages.push(Message::Assistant(assistant_msg));
    messages.push(Message::User(UserMessage::new_text(
        "Never mind, what is 2+2?",
    )));

    let transformed = transform_messages(&messages, &model, None);

    // The transform should slot in a synthetic tool result between the
    // assistant tool call and the new user turn so the next request is
    // well-formed.
    let kinds: Vec<&'static str> = transformed
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "toolResult", "user"],
        "expected synthetic tool result between orphan tool call and user follow-up",
    );

    // The synthesized tool result references the orphan tool call and is
    // marked as an error.
    if let Message::ToolResult(tr) = &transformed[2] {
        assert_eq!(tr.tool_call_id, "call_1");
        assert_eq!(tr.tool_name, "calculate");
        assert!(tr.is_error);
    } else {
        panic!("expected ToolResult at position 2");
    }
}
