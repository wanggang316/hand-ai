//! Streaming text-delta coverage.
//!
//! Asserts that multiple text deltas concatenated through
//! `EventStream::collect_to_message` yield the full text in the final
//! assistant message, with the right ordering of structural events.

use model::{
    Api, AssistantContentBlock, Context, EventStream, FauxProvider, FauxScriptStep, StopReason,
    Usage, api_registry::ApiProvider, faux_model, types::Provider,
};

#[tokio::test]
async fn multiple_text_deltas_concatenate_into_final_message() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hello".to_string()),
            FauxScriptStep::TextDelta(" ".to_string()),
            FauxScriptStep::TextDelta("world".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let raw = provider.stream(model, Context::default(), None);

    let event_stream = EventStream::with_default_provenance(raw);
    let msg = event_stream
        .collect_to_message()
        .await
        .expect("done should produce Ok");
    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        AssistantContentBlock::Text(t) => assert_eq!(t.text, "hello world"),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn text_deltas_emit_start_delta_end_in_order() {
    use futures::StreamExt;
    use model::AssistantMessageEvent;

    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("a".to_string()),
            FauxScriptStep::TextDelta("b".to_string()),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let mut stream = provider.stream(model, Context::default(), None);

    let mut order = Vec::new();
    while let Some(ev) = stream.next().await {
        order.push(match ev {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::Done { .. } => "done",
            _ => "other",
        });
    }

    assert_eq!(
        order,
        vec![
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done",
        ]
    );
}
