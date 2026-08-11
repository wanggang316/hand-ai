//! OpenAI Completions API provider.
//!
//! Implements streaming chat completions for OpenAI-compatible APIs.

use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, InputType, Message,
    Model, Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    ThinkingLevel, Tool, ToolCall, UserContentBlock,
};
use crate::{env_api_keys, supports_xhigh};
use futures::StreamExt;
use openai_rust::client::Client;
use openai_rust::types::{
    CompletionRequest, Content, ContentPart, FunctionCall, ImageUrl, RequestMessage, Role,
    Tool as OpenAiTool, ToolCall as OpenAiToolCall, ToolChoice,
};
use serde_json::Value;
use std::collections::HashMap;

// Re-export from api_registry for convenience
pub use crate::api_registry::AssistantMessageEventStream;

/// Extended options for OpenAI Completions.
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<ToolChoice>,
    pub reasoning_effort: Option<openai_rust::types::ReasoningEffort>,
    /// Provider-native effort keyword from the model's thinking level map
    /// (e.g. a level the wire enum cannot express). Takes precedence over
    /// `reasoning_effort` at emission time; the typed field still signals
    /// that thinking is enabled for compat toggles.
    pub native_reasoning_effort: Option<String>,
}

impl OpenAICompletionsOptions {
    pub fn temperature(&self) -> Option<f32> {
        self.base.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.base.max_tokens
    }

    pub fn api_key(&self) -> Option<&str> {
        self.base.api_key.as_deref()
    }

    pub fn headers(&self) -> Option<&HashMap<String, String>> {
        self.base.headers.as_ref()
    }
}

// =============================================================================
// Provider Implementation
// =============================================================================

/// Provider implementation for OpenAI Completions API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAICompletionsProvider;

impl OpenAICompletionsProvider {
    /// Create a new OpenAI Completions provider.
    pub fn new() -> Self {
        Self
    }
}

impl crate::api_registry::ApiProvider for OpenAICompletionsProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> crate::api_registry::AssistantMessageEventStream<'static> {
        let opts = options.map(|base| OpenAICompletionsOptions {
            base,
            tool_choice: None,
            reasoning_effort: None,
            native_reasoning_effort: None,
        });
        stream_openai_completions(model, context, opts)
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> crate::api_registry::AssistantMessageEventStream<'static> {
        let api_key = options
            .as_ref()
            .and_then(|o| o.api_key().map(|s| s.to_string()))
            .or_else(|| env_api_keys::get_env_api_key(&model.provider));

        if api_key.is_none() {
            let error_msg = format!("No API key for provider: {:?}", model.provider);
            return make_error_stream(error_msg, model.id.clone(), model.provider);
        }

        let mut base = StreamOptions::default();
        if let Some(opts) = &options {
            base.temperature = opts.temperature();
            base.max_tokens = opts.max_tokens();
            base.api_key = api_key;
            base.headers = opts.headers().cloned();
            // Cancellation surface: the wrapper in `stream::stream_simple`
            // installs a combined token (user signal + timeout) into
            // `opts.base.signal` before calling into us. Forward it so
            // long-running SSE loops can be interrupted; previously this
            // field was silently dropped here.
            base.signal = opts.base.signal.clone();
            base.timeout_ms = opts.base.timeout_ms;
            base.max_retries = opts.base.max_retries;
            base.max_retry_delay_ms = opts.base.max_retry_delay_ms;
        }

        let reasoning_effort = if supports_xhigh(&model) {
            options
                .as_ref()
                .and_then(|o| o.reasoning.map(map_thinking_level))
        } else {
            options
                .as_ref()
                .and_then(|o| clamp_reasoning(o.reasoning).map(map_thinking_level))
        };
        let native_reasoning_effort =
            resolve_native_effort(&model, options.as_ref().and_then(|o| o.reasoning));

        let opts = OpenAICompletionsOptions {
            base,
            tool_choice: None,
            reasoning_effort,
            native_reasoning_effort,
        };

        stream_openai_completions(model, context, Some(opts))
    }
}

// =============================================================================
// Core Streaming Functions
// =============================================================================

fn make_error_stream(
    error_msg: String,
    model_id: String,
    provider: Provider,
) -> AssistantMessageEventStream<'static> {
    Box::pin(async_stream::stream! {
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: AssistantMessage {
                role: "assistant".to_string(),
                api: crate::types::Api::OpenAICompletions,
                provider,
                model: model_id,
                usage: crate::types::Usage::default(),
                stop_reason: StopReason::Error,
                error_message: Some(error_msg),
                timestamp: current_timestamp_ms(),
                content: vec![],
                response_model: None,
                response_id: None,
                diagnostics: None,
            },
        };
    })
}

/// Stream chat completions from an OpenAI-compatible API.
pub fn stream_openai_completions(
    model: Model,
    context: Context,
    options: Option<OpenAICompletionsOptions>,
) -> AssistantMessageEventStream<'static> {
    let options = options.unwrap_or_default();

    Box::pin(async_stream::stream! {
        let mut output = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: crate::types::Api::OpenAICompletions,
            provider: model.provider,
            model: model.id.clone(),
            usage: crate::types::Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 0,
                cost: crate::types::UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };

        yield AssistantMessageEvent::Start { partial: output.clone() };

        // Drive the HTTP/SSE stream inline so we can `yield` intermediate
        // `text_*`/`thinking_*`/`toolcall_*` events as deltas arrive. The
        // previous extraction into `run_stream(...)` made this impossible —
        // events were only emitted at Start/Done, and `current_block` was
        // dropped without ever flushing accumulated text/thinking back into
        // `output.content`, so any chunk past the first one was lost.
        let api_key = options
            .api_key()
            .map(|s| s.to_string())
            .or_else(|| env_api_keys::get_env_api_key(&model.provider))
            .unwrap_or_default();

        let client = match create_client(
            &model,
            &context,
            &api_key,
            options.headers(),
            options.base.session_id.as_deref(),
            options.base.cache_retention,
        ) {
            Ok(c) => c,
            Err(e) => {
                output.stop_reason = StopReason::Error;
                output.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        let params = match build_params(&model, &context, &options) {
            Ok(p) => p,
            Err(e) => {
                output.stop_reason = StopReason::Error;
                output.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        let completions = client.completions();
        let stream_result = completions.create_stream(&params).await;
        let mut chunk_stream = match stream_result {
            Ok(s) => Box::pin(s),
            Err(e) => {
                output.stop_reason = StopReason::Error;
                output.error_message = Some(e.to_string());
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        let mut current_block: Option<CurrentBlock> = None;
        let mut errored: Option<String> = None;
        let mut saw_finish_reason = false;

        while let Some(chunk_result) = chunk_stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => { errored = Some(e.to_string()); break; }
            };

            capture_chunk_metadata(&chunk.id, &chunk.model, &model.id, &mut output);

            // Terminal usage chunk: when the request opted into
            // `stream_options.include_usage = true`, the provider emits
            // a final chunk carrying the call's token counts —
            // typically with `choices: []`. Capture before the
            // no-choice short-circuit below so we don't drop it.
            if let Some(u) = &chunk.usage {
                apply_chunk_usage(u, &model, &mut output);
            }

            let Some(choice) = chunk.choices.first() else { continue };

            if let Some(finish_reason) = &choice.finish_reason {
                saw_finish_reason = true;
                output.stop_reason = map_stop_reason(finish_reason);
            }

            for ev in handle_delta(&choice.delta, &mut current_block, &mut output) {
                yield ev;
            }
        }

        for ev in finish_current_block(&mut current_block, &mut output) {
            yield ev;
        }

        output.stop_reason =
            resolve_stop_reason(saw_finish_reason, output.stop_reason, &output.content);

        if let Some(e) = errored {
            output.stop_reason = StopReason::Error;
            output.error_message = Some(e);
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
        } else {
            yield AssistantMessageEvent::Done {
                reason: output.stop_reason,
                message: output,
            };
        }
    })
}

// =============================================================================
// Stream Processing
// =============================================================================

/// Capture per-stream identifiers from a streaming chunk.
///
/// OpenAI documents `id` as the unique chat completion identifier and
/// every chunk in a stream repeats it; record it once. `model` is the
/// model that actually served the request — for OpenRouter's `auto`
/// route it differs from the requested id (e.g. `anthropic/...`), so
/// surface it as `response_model` only when it diverges so downstream
/// callers know the routing landed on a different concrete model.
fn capture_chunk_metadata(
    chunk_id: &str,
    chunk_model: &str,
    requested_id: &str,
    output: &mut AssistantMessage,
) {
    if output.response_id.is_none() && !chunk_id.is_empty() {
        output.response_id = Some(chunk_id.to_string());
    }
    if output.response_model.is_none() && !chunk_model.is_empty() && chunk_model != requested_id {
        output.response_model = Some(chunk_model.to_string());
    }
}

/// Write the OpenAI-shaped chunk usage into the assistant message
/// and compute the per-call cost.
///
/// The OpenAI Completions API and every compatible provider report
/// per-call totals on the terminal SSE chunk (requires the caller to
/// set `stream_options.include_usage = true`, which the build_params
/// path already does for providers with `supports_usage_in_streaming`).
/// `prompt_tokens` is the raw input including any cache hits; the
/// convention used by the other providers in this crate (Google,
/// Anthropic) is to surface the *billed* input separately from
/// cached reads, so subtract the cached count out of `input` and
/// surface it under `cache_read`. `saturating_sub` guards against
/// provider quirks where the cached count exceeds the prompt total.
fn apply_chunk_usage(
    chunk_usage: &openai_rust::types::Usage,
    model: &Model,
    output: &mut AssistantMessage,
) {
    let cached = chunk_usage
        .prompt_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens as u64)
        .unwrap_or(0);
    let prompt_total = chunk_usage.prompt_tokens as u64;
    output.usage.input = prompt_total.saturating_sub(cached);
    output.usage.output = chunk_usage.completion_tokens as u64;
    output.usage.cache_read = cached;
    output.usage.total_tokens = chunk_usage.total_tokens as u64;
    crate::models::calculate_cost(model, &mut output.usage);
}

/// Apply a single SSE delta to `output`, returning the events to yield.
///
/// On each delta we (1) start a content block if the modality changed (text /
/// thinking / tool call), (2) append the delta to both the `current_block`
/// scratch buffer **and** the corresponding entry in `output.content`, and
/// (3) emit a `*_start` / `*_delta` event so subscribers can render the
/// partial message live.
fn handle_delta(
    delta: &openai_rust::types::Delta,
    current_block: &mut Option<CurrentBlock>,
    output: &mut AssistantMessage,
) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();

    // Text content. OpenAI ships it under `delta.content`; for vendors that
    // wrap text into an array (e.g. multipart with images) we flatten all
    // text parts in order.
    let text_parts: Vec<String> = match delta.content.as_ref() {
        Some(Content::Text(text)) if !text.is_empty() => vec![text.clone()],
        Some(Content::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    for piece in text_parts {
        if !matches!(current_block, Some(CurrentBlock::Text(_))) {
            events.extend(finish_current_block(current_block, output));
            *current_block = Some(CurrentBlock::Text(TextContent::new(String::new())));
            output
                .content
                .push(AssistantContentBlock::Text(TextContent::new(String::new())));
            let idx = (output.content.len() - 1) as u32;
            events.push(AssistantMessageEvent::TextStart {
                content_index: idx,
                partial: output.clone(),
            });
        }

        if let Some(CurrentBlock::Text(text_block)) = current_block {
            text_block.text.push_str(&piece);
        }
        if let Some(AssistantContentBlock::Text(last)) = output.content.last_mut() {
            last.text.push_str(&piece);
        }
        let idx = (output.content.len() - 1) as u32;
        events.push(AssistantMessageEvent::TextDelta {
            content_index: idx,
            delta: piece,
            partial: output.clone(),
        });
    }

    // Reasoning / thinking content.
    if let Some(reasoning) = &delta.reasoning
        && !reasoning.is_empty()
    {
        if !matches!(current_block, Some(CurrentBlock::Thinking(_))) {
            events.extend(finish_current_block(current_block, output));
            *current_block = Some(CurrentBlock::Thinking(ThinkingContent::new(String::new())));
            output
                .content
                .push(AssistantContentBlock::Thinking(ThinkingContent::new(
                    String::new(),
                )));
            let idx = (output.content.len() - 1) as u32;
            events.push(AssistantMessageEvent::ThinkingStart {
                content_index: idx,
                partial: output.clone(),
            });
        }

        if let Some(CurrentBlock::Thinking(thinking_block)) = current_block {
            thinking_block.thinking.push_str(reasoning);
        }
        if let Some(AssistantContentBlock::Thinking(last)) = output.content.last_mut() {
            last.thinking.push_str(reasoning);
        }
        let idx = (output.content.len() - 1) as u32;
        events.push(AssistantMessageEvent::ThinkingDelta {
            content_index: idx,
            delta: reasoning.clone(),
            partial: output.clone(),
        });
    }

    // Tool calls.
    if let Some(tool_calls) = &delta.tool_calls {
        for tool_call in tool_calls {
            let tool_id = tool_call.id.clone().unwrap_or_default();
            let tool_index = tool_call.index;

            // A delta starts a new tool call when its `index` differs from
            // the in-flight block's. `id` arrives only in the first chunk
            // for any given tool call — subsequent argument deltas omit it
            // — so comparing on `id` would (incorrectly) split a single
            // streamed tool call across multiple blocks.
            let is_new_tool = match current_block {
                Some(CurrentBlock::ToolCall(_, _, idx)) => *idx != tool_index,
                _ => true,
            };

            if is_new_tool {
                events.extend(finish_current_block(current_block, output));
                let tc = ToolCall::new(&tool_id, "", serde_json::json!({}));
                *current_block = Some(CurrentBlock::ToolCall(
                    tc.clone(),
                    String::new(),
                    tool_index,
                ));
                output.content.push(AssistantContentBlock::ToolCall(tc));
                let idx = (output.content.len() - 1) as u32;
                events.push(AssistantMessageEvent::ToolCallStart {
                    content_index: idx,
                    partial: output.clone(),
                });
            }

            let delta_str = tool_call
                .function
                .as_ref()
                .and_then(|f| f.arguments.clone())
                .unwrap_or_default();

            if let Some(CurrentBlock::ToolCall(tc, partial_args, _)) = current_block
                && let Some(function) = &tool_call.function
            {
                // `id` may arrive in a later chunk (rare but the protocol
                // permits it). Adopt the first non-empty id we see so the
                // final tool call carries the real provider-assigned id.
                if tc.id.is_empty() && !tool_id.is_empty() {
                    tc.id = tool_id.clone();
                    if let Some(AssistantContentBlock::ToolCall(last_tc)) =
                        output.content.last_mut()
                    {
                        last_tc.id = tool_id.clone();
                    }
                }
                // Treat the first non-empty name as authoritative.
                // Some providers (notably proxies) repeat `function.name`
                // on later chunks with a stale or wrong value; clobbering
                // would silently rename the tool mid-stream.
                if tc.name.is_empty()
                    && let Some(name) = &function.name
                    && !name.is_empty()
                {
                    tc.name = name.clone();
                    if let Some(AssistantContentBlock::ToolCall(last_tc)) =
                        output.content.last_mut()
                    {
                        last_tc.name = name.clone();
                    }
                }
                if let Some(args) = &function.arguments {
                    partial_args.push_str(args);
                }
            }

            let idx = (output.content.len() - 1) as u32;
            events.push(AssistantMessageEvent::ToolCallDelta {
                content_index: idx,
                delta: delta_str,
                partial: output.clone(),
            });
        }
    }

    events
}

#[derive(Clone)]
enum CurrentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    /// (tool_call, accumulated_args_buffer, openai_protocol_index).
    /// `openai_protocol_index` is the `DeltaToolCall.index` field — the
    /// canonical OpenAI streaming protocol identifier for a tool call,
    /// stable across chunks (unlike `id` which is only sent in the first
    /// chunk). We compare on this when deciding whether subsequent
    /// argument deltas extend the current block or start a new one.
    ToolCall(ToolCall, String, u32),
}

/// Finalize the in-flight block (if any) and emit its terminating event.
fn finish_current_block(
    current_block: &mut Option<CurrentBlock>,
    output: &mut AssistantMessage,
) -> Vec<AssistantMessageEvent> {
    let Some(block) = current_block.take() else {
        return Vec::new();
    };
    // The index of the block we're closing is always the last entry — we
    // never start a new block without pushing into `output.content`.
    let Some(content_index) = output.content.len().checked_sub(1).map(|i| i as u32) else {
        return Vec::new();
    };
    match block {
        CurrentBlock::Text(_) => {
            let content = match output.content.last() {
                Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                _ => String::new(),
            };
            vec![AssistantMessageEvent::TextEnd {
                content_index,
                content,
                partial: output.clone(),
            }]
        }
        CurrentBlock::Thinking(_) => {
            let content = match output.content.last() {
                Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
                _ => String::new(),
            };
            vec![AssistantMessageEvent::ThinkingEnd {
                content_index,
                content,
                partial: output.clone(),
            }]
        }
        CurrentBlock::ToolCall(mut tc, partial_args, _) => {
            tc.arguments = parse_streaming_json(&partial_args);
            if let Some(AssistantContentBlock::ToolCall(last_tc)) = output.content.last_mut() {
                last_tc.arguments = tc.arguments.clone();
            }
            vec![AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call: tc,
                partial: output.clone(),
            }]
        }
    }
}

