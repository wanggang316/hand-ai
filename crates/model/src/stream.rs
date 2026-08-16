//! High-level streaming entry points with cancellation, timeout, and retry.
//!
//! Wraps the low-level `ApiProvider::stream_simple` call with:
//! - **Provider resolution** via the `ApiProviderRegistry`.
//! - **Cross-provider message normalization** via `transform_messages`.
//! - **Timeout** support that races against a `tokio::time::sleep` and
//!   cancels the in-flight provider call by toggling a shared
//!   `CancellationToken`.
//! - **Cooperative cancellation** by merging an externally supplied
//!   `signal: CancellationToken` with the timeout token; both are exposed to
//!   the provider via `StreamOptions::signal`.
//! - **Retry on transient errors** (HTTP 429 / 503 / connection-reset). Each
//!   retry uses exponential backoff (`base * 2^attempt`) capped at
//!   `max_retry_delay_ms` (default 60s) and appends a `Retry` diagnostic to
//!   the resulting `AssistantMessage`.
//!
//! The wrapper is intentionally thin: it does not re-implement provider
//! transport, content-block tracking, or per-provider quirks — those live in
//! the individual `ApiProvider` impls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api_registry::ApiProviderRegistry;
use crate::client::ClientError;
use crate::transform::transform_messages;
use crate::types::{
    AssistantMessage, AssistantMessageDiagnostic, AssistantMessageEvent, Context, DiagnosticKind,
    Message, Model, SimpleStreamOptions, StopReason, Usage,
};
use crate::utils::event_stream::{EventStream, Provenance};
use async_stream::stream;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// Default base delay between retries (1 second).
const DEFAULT_BASE_RETRY_DELAY_MS: u64 = 1_000;
/// Default cap on exponential-backoff sleep between retries (60 seconds).
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
/// Default number of retry attempts on retriable errors.
const DEFAULT_MAX_RETRIES: u32 = 2;

