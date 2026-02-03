//! API provider registry.
//!
//! Defines the ApiProvider trait and allows registering API providers
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
pub trait ApiProvider: Send + Sync {
    /// Stream chat completions with full options.
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static>;

    /// Stream chat completions with simplified options.
    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static>;
}

/// A boxed provider trait object that can be shared across threads.
pub type BoxedApiProvider = Box<dyn ApiProvider + Send + Sync>;

struct RegisteredApiProvider {
    provider: Arc<BoxedApiProvider>,
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
    pub fn register(&self, api: Api, provider: BoxedApiProvider, source_id: Option<String>) {
        let mut providers = self.providers.write().unwrap();
        providers.insert(
            api,
            RegisteredApiProvider {
                provider: Arc::new(provider),
                source_id,
            },
        );
    }

    /// Get an API provider by API type.
    pub fn get(&self, api: &Api) -> Option<Arc<BoxedApiProvider>> {
        let providers = self.providers.read().unwrap();
        providers.get(api).map(|r| r.provider.clone())
    }

    /// Get all registered API providers.
    pub fn get_all(&self) -> Vec<Arc<BoxedApiProvider>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider;

    impl ApiProvider for MockProvider {
        fn stream(
            &self,
            _model: Model,
            _context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
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
        ) -> AssistantMessageEventStream<'static> {
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
