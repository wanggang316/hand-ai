//! AI client for streaming completions.

use crate::api_registry::{ApiProvider, ApiProviderRegistry, AssistantMessageEventStream};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, ProviderStreamOptions,
    SimpleStreamOptions, StopReason,
};
use futures::StreamExt;
use std::sync::Arc;

/// AI client that manages provider registry and streaming operations.
#[derive(Clone)]
pub struct Client {
    registry: Arc<ApiProviderRegistry>,
}

impl Client {
    /// Create a new client with an empty registry.
    pub fn new() -> Self {
        Self {
            registry: Arc::new(ApiProviderRegistry::new()),
        }
    }

    /// Create a new client with built-in providers registered.
    pub fn with_builtin_providers() -> Self {
        let client = Self::new();
        client.register_builtin_providers();
        client
    }

    /// Register all built-in API providers.
    pub fn register_builtin_providers(&self) {
        use crate::providers::openai_completions::OpenAICompletionsProvider;

        self.registry.register(
            crate::types::Api::OpenAICompletions,
            Box::new(OpenAICompletionsProvider::new()),
            Some("builtin".to_string()),
        );
    }

    /// Register a custom API provider.
    pub fn register_provider(
        &self,
        api: crate::types::Api,
        provider: Box<dyn ApiProvider + Send + Sync>,
        source_id: Option<String>,
    ) {
        self.registry.register(api, provider, source_id);
    }

    /// Stream a response from the model.
    pub fn stream(
        &self,
        model: &Model,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        if let Some(provider) = self.registry.get(&model.api) {
            provider.stream(model.clone(), context, options)
        } else {
            make_error_stream(
                format!("No provider registered for api: {:?}", model.api),
                model.clone(),
            )
        }
    }

    /// Stream a simple response from the model.
    pub fn stream_simple(
        &self,
        model: &Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        if let Some(provider) = self.registry.get(&model.api) {
            provider.stream_simple(model.clone(), context, options)
        } else {
            make_error_stream(
                format!("No provider registered for api: {:?}", model.api),
                model.clone(),
            )
        }
    }

    /// Complete a request and return the full message.
    pub async fn complete(
        &self,
        model: &Model,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> AssistantMessage {
        let mut s = self.stream(model, context, options);

        let mut final_message = None;

        while let Some(event) = s.next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => {
                    final_message = Some(message);
                    break;
                }
                AssistantMessageEvent::Error { error, .. } => {
                    return error;
                }
                _ => {}
            }
        }

        final_message.unwrap_or_else(|| make_error_message(model, "Stream ended without result"))
    }

    /// Complete a simple request and return the full message.
    pub async fn complete_simple(
        &self,
        model: &Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessage {
        let mut s = self.stream_simple(model, context, options);

        let mut final_message = None;

        while let Some(event) = s.next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => {
                    final_message = Some(message);
                    break;
                }
                AssistantMessageEvent::Error { error, .. } => {
                    return error;
                }
                _ => {}
            }
        }

        final_message.unwrap_or_else(|| make_error_message(model, "Stream ended without result"))
    }

    /// Get the underlying registry.
    pub fn registry(&self) -> &ApiProviderRegistry {
        &self.registry
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Helper functions
// =============================================================================

fn make_error_stream(
    error_msg: String,
    model: Model,
) -> AssistantMessageEventStream<'static> {
    Box::pin(async_stream::stream! {
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: make_error_message(&model, &error_msg),
        };
    })
}

fn make_error_message(model: &Model, error_msg: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some(error_msg.to_string()),
        timestamp: current_timestamp_ms(),
    }
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
