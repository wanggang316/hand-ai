//! Model and AI provider system for unified model access.
//!
//! This crate provides a unified interface for interacting with various
//! AI providers (OpenAI, Anthropic, Google, etc.) through a common API.

pub mod api_registry;
pub mod cli;
pub mod client;
pub mod env_api_keys;
pub mod models;
pub mod overflow;
pub mod providers;
pub mod transform;
pub mod types;

// Re-export commonly used items from types
pub use types::{
    Api, AssistantContentBlock, AssistantContentBlock as AssistantContent, AssistantMessage,
    AssistantMessageEvent, Compat, Context, Cost, ImageContent, InputType, Message, Model,
    OpenAICompletionsCompat, OpenAIResponsesCompat, OpenRouterRouting, ProviderStreamOptions,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, Tool, ToolCall, ToolResultContent, ToolResultMessage, Usage, UsageCost,
    UserContent, UserContentBlock, UserMessage, VercelGatewayRouting,
};

// Re-export from models module
pub use models::{
    calculate_cost, get_model, get_model_by_provider, get_models, get_models_by_provider,
    get_provider_keys, get_providers, models, models_are_equal, supports_xhigh,
};

// Re-export from api_registry
pub use api_registry::{
    ApiProvider, ApiProviderRegistry, AssistantMessageEventStream, BoxedApiProvider,
};

// Re-export from client
pub use client::{Client, ClientError};

// Re-export from env_api_keys
pub use env_api_keys::{clear_vertex_adc_cache, get_env_api_key, get_env_api_key_by_str};

// Re-export from providers
pub use providers::{
    AnthropicMessagesProvider, GoogleGenerativeAiProvider, OpenAICompletionsOptions,
    OpenAICompletionsProvider, ResolvedCompat, convert_messages, normalize_mistral_tool_id,
    stream_openai_completions,
};

// Re-export from overflow
pub use overflow::is_context_overflow;

// Re-export from transform
pub use transform::{
    normalize_tool_call_id_for_anthropic, transform_messages, NormalizeToolCallIdFn,
};
