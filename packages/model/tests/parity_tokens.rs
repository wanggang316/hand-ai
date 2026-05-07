//! Parity port of `pi-mono/packages/ai/test/total-tokens.test.ts`.
//!
//! Asserts that `Usage::total_tokens` honors the
//! `input + output + cache_read + cache_write` invariant when emitted via the
//! faux provider's `Done` step.

use model::{
    Api, Context, EventStream, FauxProvider, FauxScriptStep, StopReason, Usage, UsageCost,
    api_registry::ApiProvider, faux_model, types::Provider,
};

#[tokio::test]
async fn total_tokens_matches_sum_of_components() {
    let usage = Usage {
        input: 100,
        output: 50,
        cache_read: 30,
        cache_write: 20,
        total_tokens: 200,
        cost: UsageCost::default(),
    };

    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![
            FauxScriptStep::TextDelta("hi".to_string()),
            FauxScriptStep::Done(StopReason::Stop, usage.clone()),
        ],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let raw = provider.stream(model, Context::default(), None);

    let msg = EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("done should produce Ok");

    assert_eq!(msg.usage.input, usage.input);
    assert_eq!(msg.usage.output, usage.output);
    assert_eq!(msg.usage.cache_read, usage.cache_read);
    assert_eq!(msg.usage.cache_write, usage.cache_write);
    assert_eq!(
        msg.usage.total_tokens,
        msg.usage.input + msg.usage.output + msg.usage.cache_read + msg.usage.cache_write
    );
}

#[tokio::test]
async fn zero_usage_yields_zero_total_tokens() {
    let provider = FauxProvider::new(
        Api::Faux,
        Provider::OpenAI,
        vec![FauxScriptStep::Done(StopReason::Stop, Usage::default())],
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let raw = provider.stream(model, Context::default(), None);

    let msg = EventStream::with_default_provenance(raw)
        .collect_to_message()
        .await
        .expect("done should produce Ok");
    assert_eq!(msg.usage.total_tokens, 0);
    assert_eq!(
        msg.usage.total_tokens,
        msg.usage.input + msg.usage.output + msg.usage.cache_read + msg.usage.cache_write
    );
}
