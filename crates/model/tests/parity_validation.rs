//! Structural-validator coverage for `validate_context`.
//!
//! Covers the structural validator exposed via `validate_context`,
//! hitting every
//! `ValidationIssueKind` with a faux-generated message graph.
//!
//! NOTE: TS reference also exercises Ajv-style tool-argument schema validation
//! via `validateToolArguments`. M2's `validate_context` covers structural
//! checks only; Ajv-style argument coercion is deferred to a future milestone
//! (track via a follow-up when JSON-schema validation is added to the model
//! crate).
//!
//! TODO(M11): port the Ajv coercion cases from `validation.test.ts` once the
//! crate gains JSON-schema-based tool argument validation.

use model::{
    Api, AssistantContentBlock, AssistantMessage, Context, EventStream, FauxProvider,
    FauxScriptStep, ImageContent, StopReason, TextContent, Tool, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, UserContent, UserContentBlock, UserMessage, ValidationIssueKind,
    api_registry::ApiProvider,
    faux_model,
    types::{Message, Provider},
    validate_context,
};

async fn faux_assistant_with_tool_call() -> AssistantMessage {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::ToolCall {
                id: "call_orphan".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            FauxScriptStep::Done(StopReason::ToolUse, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let raw = provider.stream(model, Context::default(), None);
    EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("faux done should produce Ok")
}

#[tokio::test]
async fn validate_context_reports_orphan_tool_call() {
    let assistant = faux_assistant_with_tool_call().await;
    let ctx = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage::new_text("hi")),
            Message::Assistant(assistant),
        ],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::OrphanToolCall),
        "expected OrphanToolCall in {issues:?}"
    );
}

#[tokio::test]
async fn validate_context_reports_empty_content() {
    // Empty user content + empty assistant content (faux-generated, with the
    // `Done` step never emitting any block) both trigger EmptyContent.
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![FauxScriptStep::Done(StopReason::Stop, Usage::default())],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let raw = provider.stream(model, Context::default(), None);
    let empty_assistant = EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("done should produce Ok");
    assert!(empty_assistant.content.is_empty());

    let empty_user = UserMessage {
        role: "user".to_string(),
        content: UserContent::Blocks(vec![]),
        timestamp: 0,
    };
    let ctx = Context {
        system_prompt: None,
        messages: vec![
            Message::User(empty_user),
            Message::Assistant(empty_assistant),
        ],
        tools: None,
    };
    let issues = validate_context(&ctx);
    let empties: Vec<_> = issues
        .iter()
        .filter(|i| i.kind == ValidationIssueKind::EmptyContent)
        .collect();
    assert_eq!(
        empties.len(),
        2,
        "expected user + assistant empty: {issues:?}"
    );
}

#[test]
fn validate_context_reports_invalid_image() {
    let user = UserMessage {
        role: "user".to_string(),
        content: UserContent::Blocks(vec![UserContentBlock::Image(ImageContent {
            content_type: "image".to_string(),
            data: String::new(),
            mime_type: String::new(),
        })]),
        timestamp: 0,
    };
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::User(user)],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::InvalidImage),
        "{issues:?}"
    );
}

#[test]
fn validate_context_reports_empty_and_duplicate_tool_names() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![],
        tools: Some(vec![
            Tool::new("", "empty", serde_json::json!({})),
            Tool::new("dup", "first", serde_json::json!({})),
            Tool::new("dup", "second", serde_json::json!({})),
        ]),
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::EmptyToolName)
    );
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::DuplicateToolName)
    );
}

#[test]
fn validate_context_reports_orphan_tool_result() {
    let ctx = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage::new_text("hi")),
            Message::ToolResult(ToolResultMessage::new(
                "no-such-call",
                "echo",
                vec![ToolResultContent::Text(TextContent::new("oops"))],
            )),
        ],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::OrphanToolResult)
    );
}

#[test]
fn validate_context_reports_missing_assistant_between_tool_result_and_user() {
    let assistant = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "tc1",
            "echo",
            serde_json::json!({}),
        ))],
        api: Api::Faux,
        provider: Provider::OpenAI,
        model: "faux-1".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    };

    let ctx = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage::new_text("call the tool")),
            Message::Assistant(assistant),
            Message::ToolResult(ToolResultMessage::new(
                "tc1",
                "echo",
                vec![ToolResultContent::Text(TextContent::new("ok"))],
            )),
            Message::User(UserMessage::new_text("now what?")),
        ],
        tools: None,
    };
    let issues = validate_context(&ctx);
    assert!(
        issues
            .iter()
            .any(|i| i.kind == ValidationIssueKind::MissingAssistantBetweenToolResultAndUser),
        "{issues:?}"
    );
}
