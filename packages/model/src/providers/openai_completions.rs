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

        let opts = OpenAICompletionsOptions {
            base,
            tool_choice: None,
            reasoning_effort,
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

        match run_stream(&model, &context, &options, &mut output).await {
            Ok(()) => {
                yield AssistantMessageEvent::Done {
                    reason: output.stop_reason,
                    message: output
                };
            }
            Err(e) => {
                output.stop_reason = StopReason::Error;
                output.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output.clone()
                };
            }
        }
    })
}

// =============================================================================
// Stream Processing
// =============================================================================

async fn run_stream(
    model: &Model,
    context: &Context,
    options: &OpenAICompletionsOptions,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    let api_key = options
        .api_key()
        .map(|s| s.to_string())
        .or_else(|| env_api_keys::get_env_api_key(&model.provider))
        .unwrap_or_default();

    let client =
        create_client(model, context, &api_key, options.headers()).map_err(|e| e.to_string())?;

    let params = build_params(model, context, options).map_err(|e| e.to_string())?;

    let completions = client.completions();
    let stream_result = completions.create_stream(&params).await;
    let mut stream = Box::pin(stream_result.map_err(|e| e.to_string())?);

    let mut current_block: Option<CurrentBlock> = None;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(finish_reason) = &choice.finish_reason {
                        output.stop_reason = map_stop_reason(finish_reason);
                    }

                    handle_delta(&choice.delta, &mut current_block, output).await?
                }
            }
            Err(e) => {
                return Err(e.to_string());
            }
        }
    }

    finish_current_block(&mut current_block, output).await;
    Ok(())
}

