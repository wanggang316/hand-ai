//! API provider registry.
//!
//! Allows registering API providers that can handle streaming requests
//! for specific API types.

use crate::types::{
    Api, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions,
};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::RwLock;
use std::sync::Arc;

/// Type alias for a stream of assistant message events.
pub type AssistantMessageEventStream =
    Pin<Box<dyn futures::Stream<Item = AssistantMessageEvent> + Send>>;

/// Function type for streaming with full options.
pub type StreamFunction =
    Arc<dyn Fn(Model, Context, Option<StreamOptions>) -> AssistantMessageEventStream + Send + Sync>;

/// Function type for simple streaming.
pub type StreamSimpleFunction = Arc<
    dyn Fn(Model, Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync,
>;

/// An API provider that can handle streaming requests.
pub struct ApiProvider {
    pub api: Api,
    pub stream: StreamFunction,
    pub stream_simple: StreamSimpleFunction,
}

/// Internal representation of an API provider.
#[derive(Clone)]
pub struct ApiProviderInternal {
    pub api: Api,
    pub stream: StreamFunction,
    pub stream_simple: StreamSimpleFunction,
}

struct RegisteredApiProvider {
    provider: ApiProviderInternal,
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
    pub fn register(
        &self,
        provider: ApiProvider,
        source_id: Option<String>,
    ) {
        let api = provider.api;
        let stream = wrap_stream(api, provider.stream);
        let stream_simple = wrap_stream_simple(api, provider.stream_simple);

        let mut providers = self.providers.write().unwrap();
        providers.insert(
            api,
            RegisteredApiProvider {
                provider: ApiProviderInternal {
                    api,
                    stream,
                    stream_simple,
                },
                source_id,
            },
        );
    }

    /// Get an API provider by API type.
    pub fn get(&self, api: &Api) -> Option<ApiProviderInternal> {
        let providers = self.providers.read().unwrap();
        providers.get(api).map(|r| r.provider.clone())
    }

    /// Get all registered API providers.
    pub fn get_all(&self) -> Vec<ApiProviderInternal> {
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
}

/// Global API provider registry.
static GLOBAL_REGISTRY: std::sync::OnceLock<ApiProviderRegistry> = std::sync::OnceLock::new();

/// Get the global API provider registry.
pub fn global_registry() -> &'static ApiProviderRegistry {
    GLOBAL_REGISTRY.get_or_init(ApiProviderRegistry::new)
}

/// Register an API provider in the global registry.
pub fn register_api_provider(
    provider: ApiProvider,
    source_id: Option<String>,
) {
    global_registry().register(provider, source_id);
}

/// Get an API provider from the global registry.
pub fn get_api_provider(api: &Api) -> Option<ApiProviderInternal> {
    global_registry().get(api)
}

/// Get all API providers from the global registry.
pub fn get_api_providers() -> Vec<ApiProviderInternal> {
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

fn wrap_stream(
    api: Api,
    stream: StreamFunction,
) -> StreamFunction {
    Arc::new(move |model: Model, context: Context, options: Option<StreamOptions>| {
        if model.api != api {
            panic!("Mismatched api: {:?} expected {:?}", model.api, api);
        }
        stream(model, context, options)
    })
}

fn wrap_stream_simple(
    api: Api,
    stream_simple: StreamSimpleFunction,
) -> StreamSimpleFunction {
    Arc::new(
        move |model: Model, context: Context, options: Option<SimpleStreamOptions>| {
            if model.api != api {
                panic!("Mismatched api: {:?} expected {:?}", model.api, api);
            }
            stream_simple(model, context, options)
        },
    )
}
