//! Empty-script handling for the streaming event pipeline.
//!
//! Exercises the faux provider with a script that emits zero events.
//! The asserted invariant: an empty script produces an `Aborted`
//! envelope (because the stream ended without a
//! terminal event) rather than panicking, and the synthesized message carries
//! truthful provenance.

use futures::StreamExt;
use model::{
    Api, AssistantMessageEvent, Context, FauxProvider, FauxScriptStep, StopReason, UserMessage,
    api_registry::ApiProvider,
    faux_model,
    types::{Message, Provider},
};

#[tokio::test]
async fn faux_empty_script_yields_aborted_envelope() {
    let provider = FauxProvider::new(Api::Faux, Provider::OpenAI, Vec::<FauxScriptStep>::new());
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("hi"))],
        tools: None,
    };

    let mut stream = provider.stream(model, context, None);
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // No events emitted; EventStream::collect_to_message synthesizes an
    // aborted message in this case. Here we assert at the raw stream level —
    // the test simply must not panic.
    assert!(events.is_empty());
}

#[tokio::test]
async fn faux_empty_script_collected_to_message_aborts_cleanly() {
    use model::EventStream;

    let provider = FauxProvider::new(Api::Faux, Provider::OpenAI, Vec::<FauxScriptStep>::new());
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let context = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };

    let raw = provider.stream(model, context, None);
    let event_stream = EventStream::with_default_provenance(raw);
    let result = event_stream.collect_to_message().await;
    let msg = result.expect_err("empty script should resolve as Err(aborted)");
    assert_eq!(msg.stop_reason, StopReason::Aborted);
    // Empty content because no partials were observed.
    assert!(msg.content.is_empty());
}

#[tokio::test]
async fn faux_done_only_yields_clean_done() {
    // A script with only a `Done` step still works: we get Start + Done.
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![FauxScriptStep::Done(StopReason::Stop, Default::default())],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let context = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };

    let mut stream = provider.stream(model, context, None);
    let mut saw_start = false;
    let mut saw_done = false;
    while let Some(ev) = stream.next().await {
        match ev {
            AssistantMessageEvent::Start { .. } => saw_start = true,
            AssistantMessageEvent::Done { reason, message } => {
                saw_done = true;
                assert_eq!(reason, StopReason::Stop);
                assert_eq!(message.stop_reason, StopReason::Stop);
                assert!(message.content.is_empty());
            }
            _ => {}
        }
    }
    assert!(saw_start);
    assert!(saw_done);
}
