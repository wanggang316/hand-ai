//! Parity port of `pi-mono/packages/ai/test/faux-provider.test.ts`.
//!
//! Tests the contract of the Rust faux provider itself: every script-step
//! variant emits the right event, ordering is preserved, and terminal events
//! produce well-formed envelopes.

use futures::StreamExt;
use model::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, EventStream,
    FauxProvider, FauxScriptStep, StopReason, StreamOptions, Usage, api_registry::ApiProvider,
    faux_model, types::Provider,
};
use tokio_util::sync::CancellationToken;

fn ev_kind(ev: &AssistantMessageEvent) -> &'static str {
    match ev {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

async fn collect(
    provider: &FauxProvider,
    options: Option<StreamOptions>,
) -> Vec<AssistantMessageEvent> {
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let mut s = provider.stream(model, Context::default(), options);
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn text_delta_emits_start_text_start_delta_end_done() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hi".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let kinds: Vec<&'static str> = collect(&provider, None).await.iter().map(ev_kind).collect();
    assert_eq!(
        kinds,
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
}

#[tokio::test]
async fn thinking_delta_emits_thinking_envelope() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::ThinkingDelta("ponder".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let kinds: Vec<&'static str> = collect(&provider, None).await.iter().map(ev_kind).collect();
    assert_eq!(
        kinds,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "done"
        ]
    );
}

#[tokio::test]
async fn tool_call_emits_start_delta_end() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::ToolCall {
                id: "tc1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"text": "hi"}),
            },
            FauxScriptStep::Done(StopReason::ToolUse, Usage::default()),
        ],
    );
    let events = collect(&provider, None).await;
    let kinds: Vec<&'static str> = events.iter().map(ev_kind).collect();
    assert_eq!(
        kinds,
        vec![
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );

    // The toolcall_end event must carry the full argument payload.
    let end_event = events
        .iter()
        .find(|e| matches!(e, AssistantMessageEvent::ToolCallEnd { .. }))
        .unwrap();
    if let AssistantMessageEvent::ToolCallEnd { tool_call, .. } = end_event {
        assert_eq!(tool_call.id, "tc1");
        assert_eq!(tool_call.name, "echo");
        assert_eq!(tool_call.arguments, serde_json::json!({"text": "hi"}));
    }
}

#[tokio::test]
async fn mixed_script_preserves_block_ordering() {
    // thinking -> text -> tool call -> done
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::ThinkingDelta("hmm".to_string()),
            FauxScriptStep::TextDelta("ok".to_string()),
            FauxScriptStep::ToolCall {
                id: "tc1".to_string(),
                name: "noop".to_string(),
                arguments: serde_json::json!({}),
            },
            FauxScriptStep::Done(StopReason::ToolUse, Usage::default()),
        ],
    );
    let kinds: Vec<&'static str> = collect(&provider, None).await.iter().map(ev_kind).collect();
    assert_eq!(
        kinds,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
}

#[tokio::test]
async fn error_step_emits_terminal_error() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![FauxScriptStep::Error("boom".to_string())],
    );
    let events = collect(&provider, None).await;
    let kinds: Vec<&'static str> = events.iter().map(ev_kind).collect();
    assert_eq!(kinds, vec!["start", "error"]);
    if let AssistantMessageEvent::Error { reason, error } = events.last().unwrap() {
        assert_eq!(*reason, StopReason::Error);
        assert_eq!(error.error_message.as_deref(), Some("boom"));
    } else {
        panic!("expected error terminal event");
    }
}

#[tokio::test]
async fn done_step_emits_terminal_done_with_usage() {
    let usage = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total_tokens: 10,
        ..Default::default()
    };
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![FauxScriptStep::Done(StopReason::Stop, usage.clone())],
    );
    let events = collect(&provider, None).await;
    if let Some(AssistantMessageEvent::Done { reason, message }) = events.last() {
        assert_eq!(*reason, StopReason::Stop);
        assert_eq!(message.usage.input, 1);
        assert_eq!(message.usage.output, 2);
        assert_eq!(message.usage.cache_read, 3);
        assert_eq!(message.usage.cache_write, 4);
        assert_eq!(message.usage.total_tokens, 10);
    } else {
        panic!("expected done terminal");
    }
}

#[tokio::test]
async fn sleep_step_yields_control_and_can_be_cancelled() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("a".to_string()),
            FauxScriptStep::Sleep(2_000),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let token = CancellationToken::new();
    let options = StreamOptions {
        signal: Some(token.clone()),
        ..Default::default()
    };

    let stream_token = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        stream_token.cancel();
    });

    let events = collect(&provider, Some(options)).await;
    // Last event must be an Error envelope flagged Aborted.
    let last = events.last().expect("expected at least one event");
    match last {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
        }
        other => panic!("expected aborted error terminal, got {}", ev_kind(other)),
    }
}

#[tokio::test]
async fn start_with_partial_runs_mutator() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::StartWithPartial(Box::new(|m: &mut AssistantMessage| {
                m.response_id = Some("resp-123".to_string());
            })),
            FauxScriptStep::TextDelta("ok".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let events = collect(&provider, None).await;
    if let Some(AssistantMessageEvent::Start { partial }) = events.first() {
        assert_eq!(partial.response_id.as_deref(), Some("resp-123"));
    } else {
        panic!("expected start as first event");
    }
}

#[tokio::test]
async fn factory_runs_per_call() {
    use std::sync::{Arc, Mutex};

    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = counter.clone();
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, move || {
        *counter_clone.lock().unwrap() += 1;
        vec![FauxScriptStep::Done(StopReason::Stop, Usage::default())]
    });

    let _ = collect(&provider, None).await;
    let _ = collect(&provider, None).await;
    assert_eq!(*counter.lock().unwrap(), 2);
}

#[tokio::test]
async fn collect_to_message_returns_done_message_with_full_text() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hello".to_string()),
            FauxScriptStep::TextDelta(" world".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let raw = provider.stream(
        faux_model(Api::Faux, Provider::OpenAI, "faux-1"),
        Context::default(),
        None,
    );
    let msg = EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("done should produce Ok");
    match &msg.content[0] {
        AssistantContentBlock::Text(t) => assert_eq!(t.text, "hello world"),
        other => panic!("expected text content, got {other:?}"),
    }
}