// =============================================================================
// Client & Request Building
// =============================================================================

/// Assemble the per-request header set: model catalog headers, session
/// affinity headers, GitHub Copilot protocol headers, then caller
/// overrides (later entries win).
fn assemble_request_headers(
    model: &Model,
    context: &Context,
    compat: &ResolvedCompat,
    options_headers: Option<&HashMap<String, String>>,
    session_id: Option<&str>,
    cache_retention: Option<crate::types::CacheRetention>,
) -> HashMap<String, String> {
    let mut headers = model.headers.clone().unwrap_or_default();

    for (k, v) in resolve_session_affinity_headers(compat, session_id, cache_retention) {
        headers.insert(k, v);
    }

    if model.provider == Provider::GitHubCopilot {
        let messages = &context.messages;
        let last_message = messages.last();
        let is_agent_call = last_message
            .map(|m| !matches!(m, Message::User(_)))
            .unwrap_or(false);
        headers.insert(
            "X-Initiator".to_string(),
            if is_agent_call { "agent" } else { "user" }.to_string(),
        );
        headers.insert(
            "Openai-Intent".to_string(),
            "conversation-edits".to_string(),
        );

        let has_images = messages.iter().any(|msg| match msg {
            Message::User(user_msg) => match &user_msg.content {
                crate::types::UserContent::Blocks(blocks) => blocks
                    .iter()
                    .any(|b| matches!(b, UserContentBlock::Image(_))),
                _ => false,
            },
            Message::ToolResult(tool_result) => tool_result
                .content
                .iter()
                .any(|c| matches!(c, crate::types::ToolResultContent::Image(_))),
            _ => false,
        });

        if has_images {
            headers.insert("Copilot-Vision-Request".to_string(), "true".to_string());
        }
    }

    if let Some(opts) = options_headers {
        headers.extend(opts.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    headers
}

fn create_client(
    model: &Model,
    context: &Context,
    api_key: &str,
    options_headers: Option<&HashMap<String, String>>,
    session_id: Option<&str>,
    cache_retention: Option<crate::types::CacheRetention>,
) -> Result<Client, String> {
    let compat = get_compat(model);
    let headers = assemble_request_headers(
        model,
        context,
        &compat,
        options_headers,
        session_id,
        cache_retention,
    );

    // The SDK client only sets bearer auth itself; everything assembled
    // above must ride on the underlying HTTP client as default headers,
    // or it never reaches the wire. Entries that don't form valid header
    // names/values are skipped rather than failing the whole request.
    let mut header_map = reqwest::header::HeaderMap::new();
    for (k, v) in &headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            header_map.insert(name, value);
        }
    }
    let http_client = reqwest::Client::builder()
        .default_headers(header_map)
        .build()
        .map_err(|e| e.to_string())?;

    Client::builder()
        .api_key(api_key.to_string())
        .base_url(model.base_url.clone())
        .http_client(http_client)
        .build()
        .map_err(|e| e.to_string())
}

fn build_params(
    model: &Model,
    context: &Context,
    options: &OpenAICompletionsOptions,
) -> Result<CompletionRequest, String> {
    let compat = get_compat(model);
    let messages = convert_messages(model, context, &compat);

    let mut builder = CompletionRequest::builder()
        .model(&model.id)
        .messages(messages)
        .stream(true);

    let cache_decision = decide_openai_prompt_cache(
        &model.base_url,
        options.base.session_id.as_deref(),
        options.base.cache_retention,
    );
    if let Some(key) = cache_decision.key {
        builder = builder.prompt_cache_key(key);
    }
    if let Some(retention) = cache_decision.retention {
        builder = builder.insert_extra_param(
            "prompt_cache_retention",
            serde_json::Value::String(retention.to_string()),
        );
    }

    if compat.supports_usage_in_streaming {
        builder = builder.stream_options(openai_rust::types::StreamOptions {
            include_usage: Some(true),
            include_obfuscation: None,
        });
    }

    if compat.supports_store {
        builder = builder.store(false);
    }

    if let Some(max_tokens) = options.max_tokens() {
        if compat.max_tokens_field.as_deref() == Some("max_tokens") {
            builder = builder.max_tokens(max_tokens);
        } else {
            builder = builder.max_completion_tokens(max_tokens);
        }
    }

    if let Some(temp) = options.temperature() {
        builder = builder.temperature(temp);
    }

    match decide_tools_field(context.tools.as_deref(), &context.messages) {
        ToolsField::NonEmpty(tools) => {
            let openai_tools: Vec<OpenAiTool> = tools.iter().map(convert_tool).collect();
            builder = builder.tools(openai_tools);
            // Z.ai (and newer GLM models) expose tool-call streaming
            // via a top-level `tool_stream: true` flag. The detector
            // sets `zai_tool_stream` from the model id / provider;
            // emit the flag only when tools are actually present in
            // the request, never on history-only / empty-tools
            // shapes that synthesize an empty `tools: []`.
            if compat.zai_tool_stream {
                builder = builder.insert_extra_param("tool_stream", serde_json::Value::Bool(true));
            }
        }
        ToolsField::EmptyArrayForHistory => {
            builder = builder.tools(vec![]);
        }
        ToolsField::Omit => {}
    }

    if let Some(tool_choice) = &options.tool_choice {
        builder = builder.tool_choice(tool_choice.clone());
    }

    if compat.thinking_format.as_deref() == Some("zai") && model.reasoning {
        let mut extra = HashMap::new();
        extra.insert(
            "thinking".to_string(),
            serde_json::json!({ "type": if options.reasoning_effort.is_some() { "enabled" } else { "disabled" } }),
        );
        builder = builder.extra_params(extra);
    } else if compat.thinking_format.as_deref() == Some("qwen") && model.reasoning {
        let mut extra = HashMap::new();
        extra.insert(
            "enable_thinking".to_string(),
            serde_json::json!(options.reasoning_effort.is_some()),
        );
        builder = builder.extra_params(extra);
    } else if compat.thinking_format.as_deref() == Some("qwen-chat-template") && model.reasoning {
        // Local Qwen-compatible servers (vLLM, llama.cpp) read the
        // chat-template knobs nested under `chat_template_kwargs`.
        // `preserve_thinking: true` keeps prior turns' thinking text
        // available so multi-turn tool calls don't degrade to empty
        // `{}` payloads (closes the upstream regression #3325).
        let mut extra = HashMap::new();
        extra.insert(
            "chat_template_kwargs".to_string(),
            serde_json::json!({
                "enable_thinking": options.reasoning_effort.is_some(),
                "preserve_thinking": true,
            }),
        );
        builder = builder.extra_params(extra);
    } else if compat.thinking_format.as_deref() == Some("openrouter") && model.reasoning {
        // OpenRouter normalizes reasoning across upstreams via a
        // nested `reasoning` object. When the caller explicitly
        // asked for an effort, forward the mapped value; when they
        // didn't, emit `effort: "none"` so the router doesn't burn
        // thinking tokens by default. The SDK's top-level
        // `reasoning_effort` field is the wrong shape for OpenRouter
        // — fall through to extra_params for the nested form.
        // A provider-native keyword from the thinking level map beats the
        // clamped enum — it names an effort the enum cannot express.
        let effort_str =
            options
                .native_reasoning_effort
                .as_deref()
                .unwrap_or(match options.reasoning_effort {
                    Some(openai_rust::types::ReasoningEffort::Minimal) => "minimal",
                    Some(openai_rust::types::ReasoningEffort::Low) => "low",
                    Some(openai_rust::types::ReasoningEffort::Medium) => "medium",
                    Some(openai_rust::types::ReasoningEffort::High) => "high",
                    None => "none",
                });
        builder =
            builder.insert_extra_param("reasoning", serde_json::json!({ "effort": effort_str }));
    } else if options.reasoning_effort.is_some()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        if let Some(native) = &options.native_reasoning_effort {
            // Same wire key as the typed field, but free of the enum's
            // ceiling; serde flattens it into the request body.
            builder = builder.insert_extra_param("reasoning_effort", serde_json::json!(native));
        } else if let Some(effort) = options.reasoning_effort {
            builder = builder.reasoning_effort(effort);
        }
    }

    if model.base_url.contains("openrouter.ai")
        && let Some(crate::types::Compat::OpenAICompletions(compat_settings)) = &model.compat
        && let Some(router_routing) = &compat_settings.open_router_routing
    {
        // Serialize the whole `OpenRouterRouting` struct: every
        // `Option` field is `skip_serializing_if = "Option::is_none"`
        // so unset fields drop out of the JSON. Emit the `provider`
        // object only if SOMETHING set — otherwise the upstream sees
        // an empty `provider: {}` which it rejects on some routes.
        if let Ok(provider_json) = serde_json::to_value(router_routing)
            && provider_json.as_object().is_some_and(|obj| !obj.is_empty())
        {
            let mut extra = HashMap::new();
            extra.insert("provider".to_string(), provider_json);
            builder = builder.extra_params(extra);
        }
    }

    // Vercel AI Gateway provider routing. The gateway exposes
    // `providerOptions.gateway.{only, order}` to pin upstream providers
    // or set an explicit preference order — only honoured when the
    // base URL targets `ai-gateway.vercel.sh`. Serde drops every
    // `Option::None` field, so we only emit the wrapper when at least
    // one field is set (an empty `providerOptions.gateway: {}` is
    // rejected on some routes, mirroring the OpenRouter case above).
    if model.base_url.contains("ai-gateway.vercel.sh")
        && let Some(crate::types::Compat::OpenAICompletions(compat_settings)) = &model.compat
        && let Some(gateway_routing) = &compat_settings.vercel_gateway_routing
        && let Ok(gateway_json) = serde_json::to_value(gateway_routing)
        && gateway_json.as_object().is_some_and(|obj| !obj.is_empty())
    {
        builder = builder.insert_extra_param(
            "providerOptions",
            serde_json::json!({ "gateway": gateway_json }),
        );
    }

    builder.build().map_err(|e| e.to_string())
}

// =============================================================================
// Message Conversion
// =============================================================================

