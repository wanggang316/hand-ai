//! Wrapper around a stream of `AssistantMessageEvent`s.
//!
//! Mirrors the TS `AssistantMessageEventStream` semantics: callers iterate
//! events while they arrive and can drain the stream into a final
//! `AssistantMessage`. A `Done` event resolves to `Ok`; an `Error` event
//! resolves to `Err`. Streams that end without a terminal event yield an
//! aborted error message — and that synthesized message must carry truthful
//! provider attribution, so callers always supply a [`Provenance`] at
//! construction time.

use crate::types::{Api, AssistantMessage, AssistantMessageEvent, Provider, StopReason, Usage};
use futures::Stream;
use futures::StreamExt;
use std::pin::Pin;
use std::task::{Context, Poll};

type DynEventStream = Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>;

/// Provider attribution captured at stream construction. Used to fill in a
/// truthful aborted-message envelope when the stream truncates before any
/// `partial` payload arrives.
#[derive(Debug, Clone)]
pub struct Provenance {
    pub api: Api,
    pub provider: Provider,
    pub model: String,
}

/// Wrapper over a pinned, boxed event stream with helpers for collection and
/// projection.
pub struct EventStream {
    inner: DynEventStream,
    provenance: Provenance,
}

impl EventStream {
    /// Use when you already have provider context (preferred for production).
    pub fn new<S>(provenance: Provenance, s: S) -> Self
    where
        S: Stream<Item = AssistantMessageEvent> + Send + 'static,
    {
        Self {
            inner: Box::pin(s),
            provenance,
        }
    }

    /// Test-only constructor that defaults provenance to faux/openai
    /// placeholders. Production callers should prefer
    /// [`EventStream::new`].
    #[cfg(any(test, feature = "faux"))]
    pub fn with_default_provenance<S>(s: S) -> Self
    where
        S: Stream<Item = AssistantMessageEvent> + Send + 'static,
    {
        Self::new(
            Provenance {
                api: Api::OpenAICompletions,
                provider: Provider::OpenAI,
                model: String::new(),
            },
            s,
        )
    }

    /// Borrow the provenance captured at construction time.
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Pull the next event, or `None` if the stream ended.
    pub async fn next(&mut self) -> Option<AssistantMessageEvent> {
        self.inner.as_mut().next().await
    }

    /// Drain the stream and return the terminal `AssistantMessage`.
    ///
    /// `Ok(message)` when the stream ends with `Done`.
    /// `Err(message)` when the stream ends with `Error`, or when the stream
    /// terminates without a terminal event (in which case the returned
    /// message has stop reason `Aborted`). When no `partial` ever arrived,
    /// the synthesized message uses the [`Provenance`] captured at
    /// construction.
    // Both variants are the same type on purpose: a failed turn still
    // carries everything that arrived before it failed, and callers read
    // the content, usage, and stop reason off it exactly as they would a
    // successful one. `result_large_err` asks for the error to be boxed,
    // which would save nothing here — the `Ok` variant is the same type
    // and sets the width of the `Result` either way — while making the
    // two halves asymmetric to construct and to match on.
    #[allow(clippy::result_large_err)]
    pub async fn collect_to_message(mut self) -> Result<AssistantMessage, AssistantMessage> {
        let mut last_partial: Option<AssistantMessage> = None;
        while let Some(event) = self.inner.as_mut().next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => return Ok(message),
                AssistantMessageEvent::Error { error, .. } => return Err(error),
                AssistantMessageEvent::Start { partial }
                | AssistantMessageEvent::TextStart { partial, .. }
                | AssistantMessageEvent::TextDelta { partial, .. }
                | AssistantMessageEvent::TextEnd { partial, .. }
                | AssistantMessageEvent::ThinkingStart { partial, .. }
                | AssistantMessageEvent::ThinkingDelta { partial, .. }
                | AssistantMessageEvent::ThinkingEnd { partial, .. }
                | AssistantMessageEvent::ToolCallStart { partial, .. }
                | AssistantMessageEvent::ToolCallDelta { partial, .. }
                | AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                    last_partial = Some(partial);
                }
            }
        }

        // Stream ended without a terminal event — synthesize an aborted error
        // using either the most recent partial we observed or the provenance
        // captured at construction.
        let mut aborted = last_partial.unwrap_or_else(|| aborted_default_message(&self.provenance));
        aborted.stop_reason = StopReason::Aborted;
        if aborted.error_message.is_none() {
            aborted.error_message = Some("stream ended without terminal event".to_string());
        }
        Err(aborted)
    }

    /// Project the stream into one yielding only `text_delta` payloads.
    pub fn text_deltas(self) -> impl Stream<Item = String> + Send {
        self.inner.filter_map(|event| async move {
            match event {
                AssistantMessageEvent::TextDelta { delta, .. } => Some(delta),
                _ => None,
            }
        })
    }

    /// Helper: yields completed `ToolCall` payloads as the stream drives forward.
    pub fn tool_calls(self) -> impl Stream<Item = crate::types::ToolCall> + Send {
        self.filter_map(|ev| async move {
            match ev {
                AssistantMessageEvent::ToolCallEnd { tool_call, .. } => Some(tool_call),
                _ => None,
            }
        })
    }
}

impl Stream for EventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

fn aborted_default_message(p: &Provenance) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: p.api,
        provider: p.provider,
        model: p.model.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::Aborted,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}
