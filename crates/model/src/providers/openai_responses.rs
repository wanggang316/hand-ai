//! OpenAI Responses API provider.
//!
//! Implements the OpenAI Responses API format used by o1, o3, and newer models.
//! This API uses `/v1/responses` endpoint with a different event/streaming format
//! than the `/v1/chat/completions` endpoint.
//!
//! Key differences from OpenAI Completions:
//! - Endpoint: POST /v1/responses (not /v1/chat/completions)
//! - Request format: "input" array instead of "messages"
//! - Function calls use "function_call" output items (not tool_calls in delta)
//! - Streaming events: response.created, response.output_item.added,
//!   response.content_part.added, response.output_text.delta,
//!   response.function_call_arguments.delta, response.completed
//! - Built-in reasoning/thinking support with "reasoning" parameter

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::env_api_keys;
use crate::providers::openai_responses_shared::{
    build_request_body, current_timestamp_ms, drive_sse_stream,
};
use crate::types::{
    Api, AssistantMessage, AssistantMessageEvent, Context, Model, Provider, SimpleStreamOptions,
    StopReason, StreamOptions, Usage,
};

/// Provider for OpenAI Responses API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAIResponsesProvider;

impl OpenAIResponsesProvider {
    pub fn new() -> Self {
        Self
    }
}

impl ApiProvider for OpenAIResponsesProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        stream_openai_responses(model, context, options)
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let api_key = options
            .as_ref()
            .and_then(|o| o.api_key().map(|s| s.to_string()))
            .or_else(|| env_api_keys::get_env_api_key(&model.provider));

        if api_key.is_none() {
            let error_msg = format!("No API key for provider: {:?}", model.provider);
            return make_error_stream(error_msg, model.id.clone(), model.provider, model.api);
        }

        let mut base = StreamOptions::default();
        if let Some(opts) = &options {
            base.temperature = opts.temperature();
            base.max_tokens = opts.max_tokens();
            base.api_key = api_key;
            base.headers = opts.headers().cloned();
        }

        stream_openai_responses(model, context, Some(base))
    }
}

fn make_error_stream(
    error_msg: String,
    model_id: String,
    provider: Provider,
    api: Api,
) -> AssistantMessageEventStream<'static> {
    Box::pin(async_stream::stream! {
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: AssistantMessage {
                role: "assistant".to_string(),
                api,
                provider,
                model: model_id,
                usage: Usage::default(),
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

/// Stream from the OpenAI Responses API using SSE.
fn stream_openai_responses(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream<'static> {
    let options = options.unwrap_or_default();

    Box::pin(async_stream::stream! {
        let mut output = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: model.api,
            provider: model.provider,
            model: model.id.clone(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };

        yield AssistantMessageEvent::Start {
            partial: output.clone(),
        };

        // Build request
        let body = build_request_body(&model, &context, &options);
        let base_url = if model.base_url.is_empty() {
            "https://api.openai.com".to_string()
        } else {
            model.base_url.clone()
        };
        let url = format!("{}/v1/responses", base_url);

        let api_key = options.api_key
            .or_else(|| env_api_keys::get_env_api_key(&model.provider))
            .unwrap_or_default();

        // Make HTTP request
        let client = reqwest::Client::new();
        let mut builder = client.post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .body(serde_json::to_string(&body).unwrap_or_default());

        // Add custom headers
        if let Some(ref headers) = options.headers {
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
        }
        if let Some(ref model_headers) = model.headers {
            for (k, v) in model_headers {
                builder = builder.header(k, v);
            }
        }

        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                output.error_message = Some(format!("Request failed: {}", e));
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            output.error_message = Some(format!("HTTP {}: {}", status, body_text));
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        }

        // Stream SSE events from the shared decoder. The decoder yields a
        // terminal `Error` event itself if it hits a transport failure
        // mid-stream and stamps `output.stop_reason = Error` so we know to
        // skip the `Done` event below.
        {
            use futures::StreamExt;
            let mut inner = Box::pin(drive_sse_stream(response, &mut output));
            while let Some(ev) = inner.next().await {
                yield ev;
            }
        }

        if matches!(output.stop_reason, StopReason::Error) {
            return;
        }

        yield AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output,
        };
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openai_responses_shared::{build_request_body, convert_to_input};
    use crate::types::{Cost, InputType, Message, TextContent, UserMessage};

    fn test_model() -> Model {
        Model {
            id: "o3-mini".to_string(),
            name: "o3-mini".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: String::new(),
            reasoning: true,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 100_000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn test_context() -> Context {
        Context {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            messages: vec![Message::User(UserMessage::new_text("hello"))],
            tools: None,
        }
    }

    #[test]
    fn test_build_request_body() {
        let model = test_model();
        let context = test_context();
        let options = StreamOptions::default();

        let body = build_request_body(&model, &context, &options);
        assert_eq!(body["model"], "o3-mini");
        assert_eq!(body["stream"], true);
        assert!(body["instructions"].is_string());
        assert!(body["input"].is_array());
    }

    #[test]
    fn test_convert_to_input_user() {
        let context = test_context();
        let input = convert_to_input(&context);
        let arr = input.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hello");
    }

    #[test]
    fn test_convert_to_input_tool_result() {
        let context = Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(crate::types::ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "call_123".to_string(),
                tool_name: "read".to_string(),
                content: vec![crate::types::ToolResultContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: "file contents".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 0,
            })],
            tools: None,
        };
        let input = convert_to_input(&context);
        let arr = input.as_array().unwrap();
        assert_eq!(arr[0]["type"], "function_call_output");
        assert_eq!(arr[0]["call_id"], "call_123");
    }

    #[test]
    fn test_provider_creation() {
        let _provider = OpenAIResponsesProvider::new();
    }

    #[test]
    fn test_build_request_with_tools() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![crate::types::Tool {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }]),
        };
        let options = StreamOptions::default();
        let body = build_request_body(&model, &context, &options);
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }
}