/// Convert internal messages to OpenAI request messages.
pub fn convert_messages(
    model: &Model,
    context: &Context,
    compat: &ResolvedCompat,
) -> Vec<RequestMessage> {
    let mut params: Vec<RequestMessage> = vec![];

    if let Some(system_prompt) = &context.system_prompt {
        params.push(RequestMessage::new(
            Role::System,
            sanitize_surrogates(system_prompt),
        ));
    }

    let mut last_role: Option<String> = None;

    let mut i = 0;
    while i < context.messages.len() {
        let msg = &context.messages[i];
        if compat.requires_assistant_after_tool_result
            && last_role.as_deref() == Some("toolResult")
            && matches!(msg, Message::User(_))
        {
            params.push(RequestMessage::new(
                Role::Assistant,
                "I have processed the tool results.",
            ));
        }

        match msg {
            Message::User(user_msg) => match &user_msg.content {
                crate::types::UserContent::Text(text) => {
                    params.push(RequestMessage::new(Role::User, sanitize_surrogates(text)));
                }
                crate::types::UserContent::Blocks(blocks) => {
                    let content_parts: Vec<ContentPart> = blocks
                        .iter()
                        .filter(|block| match block {
                            UserContentBlock::Image(_) => model.input.contains(&InputType::Image),
                            _ => true,
                        })
                        .map(|block| match block {
                            UserContentBlock::Text(text) => ContentPart::Text {
                                text: sanitize_surrogates(&text.text),
                            },
                            UserContentBlock::Image(img) => ContentPart::ImageUrl {
                                image_url: ImageUrl {
                                    url: format!("data:{};base64,{},", img.mime_type, img.data),
                                    detail: None,
                                },
                            },
                        })
                        .collect();

                    if !content_parts.is_empty() {
                        params.push(RequestMessage {
                            role: Role::User,
                            content: Content::Array(content_parts),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                    }
                }
            },
            Message::Assistant(assistant_msg) => {
                let assistant_content = Content::Text("".to_string());

                let text_blocks: Vec<&TextContent> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContentBlock::Text(t) => Some(t),
                        _ => None,
                    })
                    .filter(|t| !t.text.trim().is_empty())
                    .collect();

                // Always serialize assistant text as a plain string. The
                // legacy `Content::Array([{type: "text", text: ...}])`
                // shape is non-standard for `role: "assistant"` and
                // triggers mirrored-structure output on some hosted
                // gateways (e.g. DeepSeek V3.2 via NVIDIA NIM echoes the
                // wrapper as literal text in the reply).
                let assistant_content = if !text_blocks.is_empty() {
                    let joined: String = text_blocks
                        .iter()
                        .map(|t| sanitize_surrogates(&t.text))
                        .collect::<Vec<_>>()
                        .join("");
                    Content::Text(joined)
                } else {
                    assistant_content
                };

                let thinking_blocks: Vec<&ThinkingContent> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContentBlock::Thinking(t) => Some(t),
                        _ => None,
                    })
                    .filter(|t| !t.thinking.trim().is_empty())
                    .collect();

                let assistant_content =
                    if !thinking_blocks.is_empty() && compat.requires_thinking_as_text {
                        let thinking_text: String = thinking_blocks
                            .iter()
                            .map(|t| sanitize_surrogates(&t.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");

                        // Emit a `Content::Array` so the thinking text and any
                        // existing assistant text survive as discrete
                        // `{type: "text"}` parts. Joining them into a single
                        // string corrupts same-model replays for providers
                        // that key on the multi-part shape (e.g. llama.cpp +
                        // gpt-oss assistant turns with both thinking and
                        // text).
                        let mut parts: Vec<ContentPart> = vec![ContentPart::Text {
                            text: thinking_text,
                        }];
                        for tb in &text_blocks {
                            parts.push(ContentPart::Text {
                                text: sanitize_surrogates(&tb.text),
                            });
                        }
                        Content::Array(parts)
                    } else {
                        assistant_content
                    };

                let tool_calls: Vec<&ToolCall> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContentBlock::ToolCall(tc) => Some(tc),
                        _ => None,
                    })
                    .collect();

                let openai_tool_calls = if tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        tool_calls
                            .iter()
                            .map(|tc| OpenAiToolCall {
                                id: normalize_tool_call_id(&tc.id, compat, model),
                                tool_type: "function".to_string(),
                                function: FunctionCall {
                                    name: tc.name.clone(),
                                    arguments: tc.arguments.to_string(),
                                },
                            })
                            .collect(),
                    )
                };

                let has_content = match &assistant_content {
                    Content::Text(t) => !t.is_empty(),
                    Content::Array(a) => !a.is_empty(),
                };

                if !has_content && openai_tool_calls.is_none() {
                    continue;
                }

                params.push(RequestMessage {
                    role: Role::Assistant,
                    content: assistant_content,
                    tool_calls: openai_tool_calls,
                    tool_call_id: None,
                    name: None,
                });
            }
            Message::ToolResult(_tool_result) => {
                let mut image_blocks: Vec<ContentPart> = vec![];
                let mut j = i;

                while j < context.messages.len() {
                    if let Message::ToolResult(tr) = &context.messages[j] {
                        let text_result: String = tr
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                crate::types::ToolResultContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        let has_images = tr
                            .content
                            .iter()
                            .any(|c| matches!(c, crate::types::ToolResultContent::Image(_)));

                        // Tool results always ship non-empty content: image-only
                        // results point at the batched image message below, and
                        // fully empty results get an explicit placeholder — some
                        // providers reject empty tool content outright, and the
                        // model otherwise can't tell the tool ran and returned
                        // nothing.
                        let content = if text_result.is_empty() {
                            if has_images {
                                "(see attached image)".to_string()
                            } else {
                                "(no tool output)".to_string()
                            }
                        } else {
                            sanitize_surrogates(&text_result)
                        };

                        // The id must be normalized with the same rules as the
                        // assistant tool_calls id above — the API matches tool
                        // messages to prior calls by exact id, so an
                        // unnormalized composite id here would orphan the pair.
                        let mut tool_msg = RequestMessage::tool_response(
                            content,
                            normalize_tool_call_id(&tr.tool_call_id, compat, model),
                        );

                        if compat.requires_tool_result_name {
                            tool_msg = tool_msg.with_name(tr.tool_name.clone());
                        }

                        params.push(tool_msg);

                        if has_images && model.input.contains(&InputType::Image) {
                            for block in &tr.content {
                                if let crate::types::ToolResultContent::Image(img) = block {
                                    image_blocks.push(ContentPart::ImageUrl {
                                        image_url: ImageUrl {
                                            url: format!(
                                                "data:{};base64,{}",
                                                img.mime_type, img.data
                                            ),
                                            detail: None,
                                        },
                                    });
                                }
                            }
                        }

                        j += 1;
                    } else {
                        break;
                    }
                }

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(RequestMessage::new(
                            Role::Assistant,
                            "I have processed the tool results.",
                        ));
                    }

                    let mut content_parts = vec![ContentPart::Text {
                        text: "Attached image(s) from tool result:".to_string(),
                    }];
                    content_parts.extend(image_blocks);

                    params.push(RequestMessage {
                        role: Role::User,
                        content: Content::Array(content_parts),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });

                    last_role = Some("user".to_string());
                } else {
                    last_role = Some("toolResult".to_string());
                }

                // Skip past the consecutive ToolResults the inner loop just
                // consumed. Without this the outer loop would re-enter the
                // second tool result and emit it (with its image batch)
                // twice.
                i = j;
                continue;
            }
        }

        last_role = Some(match msg {
            Message::User(_) => "user".to_string(),
            Message::Assistant(_) => "assistant".to_string(),
            Message::ToolResult(_) => "toolResult".to_string(),
        });
        i += 1;
    }

    params
}

// =============================================================================
// Helpers
// =============================================================================

fn convert_tool(tool: &Tool) -> OpenAiTool {
    OpenAiTool {
        tool_type: "function".to_string(),
        function: openai_rust::types::Function {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        },
    }
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|msg| {
        matches!(msg, Message::ToolResult(_)) ||
        matches!(msg, Message::Assistant(assistant) if assistant.content.iter().any(|b| matches!(b, AssistantContentBlock::ToolCall(_))))
    })
}

/// What to do with the `tools` field on an OpenAI-compatible request.
///
/// DashScope / Aliyun Qwen rejects `tools: []` with HTTP 400 (`"[] is too
/// short - 'tools'"`). At the same time, LiteLLM and certain Anthropic
/// proxies require `tools: []` when the conversation already has tool
/// history. Encode that policy here as a pure decision so the body
/// builder doesn't have to mix the three cases inline.
#[derive(Debug)]
pub(crate) enum ToolsField<'a> {
    /// Omit the `tools` field entirely.
    Omit,
    /// Emit `tools: []` to satisfy proxies that need the field present.
    EmptyArrayForHistory,
    /// Emit a non-empty array of tool definitions.
    NonEmpty(&'a [crate::types::Tool]),
}

pub(crate) fn decide_tools_field<'a>(
    tools: Option<&'a [crate::types::Tool]>,
    messages: &[Message],
) -> ToolsField<'a> {
    match tools {
        Some(t) if !t.is_empty() => ToolsField::NonEmpty(t),
        _ if has_tool_history(messages) => ToolsField::EmptyArrayForHistory,
        _ => ToolsField::Omit,
    }
}

/// Decision for OpenAI's prompt-cache fields. Direct OpenAI requests
/// only — other openai-compatible providers either don't support these
/// fields at all (DashScope, vLLM) or use their own naming, so the
/// helper gates on `model.base_url.contains("api.openai.com")`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PromptCacheDecision {
    /// Value for `prompt_cache_key`. `None` means omit the field.
    pub key: Option<String>,
    /// Value for `prompt_cache_retention`. `None` means omit the field.
    pub retention: Option<&'static str>,
}

pub(crate) fn decide_openai_prompt_cache(
    base_url: &str,
    session_id: Option<&str>,
    cache_retention: Option<crate::types::CacheRetention>,
) -> PromptCacheDecision {
    use crate::types::CacheRetention;
    if !base_url.contains("api.openai.com") {
        return PromptCacheDecision {
            key: None,
            retention: None,
        };
    }
    // Default to `Short` when the caller did not pin a value, but let
    // `PI_CACHE_RETENTION=long` flip the default to `Long` so operators
    // can opt every direct request into 24h prompt caching without
    // threading an option through every call site.
    let resolved = CacheRetention::resolve(cache_retention);
    let key = if resolved != CacheRetention::None {
        session_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    } else {
        None
    };
    let retention = if resolved == CacheRetention::Long {
        Some("24h")
    } else {
        None
    };
    PromptCacheDecision { key, retention }
}

/// Normalize tool call ID for Mistral.
/// Mistral requires tool IDs to be exactly 9 alphanumeric characters.
pub fn normalize_mistral_tool_id(id: &str) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    if normalized.len() < 9 {
        let padding = "ABCDEFGHI";
        format!("{}{}", normalized, &padding[..9 - normalized.len()])
    } else if normalized.len() > 9 {
        normalized[..9].to_string()
    } else {
        normalized
    }
}

/// Deterministic FNV-1a 64-bit hash rendered as 8 hex chars. Keeps oversized
/// composite tool call ids unique and stable within the 40-char API limit.
fn short_tool_call_id_hash(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")[..8].to_string()
}

fn normalize_tool_call_id(id: &str, compat: &ResolvedCompat, model: &Model) -> String {
    if compat.requires_mistral_tool_ids {
        return normalize_mistral_tool_id(id);
    }

    if let Some((raw_call_id, raw_item_id)) = id.split_once('|') {
        let sanitize = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        };
        // Multiple tool calls in the same turn can share the call_id part
        // while differing by item id. Keep both parts so replayed Chat
        // Completions payloads carry distinct tool call ids; collapse to a
        // hash suffix when the combination exceeds the 40-char limit.
        let call_id = sanitize(raw_call_id);
        let item_id = sanitize(raw_item_id);
        let combined = if item_id.is_empty() {
            call_id.clone()
        } else {
            format!("{call_id}_{item_id}")
        };
        if combined.len() <= 40 {
            return combined;
        }
        let hash = short_tool_call_id_hash(id);
        let prefix: String = call_id.chars().take(40 - hash.len() - 1).collect();
        return format!("{prefix}_{hash}");
    }

    if model.provider == Provider::OpenAI {
        return if id.len() > 40 {
            id[..40].to_string()
        } else {
            id.to_string()
        };
    }

    if model.provider == Provider::GitHubCopilot && model.id.to_lowercase().contains("claude") {
        return id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
    }

    id.to_string()
}

fn parse_streaming_json(input: &str) -> Value {
    let trimmed = input.trim();
    // Empty stream (model emitted a tool call without any input deltas)
    // is the no-args case — return an empty object, NOT a string. Returning
    // `Value::String("")` here breaks JSON-schema validation downstream
    // ("" is not of type "object") and produces silent tool-call failures
    // for any tool whose schema requires `type: object`, even if all
    // properties are optional.
    if trimmed.is_empty() {
        return serde_json::json!({});
    }
    if let Ok(v) = serde_json::from_str::<Value>(input) {
        return v;
    }
    // Retry with the shared repair pass — escapes raw control bytes
    // and doubles invalid backslash escapes inside string literals so
    // a malformed tool-call payload doesn't collapse to `{}`.
    if let Some(v) = crate::transform::parse_json_with_repair(input) {
        return v;
    }
    if trimmed.starts_with('{') {
        // Truncated mid-stream object — degrade to empty object rather
        // than passing fragmentary JSON downstream.
        serde_json::json!({})
    } else {
        // Genuinely non-JSON payload (e.g. some providers send the
        // arguments as a bare string for single-arg tools). Preserve
        // as a JSON string so the agent can still see what was sent.
        Value::String(input.to_string())
    }
}

fn map_thinking_level(level: ThinkingLevel) -> openai_rust::types::ReasoningEffort {
    match level {
        ThinkingLevel::Minimal => openai_rust::types::ReasoningEffort::Minimal,
        ThinkingLevel::Low => openai_rust::types::ReasoningEffort::Low,
        ThinkingLevel::Medium => openai_rust::types::ReasoningEffort::Medium,
        ThinkingLevel::High => openai_rust::types::ReasoningEffort::High,
        // The wire enum tops out at `high` — the extended levels clamp.
        ThinkingLevel::Xhigh | ThinkingLevel::Max => openai_rust::types::ReasoningEffort::High,
    }
}

fn clamp_reasoning(reasoning: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    reasoning.map(|r| match r {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        _ => r,
    })
}

