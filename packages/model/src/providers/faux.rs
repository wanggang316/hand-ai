//! Faux (in-memory) provider for tests and harness scenarios.
//!
//! Mirrors `pi-mono/packages/ai/src/providers/faux.ts` but uses a
//! Rust-friendly script-step model: callers describe the events they want the
//! stream to emit (text deltas, thinking deltas, tool calls, errors, sleeps,
//! aborts, …) and the provider drives those steps over a `tokio::sync::mpsc`
//! channel.
//!
//! Gated behind `cfg(any(test, feature = "faux"))` so it never ships into a
//! production build accidentally.

#![cfg(any(test, feature = "faux"))]

use std::pin::Pin;

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Cost, InputType,
    Model, Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    ToolCall, Usage,
};
use crate::utils::event_stream::EventStream;
use async_stream::stream;
use futures::Stream;
use tokio::sync::mpsc;

/// One scripted step the faux provider should emit.
///
/// The script is consumed in order. The provider walks it once per `stream`
/// invocation, emitting the corresponding event (or performing the
/// corresponding side effect, e.g. sleeping). The script does not need to be
/// well-formed — for instance you can emit a `TextDelta` without first
/// scripting `Start`; the provider will synthesize a default `Start` so the
/// wire shape stays sane for harness consumers.
pub enum FauxScriptStep {
    /// Emit a `text_delta` event with the given chunk.
    TextDelta(String),
    /// Emit a `thinking_delta` event.
    ThinkingDelta(String),
    /// Emit a complete tool call as a single `tool_call_end` event.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// Sleep for `ms` milliseconds. Useful for testing timeouts/cancellation.
    Sleep(u64),
    /// Emit a terminal `done` event with the given stop reason and usage.
    Done(StopReason, Usage),
    /// Emit a terminal `error` event whose message is the supplied string.
    Error(String),
    /// Emit a `start` event, allowing callers to mutate the seeded partial
    /// (e.g. set `response_id`).
    StartWithPartial(Box<dyn Fn(&mut AssistantMessage) + Send + Sync>),
}

/// A faux `ApiProvider`. Each `stream()` call rebuilds the script from the
/// supplied factory, so a single provider instance can be reused across
/// multiple turns.
pub struct FauxProvider {
    api: Api,
    provider: Provider,
    script_factory: Box<dyn Fn() -> Vec<FauxScriptStep> + Send + Sync>,
}

impl FauxProvider {
    /// Build a provider that replays the same script on every call.
    pub fn new(api: Api, provider: Provider, script: Vec<FauxScriptStep>) -> Self {
        // Wrap the script in an `Option` so we can move it out exactly once
        // and have subsequent calls return an empty script (the caller likely
        // wants `from_factory` for multi-call scenarios).
        let script_cell = std::sync::Mutex::new(Some(script));
        Self {
            api,
            provider,
            script_factory: Box::new(move || {
                script_cell.lock().unwrap().take().unwrap_or_default()
            }),
        }
    }

    /// Build a provider whose script is freshly produced on every call.
    pub fn from_factory(
        api: Api,
        provider: Provider,
        factory: impl Fn() -> Vec<FauxScriptStep> + Send + Sync + 'static,
    ) -> Self {
        Self {
            api,
            provider,
            script_factory: Box::new(factory),
        }
    }
}

