//! Parity port for the high-level streaming wrapper introduced in M12.
//!
//! Exercises `stream_simple` / `complete_simple` against the in-memory faux
//! provider. The wrapper is responsible for cancellation, timeout, and retry
//! semantics; these tests pin those guarantees end-to-end.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use model::{
    Api, AssistantMessageDiagnostic, Context, DiagnosticKind, FauxProvider, FauxScriptStep,
    SimpleStreamOptions, StopReason, Usage, UsageCost, api_registry::ApiProviderRegistry,
    complete_simple, faux_model, types::Provider,
};
use tokio_util::sync::CancellationToken;

fn faux_registry(provider: FauxProvider) -> ApiProviderRegistry {
    let registry = ApiProviderRegistry::new();
    registry.register(Api::Faux, Box::new(provider), Some("test".to_string()));
    registry
}

#[tokio::test]
async fn stream_simple_with_signal_aborts_mid_stream() {
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, || {
        vec![
            FauxScriptStep::Sleep(1_000),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let token = CancellationToken::new();

    let token_for_cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token_for_cancel.cancel();
    });

    let options = {
        let mut base = model::StreamOptions::default();
        base.signal = Some(token);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };
    let msg = complete_simple(&registry, &model, Context::default(), Some(options))
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn stream_simple_timeout_aborts_long_request() {
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, || {
        vec![
            FauxScriptStep::Sleep(2_000),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let options = {
        let mut base = model::StreamOptions::default();
        base.timeout_ms = Some(500);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };
    let msg = complete_simple(&registry, &model, Context::default(), Some(options))
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Aborted);
    let err = msg.error_message.as_deref().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("timed out"),
        "expected timeout error message, got {err:?}"
    );
}

#[tokio::test]
async fn stream_simple_retries_on_503() {
    // Track how many times the script factory was invoked so we can prove
    // the wrapper restarted the provider call on the retriable error.
    let calls = Arc::new(Mutex::new(0u32));
    let calls_for_factory = Arc::clone(&calls);
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, move || {
        let mut guard = calls_for_factory.lock().unwrap();
        *guard += 1;
        let n = *guard;
        if n == 1 {
            vec![FauxScriptStep::Error(
                "HTTP 503 service unavailable".to_string(),
            )]
        } else {
            vec![FauxScriptStep::Done(StopReason::Stop, Usage::default())]
        }
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let options = {
        let mut base = model::StreamOptions::default();
        base.max_retries = Some(2);
        // Compress the backoff so the test stays snappy.
        base.max_retry_delay_ms = Some(10);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };
    let msg = complete_simple(&registry, &model, Context::default(), Some(options))
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Stop);
    let diagnostics: &Vec<AssistantMessageDiagnostic> = msg
        .diagnostics
        .as_ref()
        .expect("retry should attach diagnostics");
    assert!(!diagnostics.is_empty(), "retry diagnostic missing");
    assert_eq!(diagnostics[0].kind, DiagnosticKind::Retry);
    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "provider should be invoked twice"
    );
}

/// A caller abort that lands while a retry is waiting out its backoff
/// must terminate as `Aborted` with the scheduled retry still recorded
/// in the diagnostics. The `Aborted` stop reason is what marks the
/// retry attempt as unsuccessful — the record must neither be dropped
/// nor end up attached to a successful-looking terminal message.
#[tokio::test]
async fn stream_simple_abort_during_backoff_reports_unsuccessful_retry() {
    let calls = Arc::new(Mutex::new(0u32));
    let calls_for_factory = Arc::clone(&calls);
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, move || {
        *calls_for_factory.lock().unwrap() += 1;
        vec![FauxScriptStep::Error(
            "HTTP 503 service unavailable".to_string(),
        )]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let token = CancellationToken::new();

    // The first attempt errors immediately, scheduling a 1s backoff.
    // Cancel well inside that window so the abort lands mid-backoff.
    let token_for_cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        token_for_cancel.cancel();
    });

    let options = {
        let mut base = model::StreamOptions::default();
        base.signal = Some(token);
        base.max_retries = Some(2);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };
    let msg = complete_simple(&registry, &model, Context::default(), Some(options))
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Aborted);
    let err = msg.error_message.as_deref().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("aborted"),
        "expected aborted error message, got {err:?}"
    );
    let diagnostics: &Vec<AssistantMessageDiagnostic> = msg
        .diagnostics
        .as_ref()
        .expect("scheduled retry must stay recorded on the aborted message");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::Retry);
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "retried call must not start after the abort"
    );
}