/// High-level streaming entry point with cancellation, timeout, and retry.
///
/// Resolves the provider for `model.api`, normalizes `context.messages` for
/// cross-provider compatibility, then drives the provider stream while
/// honoring `options.signal`, `options.timeout_ms`, and `options.max_retries`.
///
/// On retriable error (HTTP 429 / 503 / connection-reset) the wrapper
/// transparently restarts the provider call after an exponential-backoff
/// sleep, appending a `Retry` diagnostic to the eventual terminal message.
///
/// # Errors
///
/// Returns [`ClientError::ProviderNotFound`] if `model.api` is not registered.
pub fn stream_simple(
    registry: &ApiProviderRegistry,
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Result<EventStream, ClientError> {
    let provider = registry
        .get(&model.api)
        .ok_or_else(|| ClientError::ProviderNotFound {
            api: model.api,
            model_id: model.id.clone(),
        })?;

    // Normalize messages for the target model. Per-provider providers run
    // their own internal normalizers (M6) for tool-call IDs etc., so we pass
    // `None` here.
    let transformed_messages: Vec<Message> = transform_messages(&context.messages, model, None);
    let prepared_context = Context {
        system_prompt: context.system_prompt,
        messages: transformed_messages,
        tools: context.tools,
    };

    let mut options = options.unwrap_or_default();
    let user_signal = options.base.signal.clone();
    let timeout_ms = options.base.timeout_ms;
    let max_retries = options.base.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    let max_retry_delay_ms = options
        .base
        .max_retry_delay_ms
        .unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);

    // Combined cancellation token: cancelled if (a) the user signal fires, or
    // (b) the timeout elapses.
    let combined = CancellationToken::new();
    if let Some(user) = user_signal.as_ref() {
        let combined_for_user = combined.clone();
        let user = user.clone();
        tokio::spawn(async move {
            user.cancelled().await;
            combined_for_user.cancel();
        });
    }
    // Track whether the timeout fired so we can rewrite the error message.
    let timed_out = Arc::new(Mutex::new(false));
    if let Some(ms) = timeout_ms {
        let combined_for_timeout = combined.clone();
        let timed_out_flag = Arc::clone(&timed_out);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            *timed_out_flag.lock().unwrap() = true;
            combined_for_timeout.cancel();
        });
    }
    options.base.signal = Some(combined.clone());

    let provenance = Provenance {
        api: model.api,
        provider: model.provider,
        model: model.id.clone(),
    };

    let model_clone = model.clone();
    let provider = provider.clone();
    let timed_out_for_stream = Arc::clone(&timed_out);

    let inner = stream! {
        let mut diagnostics: Vec<AssistantMessageDiagnostic> = Vec::new();
        let mut attempt: u32 = 0;
        loop {
            let mut current_options = options.clone();
            // Each provider call gets the same combined signal.
            current_options.base.signal = Some(combined.clone());
            let mut inner_stream = provider.stream_simple(
                model_clone.clone(),
                prepared_context.clone(),
                Some(current_options),
            );

            let mut retried = false;
            // Cancellation gate: providers vary in how thoroughly they
            // honour `options.base.signal` from inside their HTTP / SSE
            // loops. Wrap each `next().await` in a select against the
            // combined token so cancellation (user signal OR timeout)
            // terminates the stream promptly even when a provider is
            // currently blocked on a long-lived upstream read. The
            // inner stream future is dropped on the cancelled branch,
            // which collapses the underlying reqwest connection.
            loop {
                let event_opt = tokio::select! {
                    biased;
                    _ = combined.cancelled() => None,
                    ev = inner_stream.next() => ev,
                };
                let Some(event) = event_opt else {
                    if combined.is_cancelled() {
                        let was_timeout = *timed_out_for_stream.lock().unwrap();
                        let error_message = if was_timeout {
                            format!("Request timed out after {}ms", timeout_ms.unwrap_or(0))
                        } else {
                            "Request was aborted".to_string()
                        };
                        let mut aborted = AssistantMessage {
                            role: "assistant".to_string(),
                            api: model_clone.api,
                            provider: model_clone.provider,
                            model: model_clone.id.clone(),
                            content: vec![],
                            usage: Usage::default(),
                            stop_reason: StopReason::Aborted,
                            raw_stop_reason: None,
                            error_message: Some(error_message),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            response_model: None,
                            response_id: None,
                            diagnostics: None,
                        };
                        if !diagnostics.is_empty() {
                            attach_diagnostics(&mut aborted, &diagnostics);
                        }
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted,
                        };
                        return;
                    }
                    // Stream ended naturally without a terminal event.
                    break;
                };
                match event {
                    AssistantMessageEvent::Done { reason, mut message } => {
                        if !diagnostics.is_empty() {
                            attach_diagnostics(&mut message, &diagnostics);
                        }
                        yield AssistantMessageEvent::Done { reason, message };
                        return;
                    }
                    AssistantMessageEvent::Error { reason, mut error } => {
                        // Rewrite cancellation caused by timeout.
                        let was_timeout = *timed_out_for_stream.lock().unwrap();
                        if reason == StopReason::Aborted && was_timeout {
                            error.error_message = Some(format!(
                                "Request timed out after {}ms",
                                timeout_ms.unwrap_or(0)
                            ));
                        }
                        let err_msg = error.error_message.clone().unwrap_or_default();
                        let cancelled_by_user = reason == StopReason::Aborted && !was_timeout;

                        // Don't retry on cancellation: the caller wants out.
                        if !cancelled_by_user
                            && reason == StopReason::Error
                            && attempt < max_retries
                            && is_retriable_error(&err_msg)
                        {
                            attempt += 1;
                            let delay = compute_backoff(attempt, max_retry_delay_ms);
                            diagnostics.push(make_retry_diagnostic(
                                attempt,
                                max_retries,
                                &err_msg,
                                delay,
                            ));
                            // Drain remaining events from the failing stream
                            // so spawned tasks can wind down cleanly.
                            while (inner_stream.next().await).is_some() {}
                            // Sleep, but bail out early if the caller cancels.
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                                _ = combined.cancelled() => {
                                    // Caller cancelled mid-backoff — synthesize an aborted message.
                                    let mut aborted = error;
                                    aborted.stop_reason = StopReason::Aborted;
                                    let was_timeout_now = *timed_out_for_stream.lock().unwrap();
                                    aborted.error_message = Some(if was_timeout_now {
                                        format!("Request timed out after {}ms", timeout_ms.unwrap_or(0))
                                    } else {
                                        "Request was aborted".to_string()
                                    });
                                    if !diagnostics.is_empty() {
                                        attach_diagnostics(&mut aborted, &diagnostics);
                                    }
                                    yield AssistantMessageEvent::Error {
                                        reason: StopReason::Aborted,
                                        error: aborted,
                                    };
                                    return;
                                }
                            }
                            retried = true;
                            break;
                        }

                        if !diagnostics.is_empty() {
                            attach_diagnostics(&mut error, &diagnostics);
                        }
                        yield AssistantMessageEvent::Error { reason, error };
                        return;
                    }
                    other => yield other,
                }
            }

            if !retried {
                // Stream ended without a terminal event; let EventStream
                // synthesize the aborted envelope from `provenance`.
                return;
            }
        }
    };

    Ok(EventStream::new(provenance, inner))
}