async fn handle_delta(
    delta: &openai_rust::types::Delta,
    current_block: &mut Option<CurrentBlock>,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    // Handle content (text)
    if let Some(content) = delta.content.as_ref() {
        match content {
            Content::Text(text) if !text.is_empty() => {
                if !matches!(current_block, Some(CurrentBlock::Text(_))) {
                    finish_current_block(current_block, output).await;
                    *current_block = Some(CurrentBlock::Text(TextContent::new(text.clone())));
                    output
                        .content
                        .push(AssistantContentBlock::Text(TextContent::new(text.clone())));
                }

                if let Some(CurrentBlock::Text(text_block)) = current_block {
                    text_block.text.push_str(text);
                }
            }
            Content::Array(parts) => {
                for part in parts {
                    if let ContentPart::Text { text } = part
                        && !text.is_empty()
                    {
                        if !matches!(current_block, Some(CurrentBlock::Text(_))) {
                            finish_current_block(current_block, output).await;
                            *current_block =
                                Some(CurrentBlock::Text(TextContent::new(text.clone())));
                            output
                                .content
                                .push(AssistantContentBlock::Text(TextContent::new(text.clone())));
                        }
                        if let Some(CurrentBlock::Text(text_block)) = current_block {
                            text_block.text.push_str(text);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Handle reasoning/thinking
    if let Some(reasoning) = &delta.reasoning
        && !reasoning.is_empty()
    {
        if !matches!(current_block, Some(CurrentBlock::Thinking(_))) {
            finish_current_block(current_block, output).await;
            let thinking = ThinkingContent::new(reasoning);
            *current_block = Some(CurrentBlock::Thinking(thinking.clone()));
            output
                .content
                .push(AssistantContentBlock::Thinking(thinking));
        }

        if let Some(CurrentBlock::Thinking(thinking_block)) = current_block {
            thinking_block.thinking.push_str(reasoning);
        }
    }

    // Handle tool calls
    if let Some(tool_calls) = &delta.tool_calls {
        for tool_call in tool_calls {
            let tool_id = tool_call.id.clone().unwrap_or_default();

            let is_new_tool = match current_block {
                Some(CurrentBlock::ToolCall(tc, _)) => tc.id != tool_id,
                _ => true,
            };

            if is_new_tool {
                finish_current_block(current_block, output).await;
                let tc = ToolCall::new(&tool_id, "", serde_json::json!({}));
                *current_block = Some(CurrentBlock::ToolCall(tc.clone(), String::new()));
                output.content.push(AssistantContentBlock::ToolCall(tc));
            }

            if let Some(CurrentBlock::ToolCall(tc, partial_args)) = current_block
                && let Some(function) = &tool_call.function
            {
                if let Some(name) = &function.name {
                    tc.name = name.clone();
                }
                if let Some(args) = &function.arguments {
                    partial_args.push_str(args);
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
enum CurrentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall, String),
}

async fn finish_current_block(
    current_block: &mut Option<CurrentBlock>,
    output: &mut AssistantMessage,
) {
    if let Some(block) = current_block.take() {
        match block {
            CurrentBlock::Text(_) => {}
            CurrentBlock::Thinking(_) => {}
            CurrentBlock::ToolCall(mut tc, partial_args) => {
                tc.arguments = parse_streaming_json(&partial_args);
                if let Some(AssistantContentBlock::ToolCall(last_tc)) = output.content.last_mut() {
                    last_tc.arguments = tc.arguments;
                }
            }
        }
    }
}

// =============================================================================
// Client & Request Building
// =============================================================================

fn create_client(
    model: &Model,
    context: &Context,
    api_key: &str,
    options_headers: Option<&HashMap<String, String>>,
) -> Result<Client, String> {
    let mut headers = model.headers.clone().unwrap_or_default();

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

    Client::builder()
        .api_key(api_key.to_string())
        .base_url(model.base_url.clone())
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

    if let Some(tools) = &context.tools {
        let openai_tools: Vec<OpenAiTool> = tools.iter().map(convert_tool).collect();
        builder = builder.tools(openai_tools);
    } else if has_tool_history(&context.messages) {
        builder = builder.tools(vec![]);
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
    } else if options.reasoning_effort.is_some()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        builder = builder.reasoning_effort(options.reasoning_effort.unwrap());
    }

    if model.base_url.contains("openrouter.ai")
        && let Some(crate::types::Compat::OpenAICompletions(compat_settings)) = &model.compat
        && let Some(router_routing) = &compat_settings.open_router_routing
        && (router_routing.only.is_some() || router_routing.order.is_some())
    {
        let mut extra = HashMap::new();
        extra.insert(
            "provider".to_string(),
            serde_json::json!({
                "only": router_routing.only,
                "order": router_routing.order
            }),
        );
        builder = builder.extra_params(extra);
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

    for (i, msg) in context.messages.iter().enumerate() {
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

                let assistant_content = if !text_blocks.is_empty() {
                    if model.provider == Provider::GitHubCopilot {
                        let text: String = text_blocks
                            .iter()
                            .map(|t| sanitize_surrogates(&t.text))
                            .collect();
                        Content::Text(text)
                    } else {
                        let parts: Vec<ContentPart> = text_blocks
                            .iter()
                            .map(|t| ContentPart::Text {
                                text: sanitize_surrogates(&t.text),
                            })
                            .collect();
                        Content::Array(parts)
                    }
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
                            .map(|t| t.thinking.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");

                        match assistant_content {
                            Content::Array(mut parts) => {
                                parts.insert(
                                    0,
                                    ContentPart::Text {
                                        text: thinking_text,
                                    },
                                );
                                Content::Array(parts)
                            }
                            Content::Text(existing) => {
                                Content::Text(format!("{thinking_text}\n\n{existing}"))
                            }
                        }
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

                        let content = if text_result.is_empty() && has_images {
                            "(see attached image)".to_string()
                        } else {
                            sanitize_surrogates(&text_result)
                        };

                        let mut tool_msg =
                            RequestMessage::tool_response(content, tr.tool_call_id.clone());

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

                continue;
            }
        }

        last_role = Some(match msg {
            Message::User(_) => "user".to_string(),
            Message::Assistant(_) => "assistant".to_string(),
            Message::ToolResult(_) => "toolResult".to_string(),
        });
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

fn normalize_tool_call_id(id: &str, compat: &ResolvedCompat, model: &Model) -> String {
    if compat.requires_mistral_tool_ids {
        return normalize_mistral_tool_id(id);
    }

    if id.contains('|') {
        let call_id = id.split('|').next().unwrap_or(id);
        return call_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
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
    serde_json::from_str(input).unwrap_or_else(|_| {
        if input.trim().starts_with('{') {
            serde_json::json!({})
        } else {
            Value::String(input.to_string())
        }
    })
}

fn map_thinking_level(level: ThinkingLevel) -> openai_rust::types::ReasoningEffort {
    match level {
        ThinkingLevel::Minimal => openai_rust::types::ReasoningEffort::Minimal,
        ThinkingLevel::Low => openai_rust::types::ReasoningEffort::Low,
        ThinkingLevel::Medium => openai_rust::types::ReasoningEffort::Medium,
        ThinkingLevel::High => openai_rust::types::ReasoningEffort::High,
        ThinkingLevel::Xhigh => openai_rust::types::ReasoningEffort::High,
    }
}

fn clamp_reasoning(reasoning: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    reasoning.map(|r| match r {
        ThinkingLevel::Xhigh => ThinkingLevel::High,
        _ => r,
    })
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
/// Fields are populated by [`detect_compat`] from `model.provider`/`model.base_url`,
/// then overridden by any explicit settings on `model.compat`. New fields should
/// be added with sane defaults so older callers keep compiling.
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

    let is_non_standard = *provider == Provider::Cerebras
        || base_url.contains("cerebras.ai")
        || *provider == Provider::Xai
        || base_url.contains("api.x.ai")
        || *provider == Provider::Mistral
        || base_url.contains("mistral.ai")
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || *provider == Provider::Opencode
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai;

    let use_max_tokens = *provider == Provider::Mistral
        || base_url.contains("mistral.ai")
        || base_url.contains("chutes.ai");

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
        supports_reasoning_effort: !is_grok && !is_zai,
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
        supports_strict_mode: !is_cloudflare_workers_ai,
        zai_tool_stream: is_zai,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Api, AssistantContentBlock, AssistantMessage, Cost, InputType, TextContent, ToolCall,
        Usage, UserMessage,
    };

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
}
