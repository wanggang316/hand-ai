//! Model and AI provider system for unified model access.
//!
//! This crate provides a unified interface for interacting with various
//! AI providers (OpenAI, Anthropic, Google, etc.) through a common API.

pub mod api_registry;
pub mod cli;
pub mod client;
pub mod env_api_keys;
pub mod models;
pub mod oauth;
pub mod providers;
pub mod transform;
pub mod types;
pub mod utils;

// Re-export commonly used items from types
pub use types::{
    AnthropicMessagesCompat, Api, AssistantContentBlock, AssistantContentBlock as AssistantContent,
    AssistantMessage, AssistantMessageEvent, CacheRetention, Compat, Context, Cost, ImageContent,
    InputType, Message, Model, OnPayloadCallback, OnResponseCallback, OpenAICompletionsCompat,
    OpenAIResponsesCompat, OpenRouterRouting, ProviderResponse, ProviderStreamOptions,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, ThinkingLevelMap, Tool, ToolCall, ToolResultContent, ToolResultMessage,
    Transport, Usage, UsageCost, UserContent, UserContentBlock, UserMessage, VercelGatewayRouting,
};

// Re-export from utils
pub use utils::sanitize_unicode::{sanitize, sanitize_bytes};
pub use utils::{
    AssistantMessageDiagnostic, DiagnosticKind, EventStream, Provenance, ValidationIssue,
    ValidationIssueKind, is_context_overflow, merge_headers, safe_parse_partial, sha256_hex,
    try_parse_strict, validate_context,
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
    AnthropicMessagesProvider, BedrockProvider, GoogleGenerativeAiProvider,
    OpenAICompletionsOptions, OpenAICompletionsProvider, OpenAIResponsesProvider, ResolvedCompat,
    convert_messages, normalize_mistral_tool_id, stream_openai_completions,
};
#[cfg(any(test, feature = "faux"))]
pub use providers::{FauxProvider, FauxScriptStep, faux_model};

// Re-export from oauth
pub use oauth::{
    OAuthAuthInfo, OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider,
    OAuthProviderId, OAuthRegistry,
};

// Re-export from transform
pub use transform::{
    NormalizeToolCallIdFn, normalize_tool_call_id_for_anthropic,
    supports_eager_tool_input_streaming, transform_messages,
};
