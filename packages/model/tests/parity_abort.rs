//! Parity port of `pi-mono/packages/ai/test/abort.test.ts`.
//!
//! TS exercises real providers; we exercise the faux provider with a long
//! `Sleep` step that gets cancelled mid-flight via `CancellationToken`. The
//! asserted invariant: cancellation results in a terminal `Error` event with
//! `StopReason::Aborted` and the stream does not panic.

use futures::StreamExt;
use model::{
    Api, AssistantMessageEvent, Context, FauxProvider, FauxScriptStep, StopReason, StreamOptions,
    Usage, api_registry::ApiProvider, faux_model, types::Provider,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn cancellation_during_sleep_emits_aborted_error() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hello".to_string()),
            FauxScriptStep::Sleep(10_000),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let context = Context::default();
    let token = CancellationToken::new();
    let options = StreamOptions {
        signal: Some(token.clone()),
        ..Default::default()
    };

    let stream_token = token.clone();
    tokio::spawn(async move {
        // Cancel after a brief moment — long enough for `start` and the
        // first text_delta to flow, short enough to interrupt the sleep.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        stream_token.cancel();
    });

    let mut stream = provider.stream(model, context, Some(options));
    let mut terminal_reason: Option<StopReason> = None;
    while let Some(ev) = stream.next().await {
        if let AssistantMessageEvent::Error { reason, error } = ev {
            terminal_reason = Some(reason);
            assert_eq!(error.stop_reason, StopReason::Aborted);
            assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
        }
    }

    assert_eq!(terminal_reason, Some(StopReason::Aborted));
}

#[tokio::test]
async fn pre_cancelled_token_emits_aborted_error_immediately() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hello".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let context = Context::default();
    let token = CancellationToken::new();
    token.cancel();
    let options = StreamOptions {
        signal: Some(token),
        ..Default::default()
    };

    let mut stream = provider.stream(model, context, Some(options));
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // The only event should be the aborted error envelope.
    assert_eq!(events.len(), 1);
    match &events[0] {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
        }
        other => panic!("expected aborted error event, got {other:?}"),
    }
}