#[tokio::test]
async fn stream_simple_no_retry_on_400() {
    let calls = Arc::new(Mutex::new(0u32));
    let calls_for_factory = Arc::clone(&calls);
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, move || {
        *calls_for_factory.lock().unwrap() += 1;
        vec![FauxScriptStep::Error("HTTP 400 bad request".to_string())]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let options = {
        let mut base = model::StreamOptions::default();
        base.max_retries = Some(2);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };
    let msg = complete_simple(&registry, &model, Context::default(), Some(options))
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Error);
    assert!(
        msg.diagnostics.as_ref().is_none_or(|d| d.is_empty()),
        "no retry diagnostics expected for non-retriable error"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "provider should only be called once"
    );
}

/// A provider that intentionally ignores `options.base.signal`. Models
/// the worst-case real-world implementation (a third-party HTTP provider
/// that doesn't thread the cancellation token into its SSE loop). The
/// wrapper's cancellation gate has to terminate the stream regardless.
struct SignalIgnoringProvider;

impl model::ApiProvider for SignalIgnoringProvider {
    fn stream(
        &self,
        model: model::Model,
        _context: Context,
        _options: Option<model::StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        // Never terminates on its own; only the wrapper-level gate can stop it.
        Box::pin(async_stream::stream! {
            yield model::AssistantMessageEvent::Start {
                partial: model::AssistantMessage {
                    role: "assistant".to_string(),
                    api: model.api,
                    provider: model.provider,
                    model: model.id.clone(),
                    content: vec![],
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                },
            };
            loop {
                // Long sleep simulates a stuck upstream connection. The
                // wrapper gate must dispatch cancellation here.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        })
    }

    fn stream_simple(
        &self,
        model: model::Model,
        context: Context,
        _options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        // Drop the signal-bearing options on the floor.
        self.stream(model, context, None)
    }
}

#[tokio::test]
async fn wrapper_cancels_provider_that_ignores_signal() {
    // Regression guard for the wrapper-level cancellation gate added by
    // the #32 audit. Without that gate, a provider that drops
    // `options.base.signal` would leave `complete_simple` hanging
    // indefinitely on `inner_stream.next().await`.
    let registry = ApiProviderRegistry::new();
    registry.register(
        Api::Faux,
        Box::new(SignalIgnoringProvider),
        Some("test".to_string()),
    );
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");
    let token = CancellationToken::new();

    let token_for_cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token_for_cancel.cancel();
    });

    let options = {
        let mut base = model::StreamOptions::default();
        base.signal = Some(token);
        let mut o = SimpleStreamOptions::default();
        o.base = base;
        o
    };

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        complete_simple(&registry, &model, Context::default(), Some(options)),
    )
    .await;

    let msg = result
        .expect("complete_simple must return promptly after cancellation")
        .expect("complete_simple should resolve into a message");

    assert_eq!(
        msg.stop_reason,
        StopReason::Aborted,
        "wrapper must synthesize an Aborted terminal event when the provider ignores signal"
    );
    let err = msg.error_message.as_deref().unwrap_or("");
    assert!(
        err.to_ascii_lowercase().contains("aborted"),
        "expected aborted error message, got {err:?}"
    );
}

#[tokio::test]
async fn stream_simple_response_id_propagated() {
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, || {
        vec![
            FauxScriptStep::StartWithPartial(Box::new(|partial| {
                partial.response_id = Some("resp_abc123".to_string());
            })),
            FauxScriptStep::Done(StopReason::Stop, Usage::default()),
        ]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let msg = complete_simple(&registry, &model, Context::default(), None)
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.response_id.as_deref(), Some("resp_abc123"));
}

#[tokio::test]
async fn stream_simple_total_tokens_correct() {
    // Parity from `test/total-tokens.test.ts`: `total_tokens` reflects the
    // sum of input + output + cache_read + cache_write.
    let usage = Usage {
        input: 100,
        output: 200,
        cache_read: 50,
        cache_write: 25,
        total_tokens: 100 + 200 + 50 + 25,
        cost: UsageCost::default(),
    };
    let usage_for_factory = usage.clone();
    let provider = FauxProvider::from_factory(Api::Faux, Provider::OpenAI, move || {
        vec![FauxScriptStep::Done(
            StopReason::Stop,
            usage_for_factory.clone(),
        )]
    });
    let registry = faux_registry(provider);
    let model = faux_model(Api::Faux, Provider::OpenAI, "faux-1");

    let msg = complete_simple(&registry, &model, Context::default(), None)
        .await
        .expect("complete_simple should resolve");

    assert_eq!(msg.usage.total_tokens, 375);
    assert_eq!(
        msg.usage.total_tokens,
        msg.usage.input + msg.usage.output + msg.usage.cache_read + msg.usage.cache_write,
    );
}
