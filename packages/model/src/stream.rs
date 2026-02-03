//! Streaming functions for AI providers.

use crate::api_registry::{AssistantMessageEventStream, Provider};
use crate::types::{
    AssistantMessage, Context, Model, ProviderStreamOptions, SimpleStreamOptions,
};
use futures::StreamExt;

/// Stream a response from the model.
///
/// # Arguments
///
/// * `model` - The model to use for the completion
/// * `context` - The conversation context
/// * `options` - Optional streaming configuration
///
/// # Returns
///
/// A stream of assistant message events
pub fn stream(
    model: &Model,
    context: Context,
    options: Option<ProviderStreamOptions>,
) -> AssistantMessageEventStream<'_> {
    match model.api {
        crate::types::Api::OpenAICompletions => {
            use crate::providers::openai_completions::{
                stream_openai_completions, OpenAICompletionsOptions,
            };
            let opts = OpenAICompletionsOptions {
                base: options.unwrap_or_default(),
                tool_choice: None,
                reasoning_effort: None,
            };
            stream_openai_completions(model.clone(), context, Some(opts))
        }
        _ => {
            // Return an error stream for unsupported APIs
            let error_msg = format!("No provider registered for api: {:?}", model.api);
            Box::pin(async_stream::stream! {
                yield crate::types::AssistantMessageEvent::Error {
                    reason: crate::types::StopReason::Error,
                    error: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![],
                        api: model.api.clone(),
                        provider: model.provider.clone(),
                        model: model.id.clone(),
                        usage: crate::types::Usage::default(),
                        stop_reason: crate::types::StopReason::Error,
                        error_message: Some(error_msg),
                        timestamp: current_timestamp_ms(),
                    },
                };
            })
        }
    }
}

/// Complete a request and return the full message.
///
/// This consumes the entire stream and returns the final message.
///
/// # Arguments
///
/// * `model` - The model to use for the completion
/// * `context` - The conversation context
/// * `options` - Optional streaming configuration
///
/// # Returns
///
/// The final assistant message
pub async fn complete(
    model: &Model,
    context: Context,
    options: Option<ProviderStreamOptions>,
) -> AssistantMessage {
    let mut s = stream(model, context, options);

    let mut final_message = None;

    while let Some(event) = s.next().await {
        match event {
            crate::types::AssistantMessageEvent::Done { message, .. } => {
                final_message = Some(message);
                break;
            }
            crate::types::AssistantMessageEvent::Error { error, .. } => {
                return error;
            }
            _ => {}
        }
    }

    final_message.unwrap_or_else(|| AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: crate::types::StopReason::Error,
        error_message: Some("Stream ended without result".to_string()),
        timestamp: current_timestamp_ms(),
    })
}

/// Stream a simple response from the model.
///
/// This is a convenience function that uses sensible defaults for most use cases.
///
/// # Arguments
///
/// * `model` - The model to use for the completion
/// * `context` - The conversation context
/// * `options` - Optional simplified streaming configuration
///
/// # Returns
///
/// A stream of assistant message events
pub fn stream_simple(
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream<'_> {
    match model.api {
        crate::types::Api::OpenAICompletions => {
            use crate::providers::openai_completions::OpenAICompletionsProvider;
            OpenAICompletionsProvider.stream_simple(model.clone(), context, options)
        }
        _ => {
            // Return an error stream for unsupported APIs
            let error_msg = format!("No provider registered for api: {:?}", model.api);
            Box::pin(async_stream::stream! {
                yield crate::types::AssistantMessageEvent::Error {
                    reason: crate::types::StopReason::Error,
                    error: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![],
                        api: model.api.clone(),
                        provider: model.provider.clone(),
                        model: model.id.clone(),
                        usage: crate::types::Usage::default(),
                        stop_reason: crate::types::StopReason::Error,
                        error_message: Some(error_msg),
                        timestamp: current_timestamp_ms(),
                    },
                };
            })
        }
    }
}

/// Complete a simple request and return the full message.
///
/// This consumes the entire stream and returns the final message.
///
/// # Arguments
///
/// * `model` - The model to use for the completion
/// * `context` - The conversation context
/// * `options` - Optional simplified streaming configuration
///
/// # Returns
///
/// The final assistant message
pub async fn complete_simple(
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessage {
    let mut s = stream_simple(model, context, options);

    let mut final_message = None;

    while let Some(event) = s.next().await {
        match event {
            crate::types::AssistantMessageEvent::Done { message, .. } => {
                final_message = Some(message);
                break;
            }
            crate::types::AssistantMessageEvent::Error { error, .. } => {
                return error;
            }
            _ => {}
        }
    }

    final_message.unwrap_or_else(|| AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: crate::types::StopReason::Error,
        error_message: Some("Stream ended without result".to_string()),
        timestamp: current_timestamp_ms(),
    })
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Extension trait for Model to provide convenient streaming methods.
pub trait ModelStreamExt {
    /// Stream a response using this model.
    fn stream(
        &self,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> AssistantMessageEventStream<'_>;

    /// Stream a simple response using this model.
    fn stream_simple(
        &self,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'_>;

    /// Complete a request using this model.
    fn complete(
        &self,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> impl std::future::Future<Output = AssistantMessage>;

    /// Complete a simple request using this model.
    fn complete_simple(
        &self,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> impl std::future::Future<Output = AssistantMessage>;
}

impl ModelStreamExt for Model {
    fn stream(
        &self,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> AssistantMessageEventStream<'_> {
        stream(self, context, options)
    }

    fn stream_simple(
        &self,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'_> {
        stream_simple(self, context, options)
    }

    async fn complete(
        &self,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> AssistantMessage {
        complete(self, context, options).await
    }

    async fn complete_simple(
        &self,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessage {
        complete_simple(self, context, options).await
    }
}