/// Provider-native effort keyword for the requested level, if the model's
/// thinking level map defines one. The map values name what the provider
/// itself accepts (e.g. an effort above the wire enum's ceiling), so when
/// present they beat the clamped enum at emission time.
fn resolve_native_effort(model: &Model, level: Option<ThinkingLevel>) -> Option<String> {
    let map = model.thinking_level_map.as_ref()?;
    map.get(crate::models::thinking_level_map_key(Some(level?)))?
        .clone()
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::Stop,
        "length" => StopReason::Length,
        "function_call" | "tool_calls" => StopReason::ToolUse,
        "content_filter" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

/// Final stop reason for a completed stream.
///
/// A `finish_reason` from the provider always wins. Some
/// OpenAI-compatible endpoints never send one, though, and the reason
/// then still carries the pre-stream `Stop` default — reporting a turn
/// that ended on its own even when the model asked for tools. Fall back
/// to what actually arrived in that case: tool calls mean `ToolUse`,
/// anything else means `Stop`.
fn resolve_stop_reason(
    saw_finish_reason: bool,
    reported: StopReason,
    content: &[AssistantContentBlock],
) -> StopReason {
    if saw_finish_reason {
        return reported;
    }
    if content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        StopReason::ToolUse
    } else {
        StopReason::Stop
    }
}

fn sanitize_surrogates(text: &str) -> String {
    text.chars()
        .filter(|&c| !(0xD800..=0xDFFF).contains(&(c as u32)))
        .collect()
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Compatibility Detection
// =============================================================================

/// Resolved compatibility settings with all fields set.
///
/// Fields are populated by `detect_compat` from
/// `model.provider`/`model.base_url`, then overridden by any explicit
/// settings on `model.compat`. New fields should be added with sane
/// defaults so older callers keep compiling.
#[derive(Debug, Clone)]
pub struct ResolvedCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub max_tokens_field: Option<String>,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_mistral_tool_ids: bool,
    pub thinking_format: Option<String>,
    pub open_router_routing: Option<crate::types::OpenRouterRouting>,
    pub vercel_gateway_routing: Option<crate::types::VercelGatewayRouting>,
    /// `true` when the upstream supports OpenAI strict-mode tool calls.
    /// Cloudflare Workers AI does not.
    pub supports_strict_mode: bool,
    /// `true` when the model exposes Z.ai's incremental tool-stream protocol.
    pub zai_tool_stream: bool,
    /// `true` when the upstream rejects assistant messages on replay that
    /// omit a `reasoning_content` field (deepseek native API). When set,
    /// `convert_messages` injects an empty `reasoning_content: ""` on any
    /// assistant turn that doesn't already carry one.
    pub requires_reasoning_content_on_assistant_messages: bool,
    /// `true` when the endpoint routes repeated prompts to the same
    /// cache node by reading the caller's session id from
    /// session-affinity headers. Default `false` — OpenAI's prompt
    /// cache uses the `prompt_cache_key` body field instead. Proxies
    /// that rely on header-based affinity flip this true via
    /// models.dev compat metadata. Which headers are sent is governed
    /// by `session_affinity_format`.
    pub send_session_affinity_headers: bool,
    /// Header convention for session affinity when
    /// `send_session_affinity_headers` is on. Auto-detected:
    /// OpenRouter endpoints read a single `x-session-id` header,
    /// everything else gets the OpenAI-style set.
    pub session_affinity_format: crate::types::SessionAffinityFormat,
}

fn detect_compat(model: &Model) -> ResolvedCompat {
    let provider = &model.provider;
    let base_url = &model.base_url;

    let is_zai = *provider == Provider::Zai
        || base_url.contains("api.z.ai")
        || base_url.contains("bigmodel.cn");

    let is_qwen = base_url.contains("dashscope.aliyuncs.com");

    let is_deepseek = *provider == Provider::Deepseek || base_url.contains("deepseek.com");

    let is_openrouter = *provider == Provider::Openrouter || base_url.contains("openrouter.ai");

    let is_cloudflare_workers_ai = *provider == Provider::CloudflareWorkersAi
        || base_url.contains("cloudflare.com/client/v4/accounts");

    // Moonshot's OpenAI-compatible endpoint (Kimi family) rejects
    // `reasoning_effort`, OpenAI strict tool mode, and `developer` role,
    // and requires `max_tokens` (not `max_completion_tokens`). Recognise
    // both the global and China-region providers and the public base URL.
    let is_moonshot = *provider == Provider::Moonshotai
        || *provider == Provider::MoonshotaiCn
        || base_url.contains("api.moonshot.");

    let is_non_standard = *provider == Provider::Cerebras
        || base_url.contains("cerebras.ai")
        || *provider == Provider::Xai
        || base_url.contains("api.x.ai")
        || *provider == Provider::Mistral
        || base_url.contains("mistral.ai")
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || *provider == Provider::Opencode
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai;

    let use_max_tokens = *provider == Provider::Mistral
        || base_url.contains("mistral.ai")
        || base_url.contains("chutes.ai")
        || is_moonshot;

    let is_grok = *provider == Provider::Xai || base_url.contains("api.x.ai");
    let is_mistral = *provider == Provider::Mistral || base_url.contains("mistral.ai");

    // Pick `thinking_format` precedence: explicit-deepseek > zai > qwen > openrouter
    // > openai default. Mirrors the TS reference and keeps zai overlapping with
    // the boolean `is_zai` check.
    let thinking_format = if is_deepseek {
        Some("deepseek".to_string())
    } else if is_zai {
        Some("zai".to_string())
    } else if is_qwen {
        Some("qwen".to_string())
    } else if is_openrouter {
        Some("openrouter".to_string())
    } else {
        Some("openai".to_string())
    };

    ResolvedCompat {
        supports_store: !is_non_standard,
        supports_developer_role: !is_non_standard,
        supports_reasoning_effort: !is_grok && !is_zai && !is_moonshot,
        supports_usage_in_streaming: true,
        max_tokens_field: if use_max_tokens {
            Some("max_tokens".to_string())
        } else {
            None
        },
        requires_tool_result_name: is_mistral,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: is_mistral,
        requires_mistral_tool_ids: is_mistral,
        thinking_format,
        open_router_routing: if is_openrouter {
            Some(crate::types::OpenRouterRouting::default())
        } else {
            None
        },
        vercel_gateway_routing: None,
        supports_strict_mode: !is_cloudflare_workers_ai && !is_moonshot,
        zai_tool_stream: is_zai,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        send_session_affinity_headers: false,
        session_affinity_format: if is_openrouter {
            crate::types::SessionAffinityFormat::OpenRouter
        } else {
            crate::types::SessionAffinityFormat::OpenAI
        },
    }
}

fn get_compat(model: &Model) -> ResolvedCompat {
    let detected = detect_compat(model);

    if let Some(crate::types::Compat::OpenAICompletions(compat_settings)) = &model.compat {
        return ResolvedCompat {
            supports_store: compat_settings
                .supports_store
                .unwrap_or(detected.supports_store),
            supports_developer_role: compat_settings
                .supports_developer_role
                .unwrap_or(detected.supports_developer_role),
            supports_reasoning_effort: compat_settings
                .supports_reasoning_effort
                .unwrap_or(detected.supports_reasoning_effort),
            supports_usage_in_streaming: compat_settings
                .supports_usage_in_streaming
                .unwrap_or(detected.supports_usage_in_streaming),
            max_tokens_field: compat_settings
                .max_tokens_field
                .clone()
                .or(detected.max_tokens_field),
            requires_tool_result_name: compat_settings
                .requires_tool_result_name
                .unwrap_or(detected.requires_tool_result_name),
            requires_assistant_after_tool_result: compat_settings
                .requires_assistant_after_tool_result
                .unwrap_or(detected.requires_assistant_after_tool_result),
            requires_thinking_as_text: compat_settings
                .requires_thinking_as_text
                .unwrap_or(detected.requires_thinking_as_text),
            requires_mistral_tool_ids: compat_settings
                .requires_mistral_tool_ids
                .unwrap_or(detected.requires_mistral_tool_ids),
            thinking_format: compat_settings
                .thinking_format
                .clone()
                .or(detected.thinking_format),
            open_router_routing: compat_settings
                .open_router_routing
                .clone()
                .or(detected.open_router_routing),
            vercel_gateway_routing: compat_settings
                .vercel_gateway_routing
                .clone()
                .or(detected.vercel_gateway_routing),
            supports_strict_mode: compat_settings
                .supports_strict_mode
                .unwrap_or(detected.supports_strict_mode),
            zai_tool_stream: compat_settings
                .zai_tool_stream
                .unwrap_or(detected.zai_tool_stream),
            requires_reasoning_content_on_assistant_messages: compat_settings
                .requires_reasoning_content_on_assistant_messages
                .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
            send_session_affinity_headers: compat_settings
                .send_session_affinity_headers
                .unwrap_or(detected.send_session_affinity_headers),
            session_affinity_format: compat_settings
                .session_affinity_format
                .unwrap_or(detected.session_affinity_format),
        };
    }

    detected
}

/// Resolve the merged compatibility settings for a model.
///
/// Public entry-point for callers (and tests) that want the same precedence
/// rules used internally: explicit `model.compat` values win, missing fields
/// fall back to URL/provider auto-detection.
pub fn resolve_compat(model: &Model) -> ResolvedCompat {
    get_compat(model)
}