impl ApiProvider for FauxProvider {
    fn stream(
        &self,
        model: Model,
        _context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let script = (self.script_factory)();
        let api = self.api;
        let provider = self.provider;
        let signal = options.as_ref().and_then(|o| o.signal.clone());

        let (tx, mut rx) = mpsc::channel::<AssistantMessageEvent>(32);

        tokio::spawn(async move {
            run_script_into_channel(script, model, api, provider, signal, tx).await;
        });

        // Drive the receiver through `EventStream::with_default_provenance`
        // so we exercise the M2 test seam, then unwrap back to the raw event
        // stream the trait expects.
        let receiver_stream = stream! {
            while let Some(ev) = rx.recv().await {
                yield ev;
            }
        };
        let event_stream =
            EventStream::with_default_provenance(Box::pin(receiver_stream)
                as Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>);

        Box::pin(event_stream)
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Convenience: build a `Model` targeting the faux provider with sensible
/// defaults (text + image input, faux base URL, no cost).
pub fn faux_model(api: Api, provider: Provider, model_id: impl Into<String>) -> Model {
    let id = model_id.into();
    Model {
        name: id.clone(),
        id,
        api,
        provider,
        base_url: "http://localhost:0".to_string(),
        reasoning: false,
        input: vec![InputType::Text, InputType::Image],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128_000,
        max_tokens: 16_384,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn empty_partial(api: Api, provider: Provider, model_id: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api,
        provider,
        model: model_id.to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

fn aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    let mut m = partial.clone();
    m.stop_reason = StopReason::Aborted;
    m.error_message = Some("Request was aborted".to_string());
    m
}

#[allow(unused_assignments)]
async fn run_script_into_channel(
    script: Vec<FauxScriptStep>,
    model: Model,
    api: Api,
    provider: Provider,
    signal: Option<tokio_util::sync::CancellationToken>,
    tx: mpsc::Sender<AssistantMessageEvent>,
) {
    let mut partial = empty_partial(api, provider, &model.id);
    let mut started = false;
    // Track per-content-index state so deltas append to the correct block.
    let mut current_text_idx: Option<u32> = None;
    let mut current_thinking_idx: Option<u32> = None;

    macro_rules! send {
        ($ev:expr) => {{
            if tx.send($ev).await.is_err() {
                return;
            }
        }};
    }

    macro_rules! ensure_started {
        () => {{
            if !started {
                started = true;
                send!(AssistantMessageEvent::Start {
                    partial: partial.clone(),
                });
            }
        }};
    }

    macro_rules! check_aborted {
        () => {{
            if let Some(sig) = signal.as_ref()
                && sig.is_cancelled()
            {
                let aborted = aborted_message(&partial);
                let _ = tx
                    .send(AssistantMessageEvent::Error {
                        reason: StopReason::Aborted,
                        error: aborted,
                    })
                    .await;
                return;
            }
        }};
    }

    check_aborted!();

    for step in script {
        check_aborted!();
        match step {
            FauxScriptStep::StartWithPartial(mutator) => {
                if !started {
                    mutator(&mut partial);
                    started = true;
                    send!(AssistantMessageEvent::Start {
                        partial: partial.clone(),
                    });
                }
            }
            FauxScriptStep::TextDelta(chunk) => {
                ensure_started!();
                let idx = match current_text_idx {
                    Some(i) => i,
                    None => {
                        // Close any open thinking block first.
                        if let Some(ti) = current_thinking_idx.take() {
                            let content = match partial.content.get(ti as usize) {
                                Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
                                _ => String::new(),
                            };
                            send!(AssistantMessageEvent::ThinkingEnd {
                                content_index: ti,
                                content,
                                partial: partial.clone(),
                            });
                        }
                        let i = partial.content.len() as u32;
                        partial
                            .content
                            .push(AssistantContentBlock::Text(TextContent::new("")));
                        current_text_idx = Some(i);
                        send!(AssistantMessageEvent::TextStart {
                            content_index: i,
                            partial: partial.clone(),
                        });
                        i
                    }
                };
                if let Some(AssistantContentBlock::Text(t)) = partial.content.get_mut(idx as usize)
                {
                    t.text.push_str(&chunk);
                }
                send!(AssistantMessageEvent::TextDelta {
                    content_index: idx,
                    delta: chunk,
                    partial: partial.clone(),
                });
            }
            FauxScriptStep::ThinkingDelta(chunk) => {
                ensure_started!();
                let idx = match current_thinking_idx {
                    Some(i) => i,
                    None => {
                        // Close any open text block first.
                        if let Some(ti) = current_text_idx.take() {
                            let content = match partial.content.get(ti as usize) {
                                Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                                _ => String::new(),
                            };
                            send!(AssistantMessageEvent::TextEnd {
                                content_index: ti,
                                content,
                                partial: partial.clone(),
                            });
                        }
                        let i = partial.content.len() as u32;
                        partial
                            .content
                            .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                        current_thinking_idx = Some(i);
                        send!(AssistantMessageEvent::ThinkingStart {
                            content_index: i,
                            partial: partial.clone(),
                        });
                        i
                    }
                };
                if let Some(AssistantContentBlock::Thinking(t)) =
                    partial.content.get_mut(idx as usize)
                {
                    t.thinking.push_str(&chunk);
                }
                send!(AssistantMessageEvent::ThinkingDelta {
                    content_index: idx,
                    delta: chunk,
                    partial: partial.clone(),
                });
            }
            FauxScriptStep::ToolCall {
                id,
                name,
                arguments,
            } => {
                ensure_started!();
                // Close any open text/thinking block first.
                if let Some(ti) = current_text_idx.take() {
                    let content = match partial.content.get(ti as usize) {
                        Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                        _ => String::new(),
                    };
                    send!(AssistantMessageEvent::TextEnd {
                        content_index: ti,
                        content,
                        partial: partial.clone(),
                    });
                }
                if let Some(ti) = current_thinking_idx.take() {
                    let content = match partial.content.get(ti as usize) {
                        Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
                        _ => String::new(),
                    };
                    send!(AssistantMessageEvent::ThinkingEnd {
                        content_index: ti,
                        content,
                        partial: partial.clone(),
                    });
                }

                let i = partial.content.len() as u32;
                let tc = ToolCall::new(id, name, arguments);
                partial
                    .content
                    .push(AssistantContentBlock::ToolCall(tc.clone()));
                send!(AssistantMessageEvent::ToolCallStart {
                    content_index: i,
                    partial: partial.clone(),
                });
                let serialized = serde_json::to_string(&tc.arguments).unwrap_or_default();
                send!(AssistantMessageEvent::ToolCallDelta {
                    content_index: i,
                    delta: serialized,
                    partial: partial.clone(),
                });
                send!(AssistantMessageEvent::ToolCallEnd {
                    content_index: i,
                    tool_call: tc,
                    partial: partial.clone(),
                });
            }
            FauxScriptStep::Sleep(ms) => {
                if let Some(sig) = signal.as_ref() {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {}
                        _ = sig.cancelled() => {
                            let aborted = aborted_message(&partial);
                            let _ = tx.send(AssistantMessageEvent::Error {
                                reason: StopReason::Aborted,
                                error: aborted,
                            }).await;
                            return;
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
            }
            FauxScriptStep::Done(reason, usage) => {
                ensure_started!();
                // Close any open text/thinking block first.
                if let Some(ti) = current_text_idx.take() {
                    let content = match partial.content.get(ti as usize) {
                        Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                        _ => String::new(),
                    };
                    send!(AssistantMessageEvent::TextEnd {
                        content_index: ti,
                        content,
                        partial: partial.clone(),
                    });
                }
                if let Some(ti) = current_thinking_idx.take() {
                    let content = match partial.content.get(ti as usize) {
                        Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
                        _ => String::new(),
                    };
                    send!(AssistantMessageEvent::ThinkingEnd {
                        content_index: ti,
                        content,
                        partial: partial.clone(),
                    });
                }

                let mut final_msg = partial.clone();
                final_msg.stop_reason = reason;
                final_msg.usage = usage;
                send!(AssistantMessageEvent::Done {
                    reason,
                    message: final_msg,
                });
                return;
            }
            FauxScriptStep::Error(msg) => {
                ensure_started!();
                let mut err = partial.clone();
                err.stop_reason = StopReason::Error;
                err.error_message = Some(msg);
                send!(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: err,
                });
                return;
            }
        }
    }
    // Script ended without an explicit terminal event. The EventStream
    // wrapper handles this by synthesizing an Aborted message on collection,
    // so we simply drop the sender here.
}
