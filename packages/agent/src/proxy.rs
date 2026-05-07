//! Proxy transport for routing LLM calls through a server that holds provider
//! credentials.
//!
//! The proxy forwards model requests to an HTTP endpoint that performs the
//! actual provider authentication, and streams events back to the client. As
//! part of that streaming, it strips the `partial` field from delta events so
//! that downstream consumers see a normalized event shape.
//!
//! Mirrors the TypeScript implementation at
//! `pi-mono/packages/agent/src/proxy.ts`.
//!
//! Reducer and request types are in place; the streaming driver `stream_proxy`
//! lands in T5 of the agent-proxy port (see
//! `docs/exec-plans/agent-proxy-port.md`).

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::AgentError;

/// Current wall-clock time as Unix epoch milliseconds. Used to seed
/// `partial.timestamp` once at the start of a proxy stream; mirrors TS'
/// `Date.now()` at `proxy.ts:136`.
fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Build the seed `AssistantMessage` that the proxy stream's reducer mutates
/// in place. Mirrors `proxy.ts:121-137`.
fn seed_partial(model: &model::Model) -> model::AssistantMessage {
    model::AssistantMessage {
        role: "assistant".to_string(),
        content: Vec::new(),
        api: model.api,
        provider: model.provider,
        model: model.id.clone(),
        usage: model::Usage::default(),
        stop_reason: model::StopReason::Stop,
        error_message: None,
        timestamp: current_timestamp_ms(),
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

/// Format a `reqwest::Error` without the trailing `" for url (...)"` suffix
/// that `Display` appends. The proxy URL is internal infrastructure and
/// shouldn't surface in user-visible `error_message` strings.
fn strip_url_from_reqwest_err(e: &reqwest::Error) -> String {
    let s = e.to_string();
    match s.rfind(" for url (") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Wire-format event from a proxy server. The proxy strips the `partial`
/// field from `AssistantMessageEvent` to save bandwidth; the client
/// reconstructs `partial` locally (see `process_proxy_event` in T4).
///
/// Mirrors `ProxyAssistantMessageEvent` from
/// `pi-mono/packages/agent/src/proxy.ts:36-57`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxyAssistantMessageEvent {
    Start {},
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    ToolcallStart {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    ToolcallDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
    },
    ToolcallEnd {
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    Done {
        reason: model::StopReason,
        usage: model::Usage,
    },
    Error {
        reason: model::StopReason,
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        usage: model::Usage,
    },
}

/// Bandwidth-trimmed subset of [`model::SimpleStreamOptions`] sent on the wire
/// under `body.options`. All fields are optional; only fields the caller set
/// are emitted. Field names use camelCase because the proxy server is
/// implemented in TypeScript.
///
/// Mirrors the TS `Pick<SimpleStreamOptions, ...>` at
/// `pi-mono/packages/agent/src/proxy.ts:59-71`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyRequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<model::ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_retention: Option<model::CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<model::Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budgets: Option<model::ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_retry_delay_ms: Option<u64>,
}

/// Caller-facing options for [`stream_proxy`].
///
/// The `auth_token` and `proxy_url` are required; the rest is a full
/// `SimpleStreamOptions` whose proxy-relevant subset is forwarded to the
/// server. Cancellation is honored if `cancel` is set.
#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    /// Bearer token sent as `Authorization: Bearer <auth_token>`.
    pub auth_token: String,
    /// Server base URL, e.g. `"https://genai.example.com"`. The proxy stream
    /// is fetched from `{proxy_url}/api/stream`.
    pub proxy_url: String,
    /// Optional cancellation token. Cancelling ends the stream with a
    /// synthesized `Error { reason: Aborted, .. }` event.
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    /// Full base options. Only the proxy-serializable subset is forwarded.
    pub options: model::SimpleStreamOptions,
}

/// JSON body of `POST {proxy_url}/api/stream`. Three top-level keys exactly:
/// `model`, `context`, `options` (TS shape at `proxy.ts:158-162`).
#[derive(Serialize)]
struct ProxyRequest<'a> {
    model: &'a model::Model,
    context: &'a model::Context,
    options: ProxyRequestOptions,
}

/// Project a [`model::SimpleStreamOptions`] into the bandwidth-trimmed
/// [`ProxyRequestOptions`] forwarded to the proxy server.
fn build_request_options(opts: &model::SimpleStreamOptions) -> ProxyRequestOptions {
    ProxyRequestOptions {
        temperature: opts.base.temperature,
        max_tokens: opts.base.max_tokens,
        reasoning: opts.reasoning,
        cache_retention: opts.base.cache_retention,
        session_id: opts.base.session_id.clone(),
        headers: opts.base.headers.clone(),
        metadata: opts.base.metadata.clone(),
        transport: opts.base.transport,
        thinking_budgets: opts.thinking_budgets.clone(),
        max_retry_delay_ms: opts.base.max_retry_delay_ms,
    }
}

