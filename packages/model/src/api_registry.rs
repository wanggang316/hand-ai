//! API provider registry.
//!
//! Defines the Provider trait and allows registering API providers
//! that can handle streaming requests for specific API types.

use crate::types::{
    Api, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions,
};
use futures::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

/// Type alias for the event stream returned by providers.
pub type AssistantMessageEventStream<'a> =
    Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send + 'a>>;

/// Core trait for AI providers that support streaming chat completions.
///
/// Implement this trait for each API provider (OpenAI, Anthropic, Google, etc.)
/// to enable unified access through the provider registry.
pub trait Provider: Send + Sync {
    /// Stream chat completions with full options.
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'_>;

    /// Stream chat completions with simplified options.
    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'_>;
}

/// A boxed provider trait object that can be shared across threads.
pub type BoxedProvider = Box<dyn Provider + Send + Sync>;

/// A registered API provider that can be used to stream chat completions.
#[derive(Clone)]
pub struct ApiProvider {
    pub api: Api,
    provider: Arc<BoxedProvider>,
}

impl ApiProvider {
    /// Create a new API provider wrapper.
    pub fn new(api: Api, provider: BoxedProvider) -> Self {
        Self {
            api,
            provider: Arc::new(provider),
        }
    }

    /// Get the underlying provider.
    pub fn provider(&self) -> Arc<BoxedProvider> {
        self.provider.clone()
    }
}

struct RegisteredApiProvider {
    provider: ApiProvider,
    source_id: Option<String>,
}

/// Registry of API providers.
pub struct ApiProviderRegistry {
    providers: RwLock<HashMap<Api, RegisteredApiProvider>>,
}

impl Default for ApiProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProviderRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    /// Register an API provider.
    pub fn register(&self, api: Api, provider: BoxedProvider, source_id: Option<String>) {
        let mut providers = self.providers.write().unwrap();
        providers.insert(
            api,
            RegisteredApiProvider {
                provider: ApiProvider::new(api, provider),
                source_id,
            },
        );
    }

    /// Get an API provider by API type.
    pub fn get(&self, api: &Api) -> Option<ApiProvider> {
        let providers = self.providers.read().unwrap();
        providers.get(api).map(|r| r.provider.clone())
    }

    /// Get all registered API providers.
    pub fn get_all(&self) -> Vec<ApiProvider> {
        let providers = self.providers.read().unwrap();
        providers.values().map(|r| r.provider.clone()).collect()
    }

    /// Unregister all API providers from a specific source.
    pub fn unregister_by_source(&self, source_id: &str) {
        let mut providers = self.providers.write().unwrap();
        providers.retain(|_, v| v.source_id.as_deref() != Some(source_id));
    }

    /// Clear all registered API providers.
    pub fn clear(&self) {
        let mut providers = self.providers.write().unwrap();
        providers.clear();
    }

    /// Check if a provider is registered for an API type.
    pub fn has(&self, api: &Api) -> bool {
        let providers = self.providers.read().unwrap();
        providers.contains_key(api)
    }
}

/// Global API provider registry.
static GLOBAL_REGISTRY: std::sync::OnceLock<ApiProviderRegistry> = std::sync::OnceLock::new();

/// Get the global API provider registry.
pub fn global_registry() -> &'static ApiProviderRegistry {
    GLOBAL_REGISTRY.get_or_init(ApiProviderRegistry::new)
}

/// Register an API provider in the global registry.
pub fn register_api_provider(api: Api, provider: BoxedProvider, source_id: Option<String>) {
    global_registry().register(api, provider, source_id);
}

/// Get an API provider from the global registry.
pub fn get_api_provider(api: &Api) -> Option<ApiProvider> {
    global_registry().get(api)
}

/// Get all API providers from the global registry.
pub fn get_api_providers() -> Vec<ApiProvider> {
    global_registry().get_all()
}

/// Unregister API providers by source ID from the global registry.
pub fn unregister_api_providers(source_id: &str) {
    global_registry().unregister_by_source(source_id);
}

/// Clear all API providers from the global registry.
pub fn clear_api_providers() {
    global_registry().clear();
}

/// Check if a provider is registered for an API type in the global registry.
pub fn has_api_provider(api: &Api) -> bool {
    global_registry().has(api)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider;

    impl Provider for MockProvider {
        fn stream(
            &self,
            _model: Model,
            _context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'_> {
            Box::pin(async_stream::stream! {
                yield AssistantMessageEvent::Done {
                    reason: crate::types::StopReason::Stop,
                    message: crate::types::AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![],
                        api: crate::types::Api::OpenAICompletions,
                        provider: crate::types::Provider::OpenAI,
                        model: "test".to_string(),
                        usage: crate::types::Usage::default(),
                        stop_reason: crate::types::StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };
            })
        }

        fn stream_simple(
            &self,
            model: Model,
            context: Context,
            options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream<'_> {
            self.stream(model, context, options.map(|o| o.base))
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let registry = ApiProviderRegistry::new();
        let provider = Box::new(MockProvider);

        registry.register(Api::OpenAICompletions, provider, None);

        assert!(registry.has(&Api::OpenAICompletions));
        assert!(!registry.has(&Api::AnthropicMessages));

        let retrieved = registry.get(&Api::OpenAICompletions);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().api, Api::OpenAICompletions);
    }

    #[test]
    fn test_registry_unregister_by_source() {
        let registry = ApiProviderRegistry::new();
        let provider = Box::new(MockProvider);

        registry.register(
            Api::OpenAICompletions,
            provider,
            Some("test-source".to_string()),
        );
        assert!(registry.has(&Api::OpenAICompletions));

        registry.unregister_by_source("test-source");
        assert!(!registry.has(&Api::OpenAICompletions));
    }

    #[test]
    fn test_registry_clear() {
        let registry = ApiProviderRegistry::new();
        let provider = Box::new(MockProvider);

        registry.register(Api::OpenAICompletions, provider, None);
        assert!(registry.has(&Api::OpenAICompletions));

        registry.clear();
        assert!(!registry.has(&Api::OpenAICompletions));
    }
}