/// High-level non-streaming entry point. Drives [`stream_simple`] to
/// completion and returns the terminal `AssistantMessage`.
///
/// Mirrors the existing `Client::complete_simple` semantics: error events
/// resolve to `Ok(message)` with the message's `stop_reason` indicating the
/// failure mode (`Error` / `Aborted`).
///
/// # Errors
///
/// Returns [`ClientError::ProviderNotFound`] if no provider is registered,
/// or [`ClientError::StreamEndedWithoutResult`] if the stream terminates
/// without producing a terminal event.
pub async fn complete_simple(
    registry: &ApiProviderRegistry,
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Result<AssistantMessage, ClientError> {
    let stream = stream_simple(registry, model, context, options)?;
    match stream.collect_to_message().await {
        Ok(msg) => Ok(msg),
        Err(msg) => Ok(msg),
    }
}

fn is_retriable_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    // HTTP status codes signaling overload / transient upstream failure.
    // 429 too-many-requests, 502 bad gateway, 503 service unavailable,
    // 504 gateway timeout — all are conventionally retriable. Other 5xx
    // codes (500, 501, 505) are deliberately NOT retried because they
    // indicate programming / configuration errors that retrying would
    // only amplify.
    for code in ["429", "502", "503", "504"] {
        if lower.contains(code) {
            return true;
        }
    }
    // Common connection-reset / network-blip indicators.
    if lower.contains("connection reset")
        || lower.contains("econnreset")
        || lower.contains("connection closed")
        || lower.contains("connection aborted")
    {
        return true;
    }
    // OpenAI-compatible providers (z.ai notably) surface transient
    // blips as `finish_reason: network_error` in the stream, which the
    // provider adapter maps to the error message
    // `"Provider finish_reason: network_error"`. Recognise the token
    // directly so a single z.ai connectivity dip doesn't terminate the
    // agent loop.
    if lower.contains("network_error") {
        return true;
    }
    // Apple's URLSession surfaces "Network connection lost." for
    // transient connectivity blips it believes will recover on retry.
    // Anthropic's Swift SDK passes this through verbatim. Recognise
    // the substring so iOS/macOS users on flaky WiFi don't see
    // momentary handoffs terminate the agent loop.
    if lower.contains("network connection lost") {
        return true;
    }
    // SDK / proxy chains sometimes close the connection before any
    // chunks arrive, surfacing as `request ended without sending any
    // chunks`. Retrying covers transient upstream queueing or proxy
    // handshake races. Match the substring `ended without` so related
    // wordings around the same root cause are treated alike.
    if lower.contains("ended without") {
        return true;
    }
    // Additional transient tokens. Each maps to a real upstream /
    // proxy failure mode:
    // - "overloaded" — Anthropic-style overloaded_error
    // - "rate limit" / "too many requests" — 429 with a different body
    // - "fetch failed" — generic SDK transport failure
    // - "service unavailable" / "internal error" — non-status-numbered
    //   service blips (some proxies omit the code in their message)
    // - "socket hang up" — Node-style ECONNRESET wording
    // - "upstream connect" / "reset before headers" / "other side closed"
    //   — proxy-layer connectivity blips
    // - "timed out" / "timeout" — explicit upstream timeout signal that
    //   isn't always paired with a 504 status string
    // - "terminated" — Anthropic stream early-EOF wording
    // - "retry delay" — provider asked for a backoff window
    for needle in [
        "overloaded",
        "rate limit",
        "too many requests",
        "fetch failed",
        "service unavailable",
        "internal error",
        "socket hang up",
        "upstream connect",
        "reset before headers",
        "other side closed",
        "timed out",
        "timeout",
        "terminated",
        "retry delay",
        // Smithy / AWS SDK HTTP/2 transport surface for Bedrock and
        // similar providers — the HTTP/2 stream dies before the
        // response body lands, so the SDK throws `http2 request did
        // not get a response`. Transient — retry.
        "http2 request did not get a response",
        // gRPC-style capacity wording forwarded by some
        // OpenAI-compatible gateways when a serving worker hits its
        // request cap, e.g. "ResourceExhausted: Worker local total
        // request limit reached (288/48)". Transient — retry.
        "resourceexhausted",
    ] {
        if lower.contains(needle) {
            return true;
        }
    }
    false
}

