//! AI client for streaming completions.

use crate::api_registry::{ApiProviderRegistry, AssistantMessageEventStream};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, ProviderStreamOptions,
    SimpleStreamOptions,
};
use futures::StreamExt;
use std::fmt;
use std::sync::Arc;

/// Errors that can occur when using the client.
#[derive(Debug, Clone)]
pub enum ClientError {
    /// No provider is registered for the requested API.
    ProviderNotFound {
        api: crate::types::Api,
        model_id: String,
    },
    /// The stream ended without producing a result.
    StreamEndedWithoutResult,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::ProviderNotFound { api, model_id } => {
                write!(
                    f,
                    "No provider registered for api: {api:?} (model: {model_id})"
                )
            }
            ClientError::StreamEndedWithoutResult => {
                write!(f, "Stream ended without producing a result")
            }
        }
    }
}

impl std::error::Error for ClientError {}

/// AI client that manages provider registry and streaming operations.
#[derive(Clone)]
pub struct Client {
    pub registry: Arc<ApiProviderRegistry>,
}

impl Client {
    /// Create a new client with all built-in providers registered.
    pub fn new() -> Self {
        let client = Self {
            registry: Arc::new(ApiProviderRegistry::new()),
        };
        client.register_builtin_providers();
        client
    }

    fn register_builtin_providers(&self) {
        use crate::providers::anthropic_messages::AnthropicMessagesProvider;
        use crate::providers::google_generative_ai::GoogleGenerativeAiProvider;
        use crate::providers::openai_completions::OpenAICompletionsProvider;

        self.registry.register(
            crate::types::Api::AnthropicMessages,
            Box::new(AnthropicMessagesProvider::new()),
            Some("builtin".to_string()),
        );

        self.registry.register(
            crate::types::Api::OpenAICompletions,
            Box::new(OpenAICompletionsProvider::new()),
            Some("builtin".to_string()),
        );

        self.registry.register(
            crate::types::Api::GoogleGenerativeAi,
            Box::new(GoogleGenerativeAiProvider::new()),
            Some("builtin".to_string()),
        );
    }

    /// Stream a response from the model.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::ProviderNotFound` if no provider is registered for the model's API.
    pub fn stream(
        &self,
        model: &Model,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> Result<AssistantMessageEventStream<'static>, ClientError> {
        match self.registry.get(&model.api) {
            Some(provider) => Ok(provider.stream(model.clone(), context, options)),
            None => Err(ClientError::ProviderNotFound {
                api: model.api,
                model_id: model.id.clone(),
            }),
        }
    }

    /// Stream a simple response from the model.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::ProviderNotFound` if no provider is registered for the model's API.
    pub fn stream_simple(
        &self,
        model: &Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream<'static>, ClientError> {
        match self.registry.get(&model.api) {
            Some(provider) => Ok(provider.stream_simple(model.clone(), context, options)),
            None => Err(ClientError::ProviderNotFound {
                api: model.api,
                model_id: model.id.clone(),
            }),
        }
    }

    /// Complete a request and return the full message.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::ProviderNotFound` if no provider is registered.
    /// Returns the API error wrapped in `AssistantMessage` if the stream produces an error event.
    pub async fn complete(
        &self,
        model: &Model,
        context: Context,
        options: Option<ProviderStreamOptions>,
    ) -> Result<AssistantMessage, ClientError> {
        let mut s = self.stream(model, context, options)?;

        while let Some(event) = s.next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => return Ok(message),
                AssistantMessageEvent::Error { error, .. } => return Ok(error),
                _ => {}
            }
        }

        Err(ClientError::StreamEndedWithoutResult)
    }

    /// Complete a simple request and return the full message.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::ProviderNotFound` if no provider is registered.
    /// Returns the API error wrapped in `AssistantMessage` if the stream produces an error event.
    pub async fn complete_simple(
        &self,
        model: &Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessage, ClientError> {
        let mut s = self.stream_simple(model, context, options)?;

        while let Some(event) = s.next().await {
            match event {
                AssistantMessageEvent::Done { message, .. } => return Ok(message),
                AssistantMessageEvent::Error { error, .. } => return Ok(error),
                _ => {}
            }
        }

        Err(ClientError::StreamEndedWithoutResult)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