/// Pure reducer that applies a wire-format [`ProxyAssistantMessageEvent`] to
/// the running [`model::AssistantMessage`] and a per-tool streaming-JSON
/// buffer, producing the corresponding [`model::AssistantMessageEvent`].
///
/// No I/O. Mirrors `processProxyEvent` at
/// `pi-mono/packages/agent/src/proxy.ts:238-367`.
///
/// `tool_partial_json` accumulates the raw JSON fragments per `content_index`
/// while a tool call is being streamed; the buffer is removed when the tool
/// call ends. The TS implementation hides this on the `ToolCall` block via
/// `partialJson`; the Rust port keeps it as an explicit side channel so the
/// `model::ToolCall` type stays clean.
///
/// Wrong-shape transitions surface as `Err(AgentError::Proxy { status: 0, .. })`
/// (TS would `throw`), with the single exception of `toolcall_end` on a
/// non-tool-call slot which returns `Ok(None)` to mirror TS' `return undefined`
/// at `proxy.ts:347`.
fn process_proxy_event(
    event: ProxyAssistantMessageEvent,
    partial: &mut model::AssistantMessage,
    tool_partial_json: &mut HashMap<u32, String>,
) -> Result<Option<model::AssistantMessageEvent>, AgentError> {
    use model::{AssistantContentBlock, AssistantMessageEvent, TextContent, ThinkingContent, ToolCall};

    /// Insert at `idx` into `vec`, growing by one if `idx == len`. Existing
    /// slots are replaced. The proxy server emits `*_start` events in order,
    /// so we never observe `idx > len`; if it ever does, we treat it as a
    /// shape error rather than padding silently.
    fn place<T>(vec: &mut Vec<T>, idx: usize, value: T) -> Result<(), AgentError> {
        if idx == vec.len() {
            vec.push(value);
            Ok(())
        } else if idx < vec.len() {
            vec[idx] = value;
            Ok(())
        } else {
            Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Proxy emitted content at index {idx} but only {} slots exist",
                    vec.len()
                ),
            })
        }
    }

    match event {
        ProxyAssistantMessageEvent::Start {} => Ok(Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        })),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            place(
                &mut partial.content,
                content_index as usize,
                AssistantContentBlock::Text(TextContent::new("")),
            )?;
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }))
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => match partial.content.get_mut(content_index as usize) {
            Some(AssistantContentBlock::Text(text)) => {
                text.text.push_str(&delta);
                Ok(Some(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            }
            _ => Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Received text_delta for non-text content at index {content_index}"
                ),
            }),
        },

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => match partial.content.get_mut(content_index as usize) {
            Some(AssistantContentBlock::Text(text)) => {
                text.text_signature = content_signature;
                let content = text.text.clone();
                Ok(Some(AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    partial: partial.clone(),
                }))
            }
            _ => Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Received text_end for non-text content at index {content_index}"
                ),
            }),
        },

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            place(
                &mut partial.content,
                content_index as usize,
                AssistantContentBlock::Thinking(ThinkingContent::new("")),
            )?;
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => match partial.content.get_mut(content_index as usize) {
            Some(AssistantContentBlock::Thinking(thinking)) => {
                thinking.thinking.push_str(&delta);
                Ok(Some(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            }
            _ => Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Received thinking_delta for non-thinking content at index {content_index}"
                ),
            }),
        },

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => match partial.content.get_mut(content_index as usize) {
            Some(AssistantContentBlock::Thinking(thinking)) => {
                thinking.thinking_signature = content_signature;
                let content = thinking.thinking.clone();
                Ok(Some(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: partial.clone(),
                }))
            }
            _ => Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Received thinking_end for non-thinking content at index {content_index}"
                ),
            }),
        },

        ProxyAssistantMessageEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
        } => {
            place(
                &mut partial.content,
                content_index as usize,
                AssistantContentBlock::ToolCall(ToolCall::new(
                    id,
                    tool_name,
                    serde_json::Value::Object(Default::default()),
                )),
            )?;
            tool_partial_json.insert(content_index, String::new());
            Ok(Some(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial.clone(),
            }))
        }

        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
        } => match partial.content.get_mut(content_index as usize) {
            Some(AssistantContentBlock::ToolCall(tc)) => {
                let buffer = tool_partial_json.entry(content_index).or_default();
                buffer.push_str(&delta);
                if let Some(parsed) = model::safe_parse_partial(buffer) {
                    tc.arguments = parsed;
                }
                // On parse failure: leave `tc.arguments` at the last good
                // value. `safe_parse_partial` already tolerates dangling
                // commas/strings, so failures here mean the buffer is
                // genuinely invalid.
                Ok(Some(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            }
            _ => Err(AgentError::Proxy {
                status: 0,
                message: format!(
                    "Received toolcall_delta for non-tool-call content at index {content_index}"
                ),
            }),
        },

        ProxyAssistantMessageEvent::ToolcallEnd { content_index } => {
            match partial.content.get(content_index as usize) {
                Some(AssistantContentBlock::ToolCall(tc)) => {
                    let tool_call = tc.clone();
                    tool_partial_json.remove(&content_index);
                    Ok(Some(AssistantMessageEvent::ToolCallEnd {
                        content_index,
                        tool_call,
                        partial: partial.clone(),
                    }))
                }
                // TS swallows this case (`return undefined;`) — mirror that.
                _ => Ok(None),
            }
        }

        ProxyAssistantMessageEvent::Done { reason, usage } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            Ok(Some(AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
        } => {
            partial.stop_reason = reason;
            partial.error_message = error_message;
            partial.usage = usage;
            Ok(Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

/// Open a streaming proxy request and yield translated
/// [`model::AssistantMessageEvent`]s.
///
/// `POST {proxy_url}/api/stream` is sent with the model, context, and a
/// bandwidth-trimmed projection of `options.options`. The server replies with
/// an SSE-style stream of `data: <json>` lines, each carrying a
/// [`ProxyAssistantMessageEvent`]. Lines are line-buffered, parsed, fed
/// through `process_proxy_event`, and the resulting events are yielded.
///
/// This function is synchronous: it returns the boxed stream immediately, and
/// the HTTP request is initiated on the first poll.
///
/// On any failure — network error, non-2xx status, malformed event,
/// reducer-detected protocol violation — the stream yields exactly one
/// `AssistantMessageEvent::Error` and ends. On cancellation via
/// `options.cancel`, an `Error { reason: Aborted, .. }` is yielded.
///
/// Mirrors `streamProxy` at `pi-mono/packages/agent/src/proxy.ts:116-233`.
pub fn stream_proxy(
    model: &model::Model,
    context: model::Context,
    options: ProxyStreamOptions,
) -> model::AssistantMessageEventStream<'static> {
    let model = model.clone();
    let cancel = options.cancel.clone();
    let auth_token = options.auth_token.clone();
    let proxy_url = options.proxy_url.clone();
    let request_options = build_request_options(&options.options);

    let s = async_stream::stream! {
        let mut partial = seed_partial(&model);

        let url = format!("{}/api/stream", proxy_url);
        let body = ProxyRequest {
            model: &model,
            context: &context,
            options: request_options,
        };

        let client = reqwest::Client::new();
        let send_fut = client
            .post(&url)
            .bearer_auth(&auth_token)
            .json(&body)
            .send();

        // Phase 1: send request, race against cancellation.
        let response = if let Some(cancel_tok) = cancel.as_ref() {
            tokio::select! {
                _ = cancel_tok.cancelled() => {
                    partial.stop_reason = model::StopReason::Aborted;
                    partial.error_message = Some("Aborted".to_string());
                    yield model::AssistantMessageEvent::Error {
                        reason: model::StopReason::Aborted,
                        error: partial.clone(),
                    };
                    return;
                }
                result = send_fut => match result {
                    Ok(resp) => resp,
                    Err(e) => {
                        partial.stop_reason = model::StopReason::Error;
                        partial.error_message = Some(format!("Proxy error: {}", strip_url_from_reqwest_err(&e)));
                        yield model::AssistantMessageEvent::Error {
                            reason: model::StopReason::Error,
                            error: partial.clone(),
                        };
                        return;
                    }
                },
            }
        } else {
            match send_fut.await {
                Ok(resp) => resp,
                Err(e) => {
                    partial.stop_reason = model::StopReason::Error;
                    partial.error_message = Some(format!("Proxy error: {}", strip_url_from_reqwest_err(&e)));
                    yield model::AssistantMessageEvent::Error {
                        reason: model::StopReason::Error,
                        error: partial.clone(),
                    };
                    return;
                }
            }
        };

        // Phase 2: non-2xx → drain body, surface error, end.
        if !response.status().is_success() {
            let status = response.status();
            let status_text = status.canonical_reason().unwrap_or("");
            let mut msg = format!("Proxy error: {} {}", status.as_u16(), status_text);

            if let Ok(v) = response.json::<serde_json::Value>().await
                && let Some(err_str) = v.get("error").and_then(|e| e.as_str())
            {
                msg = format!("Proxy error: {err_str}");
            }

            partial.stop_reason = model::StopReason::Error;
            partial.error_message = Some(msg);
            yield model::AssistantMessageEvent::Error {
                reason: model::StopReason::Error,
                error: partial.clone(),
            };
            return;
        }

        // Phase 3: SSE body — line-buffer, parse, dispatch.
        let mut bytes_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut tool_partial_json: HashMap<u32, String> = HashMap::new();

        loop {
            let next_chunk = if let Some(cancel_tok) = cancel.as_ref() {
                tokio::select! {
                    _ = cancel_tok.cancelled() => {
                        partial.stop_reason = model::StopReason::Aborted;
                        partial.error_message = Some("Aborted".to_string());
                        yield model::AssistantMessageEvent::Error {
                            reason: model::StopReason::Aborted,
                            error: partial.clone(),
                        };
                        return;
                    }
                    chunk = bytes_stream.next() => chunk,
                }
            } else {
                bytes_stream.next().await
            };

            let bytes = match next_chunk {
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    partial.stop_reason = model::StopReason::Error;
                    partial.error_message = Some(format!("Proxy error: {}", strip_url_from_reqwest_err(&e)));
                    yield model::AssistantMessageEvent::Error {
                        reason: model::StopReason::Error,
                        error: partial.clone(),
                    };
                    return;
                }
                None => break,
            };

            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(newline_pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_pos).collect();
                let line = line.trim_end_matches(['\r', '\n']);
                let trimmed = line.trim();

                if trimmed.is_empty() || trimmed.starts_with(':') {
                    continue;
                }
                let payload = match trimmed.strip_prefix("data: ") {
                    Some(p) => p,
                    None => continue,
                };
                if payload.is_empty() {
                    continue;
                }

                let proxy_event: ProxyAssistantMessageEvent = match serde_json::from_str(payload) {
                    Ok(ev) => ev,
                    Err(e) => {
                        partial.stop_reason = model::StopReason::Error;
                        partial.error_message = Some(format!("Proxy error: {e}"));
                        yield model::AssistantMessageEvent::Error {
                            reason: model::StopReason::Error,
                            error: partial.clone(),
                        };
                        return;
                    }
                };

                match process_proxy_event(proxy_event, &mut partial, &mut tool_partial_json) {
                    Ok(Some(ev)) => yield ev,
                    Ok(None) => {}
                    Err(e) => {
                        partial.stop_reason = model::StopReason::Error;
                        partial.error_message = Some(e.to_string());
                        yield model::AssistantMessageEvent::Error {
                            reason: model::StopReason::Error,
                            error: partial.clone(),
                        };
                        return;
                    }
                }
            }
        }
    };

    Box::pin(s)
}

/// Build a [`StreamFn`](crate::types::StreamFn) that bridges the proxy
/// transport into [`AgentOptions`](crate::AgentOptions).
///
/// The `template` carries the auth token and proxy URL; the loop's
/// `SimpleStreamOptions` for each turn is copied into
/// `template.options` per call, and the loop's cancellation token
/// replaces `template.cancel`. Other fields (auth, URL) come from
/// the template.
///
/// # Example
///
/// ```ignore
/// use hand_agent::{Agent, AgentOptions, ProxyStreamOptions, stream_fn_proxy};
///
/// let stream_fn = stream_fn_proxy(ProxyStreamOptions {
///     auth_token: my_token,
///     proxy_url: "https://genai.example.com".into(),
///     ..Default::default()
/// });
/// let agent = Agent::with_options(client, model, AgentOptions {
///     stream_fn: Some(stream_fn),
///     ..Default::default()
/// });
/// ```
pub fn stream_fn_proxy(template: ProxyStreamOptions) -> crate::types::StreamFn {
    Arc::new(move |model, context, simple_opts, cancel| {
        let mut opts = template.clone();
        opts.options = simple_opts;
        opts.cancel = Some(cancel);
        stream_proxy(model, context, opts)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `model::Usage` does not derive `PartialEq`, so we cannot derive
    /// `PartialEq` on `ProxyAssistantMessageEvent` without modifying the
    /// `model` crate. Instead, each round-trip test compares the JSON
    /// produced by serializing the original value with the JSON produced by
    /// re-serializing the deserialized value. If the wire format is stable
    /// under round-trip, those JSON strings must be identical.
    fn assert_round_trip(value: &ProxyAssistantMessageEvent) -> String {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: ProxyAssistantMessageEvent =
            serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&parsed).expect("re-serialize");
        assert_eq!(json, json2, "round-trip JSON mismatch");
        json
    }

    fn sample_usage() -> model::Usage {
        model::Usage {
            input: 10,
            output: 20,
            cache_read: 1,
            cache_write: 2,
            total_tokens: 33,
            cost: model::UsageCost::default(),
        }
    }

    #[test]
    fn round_trip_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Start {});
        assert!(json.contains(r#""type":"start""#), "json={json}");
    }

    #[test]
    fn round_trip_text_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextStart { content_index: 0 });
        assert!(json.contains(r#""type":"text_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":0"#), "json={json}");
    }

    #[test]
    fn round_trip_text_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextDelta {
            content_index: 1,
            delta: "hello".into(),
        });
        assert!(json.contains(r#""type":"text_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":1"#), "json={json}");
        assert!(json.contains(r#""delta":"hello""#), "json={json}");
    }

    #[test]
    fn round_trip_text_end() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextEnd {
            content_index: 2,
            content_signature: Some("sig".into()),
        });
        assert!(json.contains(r#""type":"text_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":2"#), "json={json}");
        assert!(json.contains(r#""contentSignature":"sig""#), "json={json}");

        // None should be omitted.
        let json = assert_round_trip(&ProxyAssistantMessageEvent::TextEnd {
            content_index: 2,
            content_signature: None,
        });
        assert!(!json.contains("contentSignature"), "json={json}");
    }

    #[test]
    fn round_trip_thinking_start() {
        let json =
            assert_round_trip(&ProxyAssistantMessageEvent::ThinkingStart { content_index: 3 });
        assert!(json.contains(r#""type":"thinking_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":3"#), "json={json}");
    }

    #[test]
    fn round_trip_thinking_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 4,
            delta: "thinking...".into(),
        });
        assert!(json.contains(r#""type":"thinking_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":4"#), "json={json}");
        assert!(json.contains(r#""delta":"thinking...""#), "json={json}");
    }

    #[test]
    fn round_trip_thinking_end() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 5,
            content_signature: Some("tsig".into()),
        });
        assert!(json.contains(r#""type":"thinking_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":5"#), "json={json}");
        assert!(json.contains(r#""contentSignature":"tsig""#), "json={json}");

        let json = assert_round_trip(&ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 5,
            content_signature: None,
        });
        assert!(!json.contains("contentSignature"), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_start() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 6,
            id: "call_1".into(),
            tool_name: "search".into(),
        });
        assert!(json.contains(r#""type":"toolcall_start""#), "json={json}");
        assert!(json.contains(r#""contentIndex":6"#), "json={json}");
        assert!(json.contains(r#""id":"call_1""#), "json={json}");
        assert!(json.contains(r#""toolName":"search""#), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_delta() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 7,
            delta: "{\"q\":".into(),
        });
        assert!(json.contains(r#""type":"toolcall_delta""#), "json={json}");
        assert!(json.contains(r#""contentIndex":7"#), "json={json}");
        assert!(json.contains(r#""delta":"#), "json={json}");
    }

    #[test]
    fn round_trip_toolcall_end() {
        let json =
            assert_round_trip(&ProxyAssistantMessageEvent::ToolcallEnd { content_index: 8 });
        assert!(json.contains(r#""type":"toolcall_end""#), "json={json}");
        assert!(json.contains(r#""contentIndex":8"#), "json={json}");
    }

    #[test]
    fn round_trip_done() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Done {
            reason: model::StopReason::Stop,
            usage: sample_usage(),
        });
        assert!(json.contains(r#""type":"done""#), "json={json}");
        assert!(json.contains(r#""reason":"stop""#), "json={json}");
        assert!(json.contains(r#""usage":"#), "json={json}");
    }

    #[test]
    fn round_trip_error() {
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Error {
            reason: model::StopReason::Error,
            error_message: Some("boom".into()),
            usage: sample_usage(),
        });
        assert!(json.contains(r#""type":"error""#), "json={json}");
        assert!(json.contains(r#""reason":"error""#), "json={json}");
        assert!(json.contains(r#""errorMessage":"boom""#), "json={json}");
        assert!(json.contains(r#""usage":"#), "json={json}");

        // Missing errorMessage should be omitted on serialization.
        let json = assert_round_trip(&ProxyAssistantMessageEvent::Error {
            reason: model::StopReason::Error,
            error_message: None,
            usage: sample_usage(),
        });
        assert!(!json.contains("errorMessage"), "json={json}");
    }

    #[test]
    fn rejects_unknown_field() {
        let result = serde_json::from_str::<ProxyAssistantMessageEvent>(
            r#"{"type":"start","extra":"x"}"#,
        );
        assert!(
            result.is_err(),
            "deny_unknown_fields should reject extra fields, got: {result:?}"
        );
    }

    #[test]
    fn rejects_unknown_field_struct_variant() {
        let result = serde_json::from_str::<ProxyAssistantMessageEvent>(
            r#"{"type":"text_start","contentIndex":0,"extra":"x"}"#,
        );
        assert!(
            result.is_err(),
            "deny_unknown_fields should reject extra fields on struct variant, got: {result:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Request-side type tests (T3).
    // ---------------------------------------------------------------------

    fn test_model() -> model::Model {
        model::Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: model::Api::OpenAICompletions,
            provider: model::types::Provider::OpenAI,
            base_url: "https://api.test.com".into(),
            reasoning: false,
            input: vec![model::InputType::Text],
            cost: model::Cost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.5,
                cache_write: 0.75,
            },
            context_window: 128_000,
            max_tokens: 4096,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn request_body_top_level_keys() {
        let model = test_model();
        let context = model::Context::default();
        let request = ProxyRequest {
            model: &model,
            context: &context,
            options: ProxyRequestOptions::default(),
        };

        let json = serde_json::to_string(&request).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = value.as_object().expect("top-level object");

        assert_eq!(obj.len(), 3, "expected exactly 3 top-level keys, got {obj:?}");
        assert!(obj.contains_key("model"), "missing `model` key in {obj:?}");
        assert!(obj.contains_key("context"), "missing `context` key in {obj:?}");
        assert!(obj.contains_key("options"), "missing `options` key in {obj:?}");
    }

    #[test]
    fn request_options_omits_unset_fields() {
        let only_temp = ProxyRequestOptions {
            temperature: Some(0.5),
            ..Default::default()
        };
        let json = serde_json::to_string(&only_temp).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = value.as_object().expect("object");
        assert_eq!(obj.len(), 1, "expected 1 key, got {obj:?}");
        assert!(obj.contains_key("temperature"), "missing temperature in {obj:?}");

        let temp_and_max = ProxyRequestOptions {
            temperature: Some(0.5),
            max_tokens: Some(1024),
            ..Default::default()
        };
        let json = serde_json::to_string(&temp_and_max).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = value.as_object().expect("object");
        assert_eq!(obj.len(), 2, "expected 2 keys, got {obj:?}");
        assert!(obj.contains_key("temperature"), "missing temperature in {obj:?}");
        assert!(obj.contains_key("maxTokens"), "missing maxTokens in {obj:?}");
    }

    #[test]
    fn build_request_options_projects_simple_stream_options() {
        let simple = model::SimpleStreamOptions {
            base: model::StreamOptions {
                temperature: Some(0.7),
                max_tokens: Some(2048),
                ..Default::default()
            },
            reasoning: Some(model::ThinkingLevel::Medium),
            thinking_budgets: None,
        };

        let projected = build_request_options(&simple);
        let json = serde_json::to_string(&projected).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = value.as_object().expect("object");

        assert!(obj.contains_key("temperature"), "missing temperature in {obj:?}");
        assert!(obj.contains_key("maxTokens"), "missing maxTokens in {obj:?}");
        assert!(obj.contains_key("reasoning"), "missing reasoning in {obj:?}");
        assert!(
            !obj.contains_key("cacheRetention"),
            "cacheRetention should be absent in {obj:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Reducer tests (T4): `process_proxy_event`.
    // ---------------------------------------------------------------------

    /// Build a fresh `AssistantMessage` analogous to TS' seed at
    /// `pi-mono/packages/agent/src/proxy.ts:121-137`. Tests don't depend on
    /// `timestamp`, so `0` is fine.
    fn fresh_partial(model: &model::Model) -> model::AssistantMessage {
        model::AssistantMessage {
            role: "assistant".to_string(),
            content: Vec::new(),
            api: model.api,
            provider: model.provider,
            model: model.id.clone(),
            usage: model::Usage::default(),
            stop_reason: model::StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn proxy_status(err: &AgentError) -> u16 {
        match err {
            AgentError::Proxy { status, .. } => *status,
            other => panic!("expected AgentError::Proxy, got {other:?}"),
        }
    }

    #[test]
    fn reducer_start_emits_start_with_cloned_partial() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let out = process_proxy_event(ProxyAssistantMessageEvent::Start {}, &mut partial, &mut buf)
            .expect("ok")
            .expect("some");

        match out {
            model::AssistantMessageEvent::Start { partial: p } => {
                assert!(p.content.is_empty(), "fresh partial should have no content");
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn reducer_text_start_inserts_empty_text_block() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::TextStart { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        assert!(matches!(out, model::AssistantMessageEvent::TextStart { content_index: 0, .. }));
        match &partial.content[0] {
            model::AssistantContentBlock::Text(t) => assert_eq!(t.text, ""),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn reducer_text_delta_appends_text() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("hi")));
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: " there".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                ..
            } => {
                assert_eq!(content_index, 0);
                assert_eq!(delta, " there");
            }
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &partial.content[0] {
            model::AssistantContentBlock::Text(t) => assert_eq!(t.text, "hi there"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn reducer_text_end_sets_signature_and_returns_text() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("done")));
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 0,
                content_signature: Some("sig".into()),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::TextEnd {
                content_index,
                content,
                ..
            } => {
                assert_eq!(content_index, 0);
                assert_eq!(content, "done");
            }
            other => panic!("expected TextEnd, got {other:?}"),
        }
        match &partial.content[0] {
            model::AssistantContentBlock::Text(t) => {
                assert_eq!(t.text_signature.as_deref(), Some("sig"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn reducer_thinking_start_inserts_empty_thinking_block() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ThinkingStart { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        assert!(matches!(
            out,
            model::AssistantMessageEvent::ThinkingStart { content_index: 0, .. }
        ));
        match &partial.content[0] {
            model::AssistantContentBlock::Thinking(t) => assert_eq!(t.thinking, ""),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn reducer_thinking_delta_appends_thinking() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Thinking(
                model::ThinkingContent::new("ponder"),
            ));
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "ing".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        assert!(matches!(out, model::AssistantMessageEvent::ThinkingDelta { .. }));
        match &partial.content[0] {
            model::AssistantContentBlock::Thinking(t) => assert_eq!(t.thinking, "pondering"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn reducer_thinking_end_sets_signature() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Thinking(
                model::ThinkingContent::new("done"),
            ));
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content_signature: Some("tsig".into()),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::ThinkingEnd { content, .. } => {
                assert_eq!(content, "done");
            }
            other => panic!("expected ThinkingEnd, got {other:?}"),
        }
        match &partial.content[0] {
            model::AssistantContentBlock::Thinking(t) => {
                assert_eq!(t.thinking_signature.as_deref(), Some("tsig"));
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn reducer_toolcall_start_inserts_block_and_seeds_buffer() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallStart {
                content_index: 0,
                id: "tc1".into(),
                tool_name: "echo".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        assert!(matches!(
            out,
            model::AssistantMessageEvent::ToolCallStart { content_index: 0, .. }
        ));
        match &partial.content[0] {
            model::AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.id, "tc1");
                assert_eq!(tc.name, "echo");
                assert_eq!(tc.arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        assert_eq!(buf.get(&0).map(String::as_str), Some(""));
    }

    #[test]
    fn reducer_toolcall_delta_parses_partial_json_into_arguments() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::ToolCall(model::ToolCall::new(
                "tc1",
                "echo",
                serde_json::json!({}),
            )));
        let mut buf = HashMap::new();
        buf.insert(0, String::new());

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: r#"{"x":1}"#.into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        assert!(matches!(out, model::AssistantMessageEvent::ToolCallDelta { .. }));
        match &partial.content[0] {
            model::AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.arguments, serde_json::json!({"x": 1}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        assert_eq!(buf.get(&0).map(String::as_str), Some(r#"{"x":1}"#));
    }

    #[test]
    fn reducer_toolcall_end_clears_buffer_and_returns_tool_call() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::ToolCall(model::ToolCall::new(
                "tc1",
                "echo",
                serde_json::json!({"x": 1}),
            )));
        let mut buf = HashMap::new();
        buf.insert(0, r#"{"x":1}"#.to_string());

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallEnd { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                ..
            } => {
                assert_eq!(content_index, 0);
                assert_eq!(tool_call.id, "tc1");
                assert_eq!(tool_call.arguments, serde_json::json!({"x": 1}));
            }
            other => panic!("expected ToolCallEnd, got {other:?}"),
        }
        assert!(!buf.contains_key(&0), "buffer entry should be removed");
    }

    #[test]
    fn reducer_done_sets_stop_reason_and_usage() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let usage = model::Usage {
            input: 5,
            output: 7,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 12,
            cost: model::UsageCost::default(),
        };

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::Done {
                reason: model::StopReason::ToolUse,
                usage: usage.clone(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::Done { reason, message } => {
                assert_eq!(reason, model::StopReason::ToolUse);
                assert_eq!(message.usage.total_tokens, 12);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(partial.stop_reason, model::StopReason::ToolUse);
        assert_eq!(partial.usage.input, 5);
    }

    #[test]
    fn reducer_error_sets_stop_reason_message_and_usage() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::Error {
                reason: model::StopReason::Error,
                error_message: Some("boom".into()),
                usage: model::Usage::default(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok")
        .expect("some");

        match out {
            model::AssistantMessageEvent::Error { reason, error } => {
                assert_eq!(reason, model::StopReason::Error);
                assert_eq!(error.error_message.as_deref(), Some("boom"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(partial.stop_reason, model::StopReason::Error);
        assert_eq!(partial.error_message.as_deref(), Some("boom"));
    }

    #[test]
    fn reducer_text_delta_on_thinking_slot_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Thinking(
                model::ThinkingContent::new("ponder"),
            ));
        let mut buf = HashMap::new();

        let err = process_proxy_event(
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "x".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect_err("expected Err for wrong-shape transition");

        assert_eq!(proxy_status(&err), 0);
    }

    #[test]
    fn reducer_text_end_on_thinking_slot_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Thinking(
                model::ThinkingContent::new("ponder"),
            ));
        let mut buf = HashMap::new();

        let err = process_proxy_event(
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 0,
                content_signature: None,
            },
            &mut partial,
            &mut buf,
        )
        .expect_err("expected Err for wrong-shape transition");

        assert_eq!(proxy_status(&err), 0);
    }

    #[test]
    fn reducer_thinking_delta_on_text_slot_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("hi")));
        let mut buf = HashMap::new();

        let err = process_proxy_event(
            ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "x".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect_err("expected Err for wrong-shape transition");

        assert_eq!(proxy_status(&err), 0);
    }

    #[test]
    fn reducer_thinking_end_on_text_slot_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("hi")));
        let mut buf = HashMap::new();

        let err = process_proxy_event(
            ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content_signature: None,
            },
            &mut partial,
            &mut buf,
        )
        .expect_err("expected Err for wrong-shape transition");

        assert_eq!(proxy_status(&err), 0);
    }

    #[test]
    fn reducer_toolcall_delta_on_text_slot_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("hi")));
        let mut buf = HashMap::new();

        let err = process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: r#"{"x":1}"#.into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect_err("expected Err for wrong-shape transition");

        assert_eq!(proxy_status(&err), 0);
    }

    #[test]
    fn reducer_text_start_at_idx_beyond_len_returns_proxy_error() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();
        let err = process_proxy_event(
            ProxyAssistantMessageEvent::TextStart { content_index: 5 },
            &mut partial,
            &mut buf,
        )
        .expect_err("idx > len must error");
        match err {
            AgentError::Proxy { status, message } => {
                assert_eq!(status, 0);
                assert!(
                    message.contains("5") || message.to_lowercase().contains("index"),
                    "unexpected message: {message}",
                );
            }
            other => panic!("expected AgentError::Proxy, got {other:?}"),
        }
    }

    #[test]
    fn text_round_trip() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        let r1 = process_proxy_event(
            ProxyAssistantMessageEvent::TextStart { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok");
        assert!(matches!(
            r1,
            Some(model::AssistantMessageEvent::TextStart { content_index: 0, .. })
        ));

        let r2 = process_proxy_event(
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: " hello".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");
        assert!(matches!(r2, Some(model::AssistantMessageEvent::TextDelta { .. })));

        let r3 = process_proxy_event(
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: " world".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");
        assert!(matches!(r3, Some(model::AssistantMessageEvent::TextDelta { .. })));

        let r4 = process_proxy_event(
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 0,
                content_signature: Some("sig".into()),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");
        assert!(matches!(r4, Some(model::AssistantMessageEvent::TextEnd { .. })));

        match &partial.content[0] {
            model::AssistantContentBlock::Text(t) => {
                assert_eq!(t.text, " hello world");
                assert_eq!(t.text_signature.as_deref(), Some("sig"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn toolcall_round_trip() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallStart {
                content_index: 0,
                id: "tc1".into(),
                tool_name: "echo".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: r#"{"x":"#.into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: "1}".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        match &partial.content[0] {
            model::AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.arguments, serde_json::json!({"x": 1}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallEnd { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        assert!(
            !buf.contains_key(&0),
            "buffer entry should be cleared after toolcall_end"
        );
    }

    #[test]
    fn toolcall_partial_json_keeps_prior_value_when_unparseable() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        let mut buf = HashMap::new();

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallStart {
                content_index: 0,
                id: "tc1".into(),
                tool_name: "echo".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: r#"{"x":1}"#.into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        // Sanity check: arguments parsed.
        match &partial.content[0] {
            model::AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.arguments, serde_json::json!({"x": 1}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // Now feed garbage that, when concatenated, no longer parses.
        process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: "garbage".into(),
            },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        // Arguments should still hold the last good value.
        match &partial.content[0] {
            model::AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(
                    tc.arguments,
                    serde_json::json!({"x": 1}),
                    "arguments should survive an unparseable buffer"
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn toolcall_end_on_wrong_slot_returns_none() {
        let m = test_model();
        let mut partial = fresh_partial(&m);
        partial
            .content
            .push(model::AssistantContentBlock::Text(model::TextContent::new("hi")));
        let mut buf = HashMap::new();

        let out = process_proxy_event(
            ProxyAssistantMessageEvent::ToolcallEnd { content_index: 0 },
            &mut partial,
            &mut buf,
        )
        .expect("ok");

        assert!(out.is_none(), "toolcall_end on non-tool-call slot should yield None");
    }
}