fn compute_backoff(attempt: u32, max_delay_ms: u64) -> u64 {
    // attempt is 1-based after we increment on first retry.
    let exp = attempt.saturating_sub(1);
    let raw = DEFAULT_BASE_RETRY_DELAY_MS.saturating_mul(1_u64 << exp.min(20));
    raw.min(max_delay_ms)
}

fn make_retry_diagnostic(
    attempt: u32,
    max_retries: u32,
    reason: &str,
    delay_ms: u64,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic::new(
        DiagnosticKind::Retry,
        format!("Retry {attempt}/{max_retries}: {reason}"),
    )
    .with_details(serde_json::json!({
        "attempt": attempt,
        "maxRetries": max_retries,
        "delayMs": delay_ms,
        "reason": reason,
    }))
}

fn attach_diagnostics(message: &mut AssistantMessage, diagnostics: &[AssistantMessageDiagnostic]) {
    let existing = message.diagnostics.take().unwrap_or_default();
    let mut merged = existing;
    merged.extend(diagnostics.iter().cloned());
    message.diagnostics = Some(merged);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retriable_recognizes_429_and_503() {
        assert!(is_retriable_error("HTTP 429 too many requests"));
        assert!(is_retriable_error("HTTP 503 service unavailable"));
        assert!(is_retriable_error("connection reset by peer"));
        assert!(is_retriable_error("ECONNRESET"));
    }

    #[test]
    fn retriable_rejects_400_and_500() {
        assert!(!is_retriable_error("HTTP 400 bad request"));
        assert!(!is_retriable_error("HTTP 401 unauthorized"));
        assert!(!is_retriable_error("HTTP 500 internal server error"));
    }

    /// OpenAI-compatible providers (z.ai notably) signal transient
    /// connectivity failures via `finish_reason: "network_error"` in
    /// the stream. The provider adapter surfaces this as
    /// `errorMessage: "Provider finish_reason: network_error"`. Without
    /// recognising it here, a single z.ai blip would terminate the
    /// agent loop with no retry.
    #[test]
    fn retriable_recognizes_provider_network_error() {
        assert!(is_retriable_error("Provider finish_reason: network_error"));
        // Bare token also matches — covers a future provider that
        // surfaces it differently.
        assert!(is_retriable_error("network_error"));
        // 502 Bad Gateway and 504 Gateway Timeout are similar transient
        // upstream errors; recognise both so flaky CDN paths retry too.
        assert!(is_retriable_error("HTTP 502 bad gateway"));
        assert!(is_retriable_error("HTTP 504 gateway timeout"));
    }

    /// Defensive: a 5xx status that is NOT a transient-upstream code
    /// must still be treated as terminal. We don't want to silently
    /// retry a 500 forever.
    #[test]
    fn retriable_still_rejects_other_5xx() {
        assert!(!is_retriable_error("HTTP 501 not implemented"));
        assert!(!is_retriable_error("HTTP 505 http version not supported"));
    }

    /// SDK / proxy chains sometimes close the connection before any
    /// chunks arrive. The error surfaces as a string of the form
    /// "request ended without sending any chunks" (or similar wordings)
    /// — these are textbook transient failures and the agent should
    /// retry rather than terminate the loop.
    #[test]
    fn retriable_recognizes_request_ended_without_chunks() {
        assert!(is_retriable_error(
            "request ended without sending any chunks"
        ));
        assert!(is_retriable_error("Stream ended without response body"));
        // A 500 with similar wording should still be rejected because
        // 500 isn't in the retriable set.
        assert!(!is_retriable_error("HTTP 500: handler error"));
    }

    /// The classifier matches a long list of transient tokens
    /// (overloaded, rate limit, fetch failed, socket hang up, timeout,
    /// terminated, retry delay, ...). Pin a handful so a future
    /// refactor can't silently drop the coverage.
    #[test]
    fn retriable_recognizes_transient_provider_tokens() {
        for msg in [
            "overloaded_error: please try again",
            "rate limit exceeded",
            "Too Many Requests",
            "fetch failed",
            "service unavailable",
            "socket hang up",
            "upstream connect error",
            "reset before headers were received",
            "other side closed",
            "request timed out after 60s",
            "AbortError: timeout",
            "Stream terminated unexpectedly",
            "retry delay 30 seconds",
            // Bedrock / Smithy HTTP/2 transport blips
            "http2 request did not get a response",
            "HTTP2 Request did not get a response within deadline",
        ] {
            assert!(is_retriable_error(msg), "expected retriable: {msg}");
        }
    }

    /// Apple's URLSession surfaces a "Network connection lost." string
    /// for transient connectivity blips that the OS itself believes
    /// will recover on retry. Anthropic's Swift SDK passes this through
    /// verbatim; without recognising it, an iOS/macOS user on flaky
    /// WiFi would see every momentary handoff terminate the agent
    /// loop. We anchor on a substring so minor wording changes
    /// ("network connection was lost") still retry.
    #[test]
    fn retriable_recognizes_network_connection_lost() {
        assert!(is_retriable_error("Network connection lost."));
        assert!(is_retriable_error("network connection lost"));
        assert!(is_retriable_error(
            "Provider returned: Network connection lost. Try again."
        ));
        // Adjacent phrasing that should NOT match — we want a tight
        // anchor, not a generic "lost" string that would catch
        // unrelated copy.
        assert!(!is_retriable_error("Network is fine."));
        assert!(!is_retriable_error("Connection details: ..."));
    }

    /// Some OpenAI-compatible gateways forward gRPC-style status text
    /// when a serving worker hits its request cap. The message carries
    /// no retriable HTTP status code, so the `ResourceExhausted` token
    /// itself must be recognised — otherwise a momentary capacity blip
    /// terminates the agent loop instead of retrying.
    #[test]
    fn retriable_recognizes_resource_exhausted() {
        assert!(is_retriable_error(
            "ResourceExhausted: Worker local total request limit reached (288/48)"
        ));
        assert!(is_retriable_error("resourceexhausted"));
        // Unrelated resource wording must not match.
        assert!(!is_retriable_error("resource not found"));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        assert_eq!(compute_backoff(1, 60_000), 1_000);
        assert_eq!(compute_backoff(2, 60_000), 2_000);
        assert_eq!(compute_backoff(3, 60_000), 4_000);
        // Cap kicks in well before overflow.
        assert_eq!(compute_backoff(20, 5_000), 5_000);
    }
}