/// Compute the session-affinity HTTP headers for a request.
///
/// A small set of proxies (LiteLLM with affinity routing, vendor
/// gateways) route repeated prompts to the same cache node by reading
/// the session id from request headers. Off by default — OpenAI's
/// prompt cache uses the `prompt_cache_key` body field instead.
/// Models served by affinity-routing proxies set
/// `OpenAICompletionsCompat.sendSessionAffinityHeaders = true` to
/// opt in; `session_affinity_format` then picks the header set
/// (OpenRouter reads a single `x-session-id` header instead of the
/// OpenAI-style trio). Caching must not be explicitly disabled
/// (`CacheRetention::None`); otherwise affinity is moot.
fn resolve_session_affinity_headers(
    compat: &ResolvedCompat,
    session_id: Option<&str>,
    cache_retention: Option<crate::types::CacheRetention>,
) -> Vec<(String, String)> {
    use crate::types::SessionAffinityFormat;

    if !compat.send_session_affinity_headers {
        return Vec::new();
    }
    if matches!(cache_retention, Some(crate::types::CacheRetention::None)) {
        return Vec::new();
    }
    let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match compat.session_affinity_format {
        SessionAffinityFormat::OpenRouter => {
            vec![("x-session-id".to_string(), sid.to_string())]
        }
        SessionAffinityFormat::OpenAI => vec![
            ("session_id".to_string(), sid.to_string()),
            ("x-client-request-id".to_string(), sid.to_string()),
            ("x-session-affinity".to_string(), sid.to_string()),
        ],
        SessionAffinityFormat::OpenAINoSession => vec![
            ("x-client-request-id".to_string(), sid.to_string()),
            ("x-session-affinity".to_string(), sid.to_string()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Api, AssistantContentBlock, AssistantMessage, Cost, InputType, TextContent, Tool, ToolCall,
        ToolResultContent, ToolResultMessage, Usage, UserMessage,
    };

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage::new_text(text))
    }

    /// Sending `tools: []` on requests with no tool history triggers a
    /// hard 400 on DashScope / Aliyun Qwen ("[] is too short"). When the
    /// caller passes an empty array we must omit the field entirely.
    #[test]
    fn decide_tools_omits_field_when_tools_some_but_empty_and_no_history() {
        let messages = vec![user_msg("hi")];
        let tools: [Tool; 0] = [];
        match decide_tools_field(Some(&tools), &messages) {
            ToolsField::Omit => {}
            other => panic!("expected Omit, got {other:?}"),
        }
    }

    #[test]
    fn decide_tools_omits_field_when_tools_none_and_no_history() {
        let messages = vec![user_msg("hi")];
        match decide_tools_field(None, &messages) {
            ToolsField::Omit => {}
            other => panic!("expected Omit, got {other:?}"),
        }
    }

    /// LiteLLM and certain Anthropic proxies require `tools: []` when
    /// the conversation already has tool history (otherwise they reject
    /// the replay). Preserve that branch.
    #[test]
    fn decide_tools_emits_empty_array_when_history_has_tool_results() {
        let messages = vec![
            user_msg("hi"),
            Message::ToolResult(ToolResultMessage::new(
                "call-1",
                "lookup",
                vec![ToolResultContent::Text(TextContent::new("ok"))],
            )),
        ];
        match decide_tools_field(None, &messages) {
            ToolsField::EmptyArrayForHistory => {}
            other => panic!("expected EmptyArrayForHistory, got {other:?}"),
        }
    }

    /// Direct OpenAI requests should emit `prompt_cache_key` (the
    /// session id) whenever caching is enabled, even at the default
    /// "short" retention. Long retention adds the 24h hint. Both
    /// branches gate on `model.baseUrl.includes("api.openai.com")`,
    /// which is what this helper encodes.
    #[test]
    fn openai_prompt_cache_emits_key_for_direct_openai() {
        use crate::types::CacheRetention;
        let decision = decide_openai_prompt_cache(
            "https://api.openai.com/v1",
            Some("sess-42"),
            None, // default → "short"
        );
        assert_eq!(decision.key.as_deref(), Some("sess-42"));
        assert_eq!(decision.retention, None);

        let long = decide_openai_prompt_cache(
            "https://api.openai.com/v1",
            Some("sess-42"),
            Some(CacheRetention::Long),
        );
        assert_eq!(long.key.as_deref(), Some("sess-42"));
        assert_eq!(long.retention, Some("24h"));
    }

    /// Retention "none" disables the cache key entirely so callers can
    /// opt out of cross-request affinity (e.g. for one-shot completions
    /// where caching adds latency without payoff).
    #[test]
    fn openai_prompt_cache_omits_key_when_retention_none() {
        use crate::types::CacheRetention;
        let decision = decide_openai_prompt_cache(
            "https://api.openai.com/v1",
            Some("sess-42"),
            Some(CacheRetention::None),
        );
        assert_eq!(decision.key, None);
        assert_eq!(decision.retention, None);
    }

    /// Non-OpenAI proxies (DashScope, vLLM, OpenRouter) must NOT receive
    /// the OpenAI-specific cache fields — DashScope rejects unknown
    /// extra parameters as 400 and most proxies just ignore them. The
    /// helper short-circuits.
    #[test]
    fn openai_prompt_cache_skips_other_proxies() {
        use crate::types::CacheRetention;
        for base in [
            "https://openrouter.ai/api/v1",
            "https://dashscope.aliyuncs.com/api/v1",
            "https://api.deepseek.com",
            "",
        ] {
            let decision =
                decide_openai_prompt_cache(base, Some("sess"), Some(CacheRetention::Long));
            assert_eq!(decision.key, None, "{base}");
            assert_eq!(decision.retention, None, "{base}");
        }
    }

    /// Missing or empty session id means no cache key — sending an
    /// empty string would create a cross-session cache collision.
    #[test]
    fn openai_prompt_cache_requires_session_id() {
        let decision = decide_openai_prompt_cache("https://api.openai.com/v1", None, None);
        assert_eq!(decision.key, None);
        let empty = decide_openai_prompt_cache("https://api.openai.com/v1", Some("   "), None);
        assert_eq!(empty.key, None);
    }

    #[test]
    fn decide_tools_passes_non_empty_array_through() {
        let messages = vec![user_msg("hi")];
        let tools = vec![Tool::new("lookup", "look it up", serde_json::json!({}))];
        match decide_tools_field(Some(&tools), &messages) {
            ToolsField::NonEmpty(passed) => assert_eq!(passed.len(), 1),
            other => panic!("expected NonEmpty, got {other:?}"),
        }
    }

    fn test_model(provider: Provider) -> Model {
        Model {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            api: Api::OpenAICompletions,
            provider,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 128_000,
            max_tokens: 4096,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn empty_assistant_message() -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: Vec::new(),
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test-model".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// Issue hand-ai#13 / openai-rust#1 regression. The terminal stream
    /// chunk carries `usage`; we must surface it on the assistant
    /// message (and recompute cost) instead of leaving zeros.
    #[test]
    fn apply_chunk_usage_writes_back_input_output_and_total() {
        let mut model = test_model(Provider::OpenAI);
        model.cost = Cost {
            input: 1.0,  // $1 per million input tokens
            output: 2.0, // $2 per million output tokens
            cache_read: 0.0,
            cache_write: 0.0,
        };
        let chunk_usage = openai_rust::types::Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_tokens_details: None,
        };
        let mut msg = empty_assistant_message();
        apply_chunk_usage(&chunk_usage, &model, &mut msg);
        assert_eq!(msg.usage.input, 100);
        assert_eq!(msg.usage.output, 50);
        assert_eq!(msg.usage.cache_read, 0);
        assert_eq!(msg.usage.total_tokens, 150);
        // 100 * 1.0/1M + 50 * 2.0/1M = 0.0001 + 0.0001 = 0.0002
        assert!((msg.usage.cost.total - 0.0002).abs() < 1e-9);
    }

    /// When the chunk reports `prompt_tokens_details.cached_tokens`,
    /// the cached portion must be subtracted from billed `input` and
    /// surfaced under `cache_read` — matching the convention the
    /// Google/Anthropic providers established in this crate.
    #[test]
    fn apply_chunk_usage_subtracts_cached_tokens_from_input() {
        let model = test_model(Provider::OpenAI);
        let chunk_usage = openai_rust::types::Usage {
            prompt_tokens: 1000,
            completion_tokens: 50,
            total_tokens: 1050,
            prompt_tokens_details: Some(openai_rust::types::PromptTokensDetails {
                cached_tokens: 800,
            }),
        };
        let mut msg = empty_assistant_message();
        apply_chunk_usage(&chunk_usage, &model, &mut msg);
        assert_eq!(msg.usage.input, 200, "billed input drops cache portion");
        assert_eq!(msg.usage.cache_read, 800);
        assert_eq!(msg.usage.output, 50);
        assert_eq!(msg.usage.total_tokens, 1050);
    }

    /// Pathological provider quirk: cached tokens reported as larger
    /// than the prompt total. Must not panic — saturating_sub clamps
    /// `input` to zero and the cache_read still surfaces the raw
    /// value for diagnostics.
    #[test]
    fn apply_chunk_usage_handles_cached_greater_than_prompt() {
        let model = test_model(Provider::OpenAI);
        let chunk_usage = openai_rust::types::Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            prompt_tokens_details: Some(openai_rust::types::PromptTokensDetails {
                cached_tokens: 200,
            }),
        };
        let mut msg = empty_assistant_message();
        apply_chunk_usage(&chunk_usage, &model, &mut msg);
        assert_eq!(msg.usage.input, 0, "saturating_sub clamps to zero");
        assert_eq!(msg.usage.cache_read, 200);
    }

    fn affinity_compat(send: bool) -> ResolvedCompat {
        use crate::types::OpenRouterRouting;
        let _ = OpenRouterRouting::default();
        // Build via detect_compat for stable defaults, then override
        // the field under test.
        let model = test_model(Provider::OpenAI);
        let mut compat = detect_compat(&model);
        compat.send_session_affinity_headers = send;
        compat
    }

    /// Default behaviour: affinity headers are off so direct OpenAI
    /// calls don't ship the three headers (the prompt_cache_key body
    /// field handles cache affinity instead). The gate applies to
    /// every format, including OpenRouter's `x-session-id`.
    #[test]
    fn session_affinity_headers_off_by_default() {
        for format in [
            crate::types::SessionAffinityFormat::OpenAI,
            crate::types::SessionAffinityFormat::OpenRouter,
        ] {
            let mut compat = affinity_compat(false);
            compat.session_affinity_format = format;
            let headers = resolve_session_affinity_headers(&compat, Some("sess-abc"), None);
            assert!(
                headers.is_empty(),
                "default off must return no headers: {headers:?}"
            );
        }
    }

    /// When compat opts in AND the caller supplies a non-empty
    /// session id, all three known affinity headers are emitted.
    #[test]
    fn session_affinity_headers_emit_when_compat_opts_in() {
        let compat = affinity_compat(true);
        let headers = resolve_session_affinity_headers(&compat, Some("sess-abc"), None);
        assert_eq!(headers.len(), 3);
        for (name, value) in &headers {
            assert_eq!(value, "sess-abc", "{name} carries the session id");
        }
        let names: std::collections::HashSet<&str> =
            headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains("session_id"));
        assert!(names.contains("x-client-request-id"));
        assert!(names.contains("x-session-affinity"));
    }

    /// `CacheRetention::None` is the explicit cache-off opt-out —
    /// affinity headers are pointless because no caching happens.
    /// Drop them.
    #[test]
    fn session_affinity_headers_dropped_when_caching_disabled() {
        use crate::types::CacheRetention;
        let compat = affinity_compat(true);
        let headers =
            resolve_session_affinity_headers(&compat, Some("sess-abc"), Some(CacheRetention::None));
        assert!(headers.is_empty());
    }

    /// No session id means no affinity headers — they'd carry no
    /// signal for the proxy to route on. Holds for every format.
    #[test]
    fn session_affinity_headers_dropped_without_session_id() {
        use crate::types::SessionAffinityFormat;
        for format in [
            SessionAffinityFormat::OpenAI,
            SessionAffinityFormat::OpenAINoSession,
            SessionAffinityFormat::OpenRouter,
        ] {
            let mut compat = affinity_compat(true);
            compat.session_affinity_format = format;
            assert!(resolve_session_affinity_headers(&compat, None, None).is_empty());
            assert!(resolve_session_affinity_headers(&compat, Some(""), None).is_empty());
        }
    }

    /// OpenRouter reads the session id from a single `x-session-id`
    /// header; none of the OpenAI-style headers may leak through.
    #[test]
    fn session_affinity_openrouter_format_sends_only_x_session_id() {
        let mut compat = affinity_compat(true);
        compat.session_affinity_format = crate::types::SessionAffinityFormat::OpenRouter;
        let headers = resolve_session_affinity_headers(&compat, Some("sess-abc"), None);
        assert_eq!(
            headers,
            vec![("x-session-id".to_string(), "sess-abc".to_string())]
        );
    }

    /// The no-session variant keeps the two dash-separated headers but
    /// drops `session_id`, for proxies that reject underscore header
    /// names.
    #[test]
    fn session_affinity_nosession_format_omits_session_id_header() {
        let mut compat = affinity_compat(true);
        compat.session_affinity_format = crate::types::SessionAffinityFormat::OpenAINoSession;
        let headers = resolve_session_affinity_headers(&compat, Some("sess-abc"), None);
        assert_eq!(
            headers,
            vec![
                ("x-client-request-id".to_string(), "sess-abc".to_string()),
                ("x-session-affinity".to_string(), "sess-abc".to_string()),
            ]
        );
    }

    /// `detect_compat` picks the OpenRouter format from the provider or
    /// an openrouter.ai base URL; everything else defaults to the
    /// OpenAI-style header set.
    #[test]
    fn session_affinity_format_autodetected_from_endpoint() {
        use crate::types::SessionAffinityFormat;

        let openai = test_model(Provider::OpenAI);
        assert_eq!(
            detect_compat(&openai).session_affinity_format,
            SessionAffinityFormat::OpenAI
        );

        let by_provider = test_model(Provider::Openrouter);
        assert_eq!(
            detect_compat(&by_provider).session_affinity_format,
            SessionAffinityFormat::OpenRouter
        );

        let mut by_url = test_model(Provider::OpenAI);
        by_url.base_url = "https://openrouter.ai/api/v1".to_string();
        assert_eq!(
            detect_compat(&by_url).session_affinity_format,
            SessionAffinityFormat::OpenRouter
        );
    }

    /// OpenRouter normalizes reasoning across providers via a nested
    /// `reasoning: { effort: ... }` object. When the caller passes an
    /// effort, hand-ai must forward the mapped value; when they don't,
    /// emit `effort: "none"` so the router doesn't burn thinking
    /// tokens by default. The SDK's top-level `reasoning_effort`
    /// field is the wrong shape and must NOT be emitted.
    #[test]
    fn build_params_emits_openrouter_reasoning_object_when_effort_set() {
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = true;
        model.base_url = "https://openrouter.ai/api/v1".to_string();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(openai_rust::types::ReasoningEffort::High),
            ..OpenAICompletionsOptions::default()
        };

        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(
            body["reasoning"],
            serde_json::json!({ "effort": "high" }),
            "openrouter expects nested reasoning object: {body}"
        );
        // The top-level `reasoning_effort` field would be the wrong
        // shape for OpenRouter and must NOT appear in the body.
        assert!(
            body.get("reasoning_effort").is_none(),
            "openrouter must not emit top-level reasoning_effort: {body}"
        );
    }

    /// Explicit-off: when reasoning is unset on an openrouter
    /// reasoning model, send `{ effort: "none" }` so the router
    /// doesn't burn thinking tokens by default. Without this, the
    /// router would use its own default (often "high") and the user
    /// pays for unwanted thinking output.
    #[test]
    fn build_params_emits_openrouter_reasoning_none_when_effort_off() {
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = true;
        model.base_url = "https://openrouter.ai/api/v1".to_string();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();

        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(
            body["reasoning"],
            serde_json::json!({ "effort": "none" }),
            "openrouter explicit-off must emit effort: none: {body}"
        );
    }

    /// Non-reasoning OpenRouter models must NOT carry a `reasoning`
    /// field at all — the upstream rejects it on models that don't
    /// support thinking.
    #[test]
    fn build_params_omits_openrouter_reasoning_on_non_reasoning_models() {
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = false;
        model.base_url = "https://openrouter.ai/api/v1".to_string();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(openai_rust::types::ReasoningEffort::High),
            ..OpenAICompletionsOptions::default()
        };

        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert!(
            body.get("reasoning").is_none(),
            "non-reasoning model must not emit reasoning: {body}"
        );
    }

    /// The Vercel AI Gateway exposes provider routing via
    /// `providerOptions.gateway.{only, order}` on the request body —
    /// only honoured when the base URL targets the gateway. The
    /// compat block carries the routing struct; the request body
    /// must surface it under `providerOptions.gateway`.
    #[test]
    fn build_params_emits_vercel_gateway_routing_when_configured() {
        use crate::types::{Compat, OpenAICompletionsCompat, VercelGatewayRouting};
        let mut model = test_model(Provider::OpenAI);
        model.base_url = "https://ai-gateway.vercel.sh/v1".to_string();
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                vercel_gateway_routing: Some(VercelGatewayRouting {
                    only: Some(vec!["bedrock".to_string(), "anthropic".to_string()]),
                    order: Some(vec!["anthropic".to_string(), "bedrock".to_string()]),
                }),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(
            body["providerOptions"]["gateway"],
            serde_json::json!({
                "only": ["bedrock", "anthropic"],
                "order": ["anthropic", "bedrock"],
            }),
        );
    }

    /// Routing must NOT be emitted on non-Vercel base URLs — the
    /// gateway-specific shape would be rejected by other proxies.
    #[test]
    fn build_params_omits_vercel_gateway_routing_on_other_hosts() {
        use crate::types::{Compat, OpenAICompletionsCompat, VercelGatewayRouting};
        let mut model = test_model(Provider::OpenAI);
        model.base_url = "https://api.openai.com/v1".to_string();
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                vercel_gateway_routing: Some(VercelGatewayRouting {
                    only: Some(vec!["bedrock".to_string()]),
                    order: None,
                }),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert!(
            body.get("providerOptions").is_none(),
            "non-Vercel host must not emit providerOptions: {body}"
        );
    }

    /// An empty routing struct (both fields None) must not emit a
    /// stub `providerOptions: { gateway: {} }`; some Vercel routes
    /// reject the empty wrapper.
    #[test]
    fn build_params_omits_vercel_gateway_routing_when_struct_empty() {
        use crate::types::{Compat, OpenAICompletionsCompat, VercelGatewayRouting};
        let mut model = test_model(Provider::OpenAI);
        model.base_url = "https://ai-gateway.vercel.sh/v1".to_string();
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                vercel_gateway_routing: Some(VercelGatewayRouting::default()),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert!(
            body.get("providerOptions").is_none(),
            "empty gateway routing must not emit providerOptions: {body}"
        );
    }

    /// Z.ai exposes incremental tool-call streaming via a top-level
    /// `tool_stream: true` flag. The flag must fire when the compat
    /// detector flags the model AND the request carries tools — but
    /// NOT on history-only / no-tools shapes where the request only
    /// synthesizes an empty `tools: []` for proxy parity.
    #[test]
    fn build_params_emits_tool_stream_for_zai_when_tools_present() {
        use crate::types::{Compat, OpenAICompletionsCompat, Tool};
        let mut model = test_model(Provider::Zai);
        model.base_url = "https://api.z.ai/api/coding/paas/v4".to_string();
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                zai_tool_stream: Some(true),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![Tool::new(
                "read",
                "Read a file",
                serde_json::json!({"type": "object", "properties": {}}),
            )]),
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(
            body["tool_stream"],
            serde_json::Value::Bool(true),
            "zai_tool_stream + tools must emit tool_stream: true: {body}"
        );
    }

    /// Without `zai_tool_stream` (the default for non-z.ai compat),
    /// the flag must NOT appear in the request body even with tools
    /// present. Most upstreams reject unknown top-level fields.
    #[test]
    fn build_params_omits_tool_stream_when_zai_compat_off() {
        use crate::types::Tool;
        let model = test_model(Provider::OpenAI);
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![Tool::new(
                "read",
                "Read a file",
                serde_json::json!({"type": "object", "properties": {}}),
            )]),
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert!(
            body.get("tool_stream").is_none(),
            "non-zai compat must NOT emit tool_stream: {body}"
        );
    }

    /// `tool_stream` is meaningless when there's nothing to stream;
    /// suppress it on history-only requests that send `tools: []`
    /// to satisfy proxy parity, otherwise the upstream may reject
    /// the request as malformed.
    #[test]
    fn build_params_omits_tool_stream_when_no_tools_in_request() {
        use crate::types::{Compat, OpenAICompletionsCompat};
        let mut model = test_model(Provider::Zai);
        model.base_url = "https://api.z.ai/api/coding/paas/v4".to_string();
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                zai_tool_stream: Some(true),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert!(
            body.get("tool_stream").is_none(),
            "zai_tool_stream without tools must NOT emit tool_stream: {body}"
        );
    }

    /// Local Qwen-compatible servers (vLLM, llama.cpp) read thinking
    /// knobs nested under `chat_template_kwargs`. The compat hook
    /// `thinkingFormat: "qwen-chat-template"` opts in to this layout;
    /// `preserve_thinking: true` keeps prior turns' thinking available
    /// so multi-turn tool calls don't degrade to empty `{}` payloads.
    #[test]
    fn build_params_emits_chat_template_kwargs_for_qwen_chat_template() {
        use crate::types::{Compat, OpenAICompletionsCompat};
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = true;
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                thinking_format: Some("qwen-chat-template".to_string()),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(openai_rust::types::ReasoningEffort::Medium),
            ..OpenAICompletionsOptions::default()
        };

        let params = build_params(&model, &context, &options).expect("build_params ok");
        let body = serde_json::to_value(&params).expect("serialize ok");
        let kwargs = &body["chat_template_kwargs"];
        assert_eq!(kwargs["enable_thinking"], serde_json::Value::Bool(true));
        assert_eq!(kwargs["preserve_thinking"], serde_json::Value::Bool(true));
    }

    /// When reasoning is disabled (no `reasoning_effort` set), the
    /// chat-template still emits `enable_thinking: false` so the
    /// upstream toggles thinking off — without the field the server
    /// falls back to its default which differs per build.
    #[test]
    fn build_params_qwen_chat_template_disables_thinking_without_effort() {
        use crate::types::{Compat, OpenAICompletionsCompat};
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = true;
        model.compat = Some(Compat::OpenAICompletions(Box::new(
            OpenAICompletionsCompat {
                thinking_format: Some("qwen-chat-template".to_string()),
                ..Default::default()
            },
        )));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions::default();

        let params = build_params(&model, &context, &options).expect("build_params ok");
        let body = serde_json::to_value(&params).expect("serialize ok");
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(
            body["chat_template_kwargs"]["preserve_thinking"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn test_normalize_mistral_tool_id_short() {
        assert_eq!(normalize_mistral_tool_id("abc"), "abcABCDEF");
    }

    #[test]
    fn test_normalize_mistral_tool_id_exact() {
        assert_eq!(normalize_mistral_tool_id("123456789"), "123456789");
    }

    #[test]
    fn test_normalize_mistral_tool_id_long() {
        assert_eq!(normalize_mistral_tool_id("abcdefghijklmnop"), "abcdefghi");
    }

    #[test]
    fn test_normalize_mistral_tool_id_strips_non_alnum() {
        assert_eq!(normalize_mistral_tool_id("a-b_c!d@e"), "abcdeABCD");
    }

    #[test]
    fn test_sanitize_surrogates_clean() {
        assert_eq!(sanitize_surrogates("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_surrogates_with_unicode() {
        assert_eq!(sanitize_surrogates("你好世界"), "你好世界");
    }

    #[test]
    fn test_parse_streaming_json_valid() {
        let result = parse_streaming_json(r#"{"key": "value"}"#);
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_parse_streaming_json_empty_object() {
        let result = parse_streaming_json("{invalid");
        assert_eq!(result, serde_json::json!({}));
    }

    #[test]
    fn test_parse_streaming_json_non_object() {
        let result = parse_streaming_json("hello");
        assert_eq!(result, serde_json::json!("hello"));
    }

    /// Regression: an empty arguments stream (model emitted a tool call
    /// without any input deltas — common for zero-arg tools) must produce
    /// `{}`, not `""`. Returning a JSON string here breaks downstream
    /// schema validation ("" is not of type "object") and surfaces as a
    /// silent tool-call failure with a confusing "Invalid arguments"
    /// error message.
    #[test]
    fn parse_streaming_json_empty_input_returns_empty_object() {
        let result = parse_streaming_json("");
        assert_eq!(result, serde_json::json!({}));
    }

    /// Tool-call argument streams sometimes contain raw control bytes or
    /// invalid backslash escapes from the model. Plain `from_str` rejects
    /// them and historically dropped the entire payload to `{}`, silently
    /// breaking the tool call. The shared repair pass lets us recover the
    /// structured args instead.
    #[test]
    fn parse_streaming_json_recovers_malformed_payload_via_repair() {
        // Raw tab inside the string + invalid `\H` escape.
        let input = "{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}";
        let result = parse_streaming_json(input);
        assert_eq!(result["path"], "A\\H");
        assert_eq!(result["text"], "col1\tcol2");
    }

    /// Whitespace-only input is treated the same as empty — the model
    /// flushed a no-op buffer with only " " / "\n" between deltas.
    #[test]
    fn parse_streaming_json_whitespace_only_returns_empty_object() {
        assert_eq!(parse_streaming_json("   "), serde_json::json!({}));
        assert_eq!(parse_streaming_json("\n\t  "), serde_json::json!({}));
    }

    #[test]
    fn test_map_stop_reason_stop() {
        assert_eq!(map_stop_reason("stop"), StopReason::Stop);
    }

    #[test]
    fn test_map_stop_reason_length() {
        assert_eq!(map_stop_reason("length"), StopReason::Length);
    }

    #[test]
    fn test_map_stop_reason_tool_calls() {
        assert_eq!(map_stop_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("function_call"), StopReason::ToolUse);
    }

    #[test]
    fn test_map_stop_reason_unknown() {
        assert_eq!(map_stop_reason("unknown_reason"), StopReason::Stop);
    }

    /// A `finish_reason` from the provider is authoritative, including
    /// the reasons that disagree with the content — `length` on a
    /// message that still managed to emit a tool call must stay
    /// `Length`, since that is what makes the caller treat the call's
    /// arguments as truncated.
    #[test]
    fn reported_finish_reason_wins_over_inference() {
        let with_tool_call = vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "call-1",
            "lookup",
            serde_json::json!({}),
        ))];
        assert_eq!(
            resolve_stop_reason(true, StopReason::Length, &with_tool_call),
            StopReason::Length
        );
        assert_eq!(
            resolve_stop_reason(true, StopReason::Stop, &with_tool_call),
            StopReason::Stop
        );
    }

    /// Endpoints that close the stream without ever sending a
    /// `finish_reason` used to leave the pre-stream `Stop` default in
    /// place, reporting a self-terminated turn even though the model
    /// asked for tools. Infer the reason from what arrived instead.
    #[test]
    fn missing_finish_reason_infers_tool_use_from_content() {
        let content = vec![
            AssistantContentBlock::Text(TextContent::new("on it")),
            AssistantContentBlock::ToolCall(ToolCall::new(
                "call-1",
                "lookup",
                serde_json::json!({"q": "x"}),
            )),
        ];
        assert_eq!(
            resolve_stop_reason(false, StopReason::Stop, &content),
            StopReason::ToolUse
        );
    }

    /// Without tool calls a missing `finish_reason` is an ordinary end
    /// of turn, and an empty stream is too.
    #[test]
    fn missing_finish_reason_without_tool_calls_is_stop() {
        let text_only = vec![AssistantContentBlock::Text(TextContent::new("done"))];
        assert_eq!(
            resolve_stop_reason(false, StopReason::Stop, &text_only),
            StopReason::Stop
        );
        assert_eq!(
            resolve_stop_reason(false, StopReason::Stop, &[]),
            StopReason::Stop
        );
    }

    #[test]
    fn test_detect_compat_openai() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        assert!(compat.supports_store);
        assert!(compat.supports_developer_role);
        assert!(compat.supports_reasoning_effort);
        assert!(!compat.requires_mistral_tool_ids);
    }

    #[test]
    fn test_detect_compat_mistral() {
        let mut model = test_model(Provider::Mistral);
        model.base_url = "https://api.mistral.ai".to_string();
        let compat = detect_compat(&model);
        assert!(!compat.supports_store);
        assert!(compat.requires_tool_result_name);
        assert!(compat.requires_thinking_as_text);
        assert!(compat.requires_mistral_tool_ids);
        assert_eq!(compat.max_tokens_field, Some("max_tokens".to_string()));
    }

    #[test]
    fn test_detect_compat_xai() {
        let model = test_model(Provider::Xai);
        let compat = detect_compat(&model);
        assert!(!compat.supports_store);
        assert!(!compat.supports_reasoning_effort);
    }

    /// Moonshot's OpenAI-compatible endpoint (Kimi family) rejects
    /// `reasoning_effort`, OpenAI strict tool mode, `store`, and the
    /// `developer` role, and requires `max_tokens` instead of
    /// `max_completion_tokens`. Pin the detection across the global
    /// provider, the China-region provider, and the public base URL.
    #[test]
    fn test_detect_compat_moonshot_disables_unsupported_openai_features() {
        for provider in [Provider::Moonshotai, Provider::MoonshotaiCn] {
            let mut model = test_model(provider);
            model.base_url = "https://api.moonshot.cn/v1".to_string();
            let compat = detect_compat(&model);
            assert!(
                !compat.supports_store,
                "moonshot {:?} must not advertise store",
                provider
            );
            assert!(
                !compat.supports_developer_role,
                "moonshot {:?} must not use developer role",
                provider
            );
            assert!(
                !compat.supports_reasoning_effort,
                "moonshot {:?} must not send reasoning_effort",
                provider
            );
            assert!(
                !compat.supports_strict_mode,
                "moonshot {:?} must not enable strict tool mode",
                provider
            );
            assert_eq!(
                compat.max_tokens_field,
                Some("max_tokens".to_string()),
                "moonshot {:?} must use max_tokens",
                provider
            );
        }
    }

    /// The base-URL fallback for Moonshot must catch proxies that route
    /// through the public `api.moonshot.*` host even when the provider
    /// enum is something else (e.g. a generic OpenAI-compatible).
    #[test]
    fn test_detect_compat_moonshot_via_base_url() {
        let mut model = test_model(Provider::OpenAI);
        model.base_url = "https://api.moonshot.ai/v1".to_string();
        let compat = detect_compat(&model);
        assert!(!compat.supports_reasoning_effort);
        assert!(!compat.supports_strict_mode);
        assert_eq!(compat.max_tokens_field, Some("max_tokens".to_string()));
    }

    #[test]
    fn test_convert_tool_creates_openai_tool() {
        let tool = Tool {
            name: "calculator".to_string(),
            description: "A calculator".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let openai_tool = convert_tool(&tool);
        assert_eq!(openai_tool.tool_type, "function");
        assert_eq!(openai_tool.function.name, "calculator");
    }

    #[test]
    fn test_has_tool_history_empty() {
        assert!(!has_tool_history(&[]));
    }

    #[test]
    fn test_has_tool_history_with_tool_result() {
        let messages = vec![Message::ToolResult(crate::types::ToolResultMessage {
            role: "tool".to_string(),
            tool_call_id: "tc_1".to_string(),
            tool_name: "test".to_string(),
            content: vec![crate::types::ToolResultContent::Text(TextContent::new(
                "result".to_string(),
            ))],
            details: None,
            is_error: false,
            timestamp: 0,
        })];
        assert!(has_tool_history(&messages));
    }

    #[test]
    fn test_has_tool_history_with_tool_call_in_assistant() {
        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                "tc_1",
                "test",
                serde_json::json!({}),
            ))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];
        assert!(has_tool_history(&messages));
    }

    #[test]
    fn test_clamp_reasoning_xhigh_to_high() {
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Xhigh)),
            Some(ThinkingLevel::High)
        );
    }

    #[test]
    fn test_clamp_reasoning_max_to_high() {
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Max)),
            Some(ThinkingLevel::High)
        );
    }

    /// The wire enum has no effort above `high`; both extended
    /// levels map onto it.
    #[test]
    fn test_map_thinking_level_clamps_extended_levels() {
        assert_eq!(
            map_thinking_level(ThinkingLevel::Xhigh),
            openai_rust::types::ReasoningEffort::High
        );
        assert_eq!(
            map_thinking_level(ThinkingLevel::Max),
            openai_rust::types::ReasoningEffort::High
        );
    }

    #[test]
    fn test_clamp_reasoning_low_unchanged() {
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Low)),
            Some(ThinkingLevel::Low)
        );
    }

    #[test]
    fn test_clamp_reasoning_none() {
        assert_eq!(clamp_reasoning(None), None);
    }

    #[test]
    fn test_convert_messages_system_prompt() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        let context = Context {
            system_prompt: Some("You are helpful.".to_string()),
            messages: vec![Message::User(UserMessage::new_text("Hello"))],
            tools: None,
        };
        let msgs = convert_messages(&model, &context, &compat);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, Role::System));
    }

    /// `requires_thinking_as_text` compat replay must preserve every original
    /// assistant text block alongside the thinking text as discrete
    /// `{type: "text"}` content parts. Joining them into one string corrupts
    /// same-model replays for providers (e.g. llama.cpp + gpt-oss) that key on
    /// the multi-part shape — they crash when prior assistant messages mix
    /// thinking and text.
    #[test]
    fn assistant_with_thinking_and_text_emits_array_parts_when_compat_requires() {
        use crate::types::{AssistantContentBlock, TextContent, ThinkingContent};
        let mut model = test_model(Provider::Mistral);
        model.base_url = "https://api.mistral.ai".to_string();
        let compat = detect_compat(&model);
        assert!(
            compat.requires_thinking_as_text,
            "mistral baseline must require thinking-as-text"
        );

        let asst = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("inner reasoning")),
                AssistantContentBlock::Text(TextContent::new("visible answer")),
            ],
            api: Api::OpenAICompletions,
            provider: Provider::Mistral,
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("hi")),
                Message::Assistant(asst),
            ],
            tools: None,
        };
        let msgs = convert_messages(&model, &context, &compat);
        let assistant_msg = msgs
            .iter()
            .find(|m| matches!(m.role, Role::Assistant))
            .expect("assistant message present");
        match &assistant_msg.content {
            Content::Array(parts) => {
                assert_eq!(parts.len(), 2, "expected two text parts, got {parts:?}");
                match &parts[0] {
                    ContentPart::Text { text } => assert_eq!(text, "inner reasoning"),
                    other => panic!("part[0] must be text, got {other:?}"),
                }
                match &parts[1] {
                    ContentPart::Text { text } => assert_eq!(text, "visible answer"),
                    other => panic!("part[1] must be text, got {other:?}"),
                }
            }
            other => panic!("assistant content must be array, got {other:?}"),
        }
    }

    /// When an assistant turn produces multiple tool_use calls and each
    /// tool result returns text + image, the converter must emit each
    /// result as its own `tool` role message (text only) and then batch
    /// every image into a single trailing synthetic `user` message with
    /// `image_url` parts — OpenAI Completions rejects images inside
    /// the `tool` role.
    #[test]
    fn convert_messages_batches_tool_result_images_after_consecutive_tools() {
        use crate::types::{
            AssistantContentBlock, ImageContent, TextContent, ToolCall, ToolResultContent,
            ToolResultMessage,
        };

        let mut model = test_model(Provider::OpenAI);
        model.input = vec![InputType::Text, InputType::Image];
        let compat = detect_compat(&model);

        let now = 1_000_000u64;
        let assistant = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![
                AssistantContentBlock::ToolCall(ToolCall::new(
                    "tool-1",
                    "read",
                    serde_json::json!({"path":"img-1.png"}),
                )),
                AssistantContentBlock::ToolCall(ToolCall::new(
                    "tool-2",
                    "read",
                    serde_json::json!({"path":"img-2.png"}),
                )),
            ],
            api: model.api,
            provider: model.provider,
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: now,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };

        let make_tool_result = |id: &str, ts: u64| ToolResultMessage {
            role: "toolResult".to_string(),
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            content: vec![
                ToolResultContent::Text(TextContent::new("Read image file [image/png]")),
                ToolResultContent::Image(ImageContent::new("ZmFrZQ==", "image/png")),
            ],
            details: None,
            is_error: false,
            timestamp: ts,
        };

        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("Read the images")),
                Message::Assistant(assistant),
                Message::ToolResult(make_tool_result("tool-1", now + 1)),
                Message::ToolResult(make_tool_result("tool-2", now + 2)),
            ],
            tools: None,
        };

        let msgs = convert_messages(&model, &context, &compat);
        let roles: Vec<&'static str> = msgs
            .iter()
            .map(|m| match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
                Role::System => "system",
            })
            .collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "user"]);

        let trailing = msgs.last().expect("trailing user message");
        let parts = match &trailing.content {
            Content::Array(parts) => parts,
            other => panic!("expected Array content, got {other:?}"),
        };
        let image_count = parts
            .iter()
            .filter(|p| matches!(p, ContentPart::ImageUrl { .. }))
            .count();
        assert_eq!(image_count, 2, "both images must be batched");
    }

    fn tool_call_assistant(model: &Model, tool_name: &str) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                "tool-1",
                tool_name,
                serde_json::json!({}),
            ))],
            api: model.api,
            provider: model.provider,
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// Empty tool results (no text, no images) must ship the explicit
    /// "(no tool output)" placeholder instead of empty content — some
    /// providers reject empty tool content, and the model otherwise
    /// can't tell the tool ran and returned nothing.
    #[test]
    fn convert_messages_empty_tool_result_uses_no_output_placeholder() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);

        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("Run the command")),
                Message::Assistant(tool_call_assistant(&model, "bash")),
                Message::ToolResult(ToolResultMessage::new(
                    "tool-1",
                    "bash",
                    vec![ToolResultContent::Text(TextContent::new(""))],
                )),
            ],
            tools: None,
        };

        let msgs = convert_messages(&model, &context, &compat);
        let roles: Vec<&'static str> = msgs
            .iter()
            .map(|m| match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
                Role::System => "system",
            })
            .collect();
        // No synthetic trailing user message — there is no image to batch.
        assert_eq!(roles, vec!["user", "assistant", "tool"]);
        let tool_msg = msgs
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .expect("tool message present");
        match &tool_msg.content {
            Content::Text(s) => assert_eq!(s, "(no tool output)"),
            other => panic!("expected plain string tool content, got {other:?}"),
        }
    }

    /// Image-only tool results keep the "(see attached image)" pointer —
    /// the "(no tool output)" placeholder applies only when the result
    /// has neither text nor images. The image itself still lands in the
    /// trailing synthetic user message.
    #[test]
    fn convert_messages_image_only_tool_result_keeps_image_placeholder() {
        use crate::types::ImageContent;

        let mut model = test_model(Provider::OpenAI);
        model.input = vec![InputType::Text, InputType::Image];
        let compat = detect_compat(&model);

        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("Take a screenshot")),
                Message::Assistant(tool_call_assistant(&model, "screenshot")),
                Message::ToolResult(ToolResultMessage::new(
                    "tool-1",
                    "screenshot",
                    vec![ToolResultContent::Image(ImageContent::new(
                        "ZmFrZQ==",
                        "image/png",
                    ))],
                )),
            ],
            tools: None,
        };

        let msgs = convert_messages(&model, &context, &compat);
        let tool_msg = msgs
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .expect("tool message present");
        match &tool_msg.content {
            Content::Text(s) => assert_eq!(s, "(see attached image)"),
            other => panic!("expected plain string tool content, got {other:?}"),
        }
        let trailing = msgs.last().expect("trailing user message");
        assert!(matches!(trailing.role, Role::User));
        let parts = match &trailing.content {
            Content::Array(parts) => parts,
            other => panic!("expected Array content, got {other:?}"),
        };
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
            "image must be batched into the trailing user message: {parts:?}"
        );
    }

    /// Assistant turns with text-only content must serialize as a plain
    /// string. The legacy `Content::Array([{type: "text", text: ...}])`
    /// shape is non-standard for `role: "assistant"` and triggers
    /// mirrored-structure output on some hosted gateways (DeepSeek V3.2
    /// via NVIDIA NIM recursively echoes the wrapper as literal text in
    /// the reply). This test pins the plain-string shape.
    #[test]
    fn assistant_text_only_content_serializes_as_plain_string() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        let assistant = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new("hi there"))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        let context = Context {
            system_prompt: None,
            messages: vec![Message::Assistant(assistant)],
            tools: None,
        };
        let msgs = convert_messages(&model, &context, &compat);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].role, Role::Assistant));
        match &msgs[0].content {
            openai_rust::types::Content::Text(s) => assert_eq!(s, "hi there"),
            other => panic!("expected plain string content, got {other:?}"),
        }
    }

    #[test]
    fn test_convert_messages_no_system_prompt() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("Hello"))],
            tools: None,
        };
        let msgs = convert_messages(&model, &context, &compat);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].role, Role::User));
    }

    // -------------------------------------------------------------------------
    // Streaming-delta regression coverage
    // -------------------------------------------------------------------------

    fn empty_output() -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn text_delta(text: &str) -> openai_rust::types::Delta {
        // The Delta type has private fields and a custom Deserialize impl, so
        // we round-trip through JSON to construct one in tests. This mirrors
        // what the SSE parser does at runtime.
        let json = serde_json::json!({ "content": text });
        serde_json::from_value(json).expect("valid delta")
    }

    fn reasoning_delta(text: &str) -> openai_rust::types::Delta {
        let json = serde_json::json!({ "reasoning": text });
        serde_json::from_value(json).expect("valid delta")
    }

    /// Regression guard: prior to this fix the provider only persisted the
    /// FIRST `delta.content` chunk into `output.content` — all subsequent
    /// chunks were accumulated in a local `CurrentBlock` buffer that was
    /// dropped on flush. So `say hi briefly` -> "Hi" + "!" + " 👋" came back
    /// as just "Hi" and the TUI rendered nothing because no `TextDelta`
    /// events were ever yielded between Start and Done either.
    #[test]
    fn streaming_text_deltas_accumulate_into_output_and_emit_events() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;

        let mut all_events = Vec::new();
        for piece in ["Hi", "!", " 👋"] {
            let d = text_delta(piece);
            all_events.extend(handle_delta(&d, &mut current, &mut output));
        }
        all_events.extend(finish_current_block(&mut current, &mut output));

        // Output buffer should hold the FULL concatenated text, not just the
        // first chunk.
        assert_eq!(output.content.len(), 1);
        match &output.content[0] {
            AssistantContentBlock::Text(t) => assert_eq!(t.text, "Hi! 👋"),
            other => panic!("expected text block, got {other:?}"),
        }

        // Event tape should be: TextStart, TextDelta×3, TextEnd.
        let tags: Vec<&'static str> = all_events
            .iter()
            .map(|e| match e {
                AssistantMessageEvent::TextStart { .. } => "start",
                AssistantMessageEvent::TextDelta { .. } => "delta",
                AssistantMessageEvent::TextEnd { .. } => "end",
                _ => "other",
            })
            .collect();
        assert_eq!(tags, vec!["start", "delta", "delta", "delta", "end"]);

        // The per-chunk delta strings must round-trip verbatim.
        let deltas: Vec<&str> = all_events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::TextDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hi", "!", " 👋"]);
    }

    /// Build a `tool_calls` delta chunk that matches the wire format
    /// emitted by OpenAI / OpenRouter / Deepseek / etc. Subsequent chunks
    /// for the same tool call MUST share the same `index`; `id` is
    /// typically only set on the first chunk.
    fn tool_call_delta(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> openai_rust::types::Delta {
        let mut tc = serde_json::Map::new();
        tc.insert("index".into(), serde_json::json!(index));
        if let Some(id) = id {
            tc.insert("id".into(), serde_json::json!(id));
        }
        tc.insert("type".into(), serde_json::json!("function"));
        let mut fn_obj = serde_json::Map::new();
        if let Some(name) = name {
            fn_obj.insert("name".into(), serde_json::json!(name));
        }
        if let Some(args) = args {
            fn_obj.insert("arguments".into(), serde_json::json!(args));
        }
        tc.insert("function".into(), serde_json::Value::Object(fn_obj));
        let json = serde_json::json!({
            "tool_calls": [serde_json::Value::Object(tc)]
        });
        serde_json::from_value(json).expect("valid delta")
    }

    /// Regression for the issue where a streamed tool call was split into
    /// multiple `AssistantContentBlock::ToolCall` blocks because subsequent
    /// argument chunks omit `id` (per OpenAI protocol). The pre-fix logic
    /// compared on `id` and treated every subsequent chunk as a NEW tool,
    /// producing a final message with N ToolCall blocks (one per chunk)
    /// where the trailing ones had empty `name` / `id`.
    ///
    /// Reproduces the live wire shape captured from openrouter:
    ///   chunk 1: {index:0, id:"call_abc", function:{name:"read",arguments:""}}
    ///   chunk 2: {index:0,               function:{arguments:"{\"pa"}}
    ///   chunk 3: {index:0,               function:{arguments:"th\":\""}}
    ///   chunk 4: {index:0,               function:{arguments:"/tmp/x\"}"}}
    #[test]
    fn streaming_tool_call_chunks_merge_into_single_block() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;
        let mut events = Vec::new();
        events.extend(handle_delta(
            &tool_call_delta(0, Some("call_abc"), Some("read"), Some("")),
            &mut current,
            &mut output,
        ));
        events.extend(handle_delta(
            &tool_call_delta(0, None, None, Some("{\"pa")),
            &mut current,
            &mut output,
        ));
        events.extend(handle_delta(
            &tool_call_delta(0, None, None, Some("th\":\"")),
            &mut current,
            &mut output,
        ));
        events.extend(handle_delta(
            &tool_call_delta(0, None, None, Some("/tmp/x\"}")),
            &mut current,
            &mut output,
        ));
        events.extend(finish_current_block(&mut current, &mut output));

        // Exactly ONE tool-call block, name + id + parsed args all set.
        assert_eq!(output.content.len(), 1, "should be a single ToolCall block");
        match &output.content[0] {
            AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.id, "call_abc");
                assert_eq!(tc.name, "read");
                assert_eq!(tc.arguments, serde_json::json!({"path": "/tmp/x"}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // Event tape: one Start, three Deltas (one per arg chunk that
        // carried text), one End.
        let starts = events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallStart { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallEnd { .. }))
            .count();
        assert_eq!(starts, 1);
        assert_eq!(ends, 1);
    }

    /// When two tool calls arrive interleaved on different `index` slots
    /// (rare but allowed — happens when a model parallel-calls), each
    /// index must keep its own block. The pre-fix logic also got this
    /// case wrong because the `id != tool_id` heuristic happened to work
    /// for the first chunk of each — but the SECOND chunk for tool 0
    /// would create a new block instead of extending the existing one.
    #[test]
    fn streaming_parallel_tool_calls_keep_separate_blocks_per_index() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;
        let mut events = Vec::new();
        // Tool 0 first chunk.
        events.extend(handle_delta(
            &tool_call_delta(0, Some("call_a"), Some("read"), Some("{\"p\":\"x\"}")),
            &mut current,
            &mut output,
        ));
        // Tool 1 first chunk.
        events.extend(handle_delta(
            &tool_call_delta(1, Some("call_b"), Some("write"), Some("{\"q\":\"y\"}")),
            &mut current,
            &mut output,
        ));
        events.extend(finish_current_block(&mut current, &mut output));

        assert_eq!(output.content.len(), 2);
        match (&output.content[0], &output.content[1]) {
            (AssistantContentBlock::ToolCall(a), AssistantContentBlock::ToolCall(b)) => {
                assert_eq!(a.name, "read");
                assert_eq!(b.name, "write");
                assert_eq!(a.id, "call_a");
                assert_eq!(b.id, "call_b");
            }
            other => panic!("expected two ToolCalls, got {other:?}"),
        }
        let _ = events;
    }

    #[test]
    fn streaming_reasoning_then_text_produces_two_blocks() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;

        let mut events = Vec::new();
        events.extend(handle_delta(
            &reasoning_delta("We"),
            &mut current,
            &mut output,
        ));
        events.extend(handle_delta(
            &reasoning_delta(" need to respond"),
            &mut current,
            &mut output,
        ));
        events.extend(handle_delta(&text_delta("hi"), &mut current, &mut output));
        events.extend(finish_current_block(&mut current, &mut output));

        assert_eq!(output.content.len(), 2);
        match &output.content[0] {
            AssistantContentBlock::Thinking(t) => assert_eq!(t.thinking, "We need to respond"),
            other => panic!("expected thinking block, got {other:?}"),
        }
        match &output.content[1] {
            AssistantContentBlock::Text(t) => assert_eq!(t.text, "hi"),
            other => panic!("expected text block, got {other:?}"),
        }

        let tags: Vec<&'static str> = events
            .iter()
            .map(|e| match e {
                AssistantMessageEvent::ThinkingStart { .. } => "ts",
                AssistantMessageEvent::ThinkingDelta { .. } => "td",
                AssistantMessageEvent::ThinkingEnd { .. } => "te",
                AssistantMessageEvent::TextStart { .. } => "Ts",
                AssistantMessageEvent::TextDelta { .. } => "Td",
                AssistantMessageEvent::TextEnd { .. } => "Te",
                _ => "?",
            })
            .collect();
        // Modality switch from thinking -> text should close the thinking
        // block (ThinkingEnd) before opening the text block (TextStart).
        assert_eq!(tags, vec!["ts", "td", "td", "te", "Ts", "Td", "Te"]);
    }

    /// Some providers emit malformed mid-stream chunks that repeat
    /// `function.name` with a stale or wrong value after the first
    /// chunk already set the canonical name. Once a name is recorded
    /// from the first chunk, later chunks must NOT overwrite it —
    /// the parser must treat the first non-empty name as authoritative
    /// (mirroring the same id-preservation rule that's been in place).
    #[test]
    fn streaming_tool_call_preserves_first_name_against_later_overrides() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;
        let _ = handle_delta(
            &tool_call_delta(0, Some("call_abc"), Some("read"), Some("{")),
            &mut current,
            &mut output,
        );
        // Malformed later chunk repeats the name with a different
        // value. This must NOT silently rename the tool.
        let _ = handle_delta(
            &tool_call_delta(0, None, Some("write"), Some("\"a\":1}")),
            &mut current,
            &mut output,
        );
        let _ = finish_current_block(&mut current, &mut output);

        assert_eq!(output.content.len(), 1);
        match &output.content[0] {
            AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.id, "call_abc");
                assert_eq!(tc.name, "read", "first chunk's name must be authoritative");
                assert_eq!(tc.arguments, serde_json::json!({"a": 1}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn finish_current_block_with_no_block_is_noop() {
        let mut output = empty_output();
        let mut current: Option<CurrentBlock> = None;
        let events = finish_current_block(&mut current, &mut output);
        assert!(events.is_empty());
        assert!(output.content.is_empty());
    }

    /// `capture_chunk_metadata` records the chat completion id from
    /// the first chunk that carries one and never overwrites it after.
    #[test]
    fn capture_chunk_metadata_records_id_once() {
        let mut output = empty_output();
        capture_chunk_metadata("chatcmpl-abc", "gpt-4o", "gpt-4o", &mut output);
        assert_eq!(output.response_id.as_deref(), Some("chatcmpl-abc"));
        // A later chunk with a different id (shouldn't happen in
        // practice, but guard against it) must not clobber.
        capture_chunk_metadata("chatcmpl-different", "gpt-4o", "gpt-4o", &mut output);
        assert_eq!(output.response_id.as_deref(), Some("chatcmpl-abc"));
    }

    /// Empty chunk ids are ignored — some proxies emit a placeholder
    /// chunk before the real one with id populated.
    #[test]
    fn capture_chunk_metadata_skips_empty_id() {
        let mut output = empty_output();
        capture_chunk_metadata("", "gpt-4o", "gpt-4o", &mut output);
        assert_eq!(output.response_id, None);
        capture_chunk_metadata("chatcmpl-xyz", "gpt-4o", "gpt-4o", &mut output);
        assert_eq!(output.response_id.as_deref(), Some("chatcmpl-xyz"));
    }

    /// `response_model` is set ONLY when the served model differs from
    /// the requested one. OpenRouter's `auto` route returns concrete
    /// ids like `anthropic/claude-...` and callers rely on this field
    /// to know what routing actually picked.
    #[test]
    fn capture_chunk_metadata_records_routed_model() {
        let mut output = empty_output();
        capture_chunk_metadata(
            "chatcmpl-1",
            "anthropic/claude-opus-4",
            "openrouter/auto",
            &mut output,
        );
        assert_eq!(
            output.response_model.as_deref(),
            Some("anthropic/claude-opus-4")
        );
    }

    /// If the served model matches what was requested, `response_model`
    /// stays None — there is nothing interesting to surface.
    #[test]
    fn capture_chunk_metadata_skips_matching_model() {
        let mut output = empty_output();
        capture_chunk_metadata("chatcmpl-1", "gpt-4o", "gpt-4o", &mut output);
        assert_eq!(output.response_model, None);
    }

    /// Once `response_model` is set, later chunks must not overwrite
    /// it — only the first routed-model signal is authoritative.
    #[test]
    fn capture_chunk_metadata_does_not_overwrite_routed_model() {
        let mut output = empty_output();
        capture_chunk_metadata("chatcmpl-1", "anthropic/claude-opus-4", "auto", &mut output);
        capture_chunk_metadata(
            "chatcmpl-1",
            "anthropic/claude-sonnet-4",
            "auto",
            &mut output,
        );
        assert_eq!(
            output.response_model.as_deref(),
            Some("anthropic/claude-opus-4")
        );
    }

    /// An empty `chunk.model` must not populate response_model —
    /// some proxies omit the field entirely on early chunks.
    #[test]
    fn capture_chunk_metadata_skips_empty_model() {
        let mut output = empty_output();
        capture_chunk_metadata("chatcmpl-1", "", "gpt-4o", &mut output);
        assert_eq!(output.response_model, None);
    }

    /// A composite tool call id must normalize identically on the
    /// assistant side and the tool-response side — the API pairs tool
    /// messages to prior calls by exact id, so a one-sided rewrite
    /// orphans the pair.
    #[test]
    fn convert_messages_normalizes_tool_result_ids_to_match_assistant() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);

        let mut assistant = tool_call_assistant(&model, "bash");
        if let AssistantContentBlock::ToolCall(tc) = &mut assistant.content[0] {
            tc.id = "call_1|item_a".to_string();
        }
        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("run")),
                Message::Assistant(assistant),
                Message::ToolResult(ToolResultMessage::new(
                    "call_1|item_a",
                    "bash",
                    vec![ToolResultContent::Text(TextContent::new("ok"))],
                )),
            ],
            tools: None,
        };

        let msgs = convert_messages(&model, &context, &compat);
        let assistant_id = msgs
            .iter()
            .find_map(|m| m.tool_calls.as_ref())
            .and_then(|tcs| tcs.first())
            .map(|tc| tc.id.clone())
            .expect("assistant tool call present");
        let tool_id = msgs
            .iter()
            .find_map(|m| m.tool_call_id.clone())
            .expect("tool message present");
        assert_eq!(assistant_id, "call_1_item_a");
        assert_eq!(tool_id, assistant_id);
    }

    /// The assembled header set must combine catalog headers, session
    /// affinity headers, and the GitHub Copilot protocol headers, with
    /// caller overrides winning. `create_client` installs exactly this
    /// map as default headers on the underlying HTTP client.
    #[test]
    fn assemble_request_headers_collects_all_sources() {
        use crate::types::{CacheRetention, SessionAffinityFormat};
        let mut model = test_model(Provider::GitHubCopilot);
        model.headers = Some(HashMap::from([(
            "Editor-Version".to_string(),
            "hand/1.0".to_string(),
        )]));
        let mut compat = detect_compat(&model);
        compat.send_session_affinity_headers = true;
        compat.session_affinity_format = SessionAffinityFormat::OpenRouter;

        let context = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage::new_text("hi")),
                Message::Assistant(tool_call_assistant(&model, "bash")),
            ],
            tools: None,
        };
        let overrides = HashMap::from([("X-Custom".to_string(), "1".to_string())]);
        let headers = assemble_request_headers(
            &model,
            &context,
            &compat,
            Some(&overrides),
            Some("sess-1"),
            Some(CacheRetention::Short),
        );
        assert_eq!(
            headers.get("Editor-Version").map(String::as_str),
            Some("hand/1.0")
        );
        assert_eq!(
            headers.get("x-session-id").map(String::as_str),
            Some("sess-1")
        );
        // Last message is an assistant turn → agent-initiated call.
        assert_eq!(
            headers.get("X-Initiator").map(String::as_str),
            Some("agent")
        );
        assert_eq!(headers.get("X-Custom").map(String::as_str), Some("1"));
    }

    /// A provider-native effort keyword from the thinking level map must
    /// reach the wire even though the typed enum clamps the level to
    /// `high`.
    #[test]
    fn build_params_prefers_native_effort_over_clamped_enum() {
        let mut model = test_model(Provider::OpenAI);
        model.reasoning = true;
        model.thinking_level_map = Some(HashMap::from([(
            "max".to_string(),
            Some("max".to_string()),
        )]));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(openai_rust::types::ReasoningEffort::High),
            native_reasoning_effort: resolve_native_effort(&model, Some(ThinkingLevel::Max)),
            ..OpenAICompletionsOptions::default()
        };
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(
            body["reasoning_effort"], "max",
            "native keyword must beat the clamped enum: {body}"
        );
    }

    /// The OpenRouter nested reasoning object honours the native keyword
    /// the same way.
    #[test]
    fn build_params_openrouter_prefers_native_effort() {
        let mut model = test_model(Provider::Openrouter);
        model.reasoning = true;
        model.base_url = "https://openrouter.ai/api/v1".to_string();
        model.thinking_level_map = Some(HashMap::from([(
            "max".to_string(),
            Some("max".to_string()),
        )]));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: None,
        };
        let options = OpenAICompletionsOptions {
            reasoning_effort: Some(openai_rust::types::ReasoningEffort::High),
            native_reasoning_effort: resolve_native_effort(&model, Some(ThinkingLevel::Max)),
            ..OpenAICompletionsOptions::default()
        };
        let params = build_params(&model, &context, &options).expect("build ok");
        let body = serde_json::to_value(&params).expect("serialize");
        assert_eq!(body["reasoning"], serde_json::json!({ "effort": "max" }));
    }

    /// Map lookup semantics: value string when defined, None when the
    /// map or the key is absent, and None for the off sentinel.
    #[test]
    fn resolve_native_effort_reads_map_value() {
        let mut model = test_model(Provider::OpenAI);
        assert_eq!(
            resolve_native_effort(&model, Some(ThinkingLevel::Max)),
            None
        );
        model.thinking_level_map = Some(HashMap::from([(
            "max".to_string(),
            Some("max".to_string()),
        )]));
        assert_eq!(
            resolve_native_effort(&model, Some(ThinkingLevel::Max)),
            Some("max".to_string())
        );
        assert_eq!(
            resolve_native_effort(&model, Some(ThinkingLevel::High)),
            None
        );
        assert_eq!(resolve_native_effort(&model, None), None);
    }

    /// Composite `{call_id}|{item_id}` ids can repeat the call_id part
    /// across tool calls in the same turn. The item id must survive
    /// normalization so replayed Chat Completions payloads keep the
    /// ids distinct — the API rejects duplicate tool call ids.
    #[test]
    fn composite_tool_call_ids_stay_unique_per_item() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        let a = normalize_tool_call_id("call_1|item_a", &compat, &model);
        let b = normalize_tool_call_id("call_1|item_b", &compat, &model);
        assert_eq!(a, "call_1_item_a");
        assert_eq!(b, "call_1_item_b");
        assert_ne!(a, b);
    }

    /// A composite id with an empty item part keeps the sanitized
    /// call_id alone, matching the previous behaviour.
    #[test]
    fn composite_tool_call_id_without_item_part_keeps_call_id() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        assert_eq!(normalize_tool_call_id("call_1|", &compat, &model), "call_1");
    }

    /// Item ids from some providers run past 400 chars with base64
    /// characters. Over the 40-char limit the id collapses to a
    /// sanitized call_id prefix plus a deterministic hash of the full
    /// composite id: stable across calls, distinct across items.
    #[test]
    fn composite_tool_call_id_over_limit_hashes_deterministically() {
        let model = test_model(Provider::OpenAI);
        let compat = detect_compat(&model);
        let long_item_a = format!("call_abc|{}+/=", "x".repeat(400));
        let long_item_b = format!("call_abc|{}+/=", "y".repeat(400));

        let a1 = normalize_tool_call_id(&long_item_a, &compat, &model);
        let a2 = normalize_tool_call_id(&long_item_a, &compat, &model);
        let b = normalize_tool_call_id(&long_item_b, &compat, &model);

        assert_eq!(a1, a2, "same input must normalize identically");
        assert_ne!(a1, b, "different item ids must stay distinct");
        assert!(a1.len() <= 40, "must respect the 40-char limit: {a1}");
        assert!(a1.starts_with("call_abc_"), "keeps call_id prefix: {a1}");
        assert!(
            a1.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "only allowed chars survive: {a1}"
        );
    }
}
