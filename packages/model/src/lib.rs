//! Model and AI provider system for unified model access.
//!
//! This crate provides a unified interface for interacting with various
//! AI providers (OpenAI, Anthropic, Google, etc.) through a common API.
//!

pub mod api_registry;
pub mod env_api_keys;
pub mod models;
pub mod stream;
pub mod types;

// Re-export commonly used items from types
pub use types::{
    Api, AssistantContentBlock, AssistantContentBlock as AssistantContent, AssistantMessage,
    AssistantMessageEvent, Compat, Context, Cost, ImageContent, InputType, Message, Model,
    OpenAICompletionsCompat, OpenAIResponsesCompat, OpenRouterRouting, Provider,
    ProviderStreamOptions, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingBudgets, ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolResultContent,
    ToolResultMessage, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    VercelGatewayRouting,
};

// Re-export from models module
pub use models::{
    calculate_cost, get_model, get_model_by_provider, get_models, get_models_by_provider,
    get_provider_keys, get_providers, models, models_are_equal, supports_xhigh,
};

// Re-export from api_registry
pub use api_registry::{
    ApiProvider, ApiProviderInternal, AssistantMessageEventStream, clear_api_providers,
    get_api_provider, get_api_providers, register_api_provider, unregister_api_providers,
};

// Re-export from stream
pub use stream::{complete, complete_simple, stream, stream_simple};

// Re-export from env_api_keys
pub use env_api_keys::{clear_vertex_adc_cache, get_env_api_key, get_env_api_key_by_str};
