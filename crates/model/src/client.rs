//! AI client for streaming completions.

use crate::api_registry::{ApiProviderRegistry, AssistantMessageEventStream, BoxedApiProvider};
use crate::types::{
    Api, AssistantMessage, AssistantMessageEvent, Context, Model, ProviderStreamOptions,
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
    /// An OAuth-backed provider has no credentials and the caller did
    /// not supply an explicit `api_key`. Surfaced primarily through the
    /// stream itself as an `Error` event; this variant is here for
    /// callers that synthesize the error eagerly.
    OAuthRequired {
        provider: crate::oauth::OAuthProviderId,
    },
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
            ClientError::OAuthRequired { provider } => {
                write!(
                    f,
                    "OAuth credentials required for provider {provider:?} (run `oauth login`)",
                )
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
    ///
    /// Shortcut for [`Client::builder`]`().with_all_builtins().build()`.
    /// Embedders that want only a subset of providers — or that need
    /// to plug in a custom one — should use [`ClientBuilder`] directly
    /// so they don't pay the binary-size cost of provider code they
    /// can't reach.
    pub fn new() -> Self {
        Self::builder().with_all_builtins().build()
    }

    /// Start a [`ClientBuilder`] for fine-grained control over which
    /// providers are registered. See the [`ClientBuilder`] docs for
    /// the full surface.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
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
    /// Delegates to [`crate::stream::stream_simple`] so callers automatically
    /// pick up cancellation, timeout, and retry semantics. The returned
    /// `EventStream` is converted back to the trait-level
    /// `AssistantMessageEventStream` boxed alias for backwards compatibility.
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
        let event_stream = crate::stream::stream_simple(&self.registry, model, context, options)?;
        Ok(Box::pin(event_stream))
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
    /// Delegates to [`crate::stream::complete_simple`].
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
        crate::stream::complete_simple(&self.registry, model, context, options).await
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for [`Client`] that lets embedders register an
/// arbitrary subset of built-in providers and plug in custom ones
/// implemented outside this crate.
///
/// The motivation is binary size: each built-in provider links its
/// own transport (AWS SigV4 for Bedrock, OAuth flows for Vertex, …),
/// so a UI that only exposes openai / anthropic / google should not
/// have to pay the code-gen cost of provider paths it never invokes.
///
/// # Example
///
/// ```no_run
/// use model::{Client, Api};
///
/// // Only the providers we ship UI for.
/// let client = Client::builder()
///     .with_builtin(Api::AnthropicMessages)
///     .with_builtin(Api::OpenAICompletions)
///     .with_builtin(Api::GoogleGenerativeAi)
///     .build();
/// ```
///
/// `build()` cannot fail; an empty registry is legal and any
/// `stream` / `complete` call against a model whose `api` is not
/// registered returns [`ClientError::ProviderNotFound`] at runtime.
pub struct ClientBuilder {
    registry: ApiProviderRegistry,
}

impl ClientBuilder {
    /// Start with an empty provider registry. The caller is expected
    /// to add at least one provider via [`Self::with_builtin`],
    /// [`Self::with_all_builtins`], or [`Self::with_provider`] before
    /// streaming.
    pub fn new() -> Self {
        Self {
            registry: ApiProviderRegistry::new(),
        }
    }

    /// Register the built-in provider for `api`. No-op (returns
    /// `self` unchanged) when `api` has no built-in counterpart
    /// (currently `Api::Faux`, which is only available via the
    /// `faux` cargo feature). Calling twice for the same `api`
    /// overwrites the earlier registration, matching the registry's
    /// last-wins semantics.
    #[must_use]
    pub fn with_builtin(self, api: Api) -> Self {
        if let Some(provider) = crate::providers::register_builtins::make_builtin_provider(api) {
            self.registry.register(
                api,
                provider,
                Some(crate::providers::register_builtins::BUILTIN_SOURCE_ID.to_string()),
            );
        }
        self
    }

    /// Register every built-in provider. Identical to
    /// [`Client::new`]'s default behaviour.
    #[must_use]
    pub fn with_all_builtins(self) -> Self {
        crate::providers::register_builtins(&self.registry);
        self
    }

    /// Plug in a custom provider — typically one implemented outside
    /// the `model` crate — against `api`. The `provider` parameter is
    /// the standard [`BoxedApiProvider`] used everywhere else in the
    /// registry so existing custom providers work unchanged.
    #[must_use]
    pub fn with_provider(self, api: Api, provider: BoxedApiProvider) -> Self {
        // Source id `None` so introspection can tell a custom plug-in
        // apart from a built-in even when they share an `Api`.
        self.registry.register(api, provider, None);
        self
    }

    /// Finalize into a [`Client`].
    pub fn build(self) -> Client {
        Client {
            registry: Arc::new(self.registry),
        }
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Cost, InputType, Provider};

    fn test_model(api: Api) -> Model {
        Model {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            api,
            provider: Provider::Anthropic,
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
    fn client_new_registers_all_builtin_providers() {
        let client = Client::new();
        // All 9 API types should have providers
        let apis = [
            Api::AnthropicMessages,
            Api::OpenAICompletions,
            Api::OpenAIResponses,
            Api::AzureOpenAiResponses,
            Api::OpenAICodexResponses,
            Api::GoogleGenerativeAi,
            Api::GoogleVertex,
            Api::GoogleGeminiCli,
            Api::BedrockConverseStream,
        ];
        for api in apis {
            assert!(
                client.registry.get(&api).is_some(),
                "Provider missing for {api:?}"
            );
        }
    }

    #[test]
    fn client_default_same_as_new() {
        let c1 = Client::new();
        let c2 = Client::default();
        // Both should have all providers registered
        assert!(c1.registry.get(&Api::AnthropicMessages).is_some());
        assert!(c2.registry.get(&Api::AnthropicMessages).is_some());
    }

    #[test]
    fn stream_returns_error_for_cleared_registry() {
        let client = Client::new();
        client.registry.clear();
        let model = test_model(Api::AnthropicMessages);
        let ctx = Context::default();
        let result = client.stream(&model, ctx, None);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(matches!(err, ClientError::ProviderNotFound { .. }));
        assert!(err.to_string().contains("AnthropicMessages"));
    }

    #[test]
    fn stream_simple_returns_error_for_cleared_registry() {
        let client = Client::new();
        client.registry.clear();
        let model = test_model(Api::OpenAICompletions);
        let ctx = Context::default();
        let result = client.stream_simple(&model, ctx, None);
        assert!(result.is_err());
    }

    #[test]
    fn client_error_display() {
        let err = ClientError::ProviderNotFound {
            api: Api::AnthropicMessages,
            model_id: "claude-3".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("AnthropicMessages"));
        assert!(msg.contains("claude-3"));

        let err2 = ClientError::StreamEndedWithoutResult;
        assert!(err2.to_string().contains("Stream ended"));
    }

    #[test]
    fn client_is_clone() {
        let client = Client::new();
        let cloned = client.clone();
        // Both share the same Arc registry
        assert!(cloned.registry.get(&Api::AnthropicMessages).is_some());
    }

    // ----- ClientBuilder (#33) -----

    #[test]
    fn builder_empty_registers_no_providers() {
        let client = Client::builder().build();
        for api in [
            Api::AnthropicMessages,
            Api::OpenAICompletions,
            Api::GoogleGenerativeAi,
        ] {
            assert!(
                client.registry.get(&api).is_none(),
                "empty builder must not register {api:?}"
            );
        }
    }

    #[test]
    fn builder_with_builtin_registers_just_that_api() {
        let client = Client::builder()
            .with_builtin(Api::AnthropicMessages)
            .build();
        assert!(client.registry.get(&Api::AnthropicMessages).is_some());
        // Untouched APIs stay absent — the whole point of the subset
        // API is that callers don't pay for code they don't reach.
        assert!(client.registry.get(&Api::OpenAICompletions).is_none());
        assert!(client.registry.get(&Api::BedrockConverseStream).is_none());
    }

    #[test]
    fn builder_with_all_builtins_matches_client_new() {
        let via_builder = Client::builder().with_all_builtins().build();
        let via_new = Client::new();
        for api in [
            Api::AnthropicMessages,
            Api::OpenAICompletions,
            Api::OpenAIResponses,
            Api::AzureOpenAiResponses,
            Api::OpenAICodexResponses,
            Api::GoogleGenerativeAi,
            Api::GoogleVertex,
            Api::GoogleGeminiCli,
            Api::BedrockConverseStream,
        ] {
            assert!(via_builder.registry.get(&api).is_some(), "missing {api:?}");
            assert!(via_new.registry.get(&api).is_some(), "missing {api:?}");
        }
    }

    /// Stream against a builder-built client that omits the model's
    /// API must surface the same `ProviderNotFound` error users would
    /// see if they hand-cleared the full registry — confirms the
    /// runtime contract for an "intentionally subset" build.
    #[test]
    fn builder_subset_returns_provider_not_found_for_unregistered_api() {
        let client = Client::builder()
            .with_builtin(Api::AnthropicMessages)
            .build();
        let model = test_model(Api::OpenAICompletions);
        let result = client.stream(&model, Context::default(), None);
        match result {
            Err(ClientError::ProviderNotFound { api, .. }) => {
                assert_eq!(api, Api::OpenAICompletions);
            }
            Err(e) => panic!("expected ProviderNotFound, got {e:?}"),
            Ok(_) => panic!("OpenAI is not registered; stream() must error"),
        }
    }
}
