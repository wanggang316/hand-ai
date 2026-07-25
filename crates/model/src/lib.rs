//! Model and AI provider system for unified model access.
//!
//! This crate provides a unified interface for interacting with various
//! AI providers (OpenAI, Anthropic, Google, etc.) through a common API.

pub mod api_registry;
pub mod capabilities;
pub mod catalog_refresh;
#[cfg(feature = "cli")]
pub mod cli;
pub mod client;
pub mod env_api_keys;
pub mod models;
pub mod oauth;
pub mod providers;
pub mod session_resources;
pub mod stream;
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
    try_parse_strict, uuid_v7, validate_context,
};

// Re-export from models module
pub use models::{
    calculate_cost, get_model, get_model_by_provider, get_models, get_models_by_provider,
    get_provider_keys, get_providers, models, models_are_equal, supports_xhigh,
};

// Re-export from catalog_refresh
pub use catalog_refresh::{
    DEFAULT_CATALOG_URL, RefreshError, RefreshOutcome, load_cached_catalog, refresh_from_remote,
    resolve_catalog_url,
};

// Re-export from api_registry
pub use api_registry::{
    ApiProvider, ApiProviderRegistry, AssistantMessageEventStream, BoxedApiProvider,
};

// Re-export from capabilities
pub use capabilities::{ApiCapabilities, ProviderCapabilities};

// Re-export from client
pub use client::{Client, ClientBuilder, ClientError};

// Re-export from stream
pub use stream::{complete_simple, stream_simple};

// Re-export from env_api_keys
pub use env_api_keys::{clear_vertex_adc_cache, get_env_api_key, get_env_api_key_by_str};

// Re-export from providers
pub use providers::{
    AnthropicMessagesProvider, AzureOpenAIResponsesOptions, AzureOpenAIResponsesProvider,
    BedrockProvider, GoogleGenerativeAiProvider, GoogleVertexOptions, GoogleVertexProvider,
    GoogleVertexThinkingLevel, MistralOptions, MistralProvider, OpenAICodexResponsesOptions,
    OpenAICodexResponsesProvider, OpenAICodexWebSocketDebugStats, OpenAICompletionsOptions,
    OpenAICompletionsProvider, OpenAIResponsesProvider, ResolvedCompat, VertexTokenProvider,
    cloudflare_ai_gateway_model, cloudflare_workers_ai_model, convert_messages,
    normalize_mistral_tool_id, register_builtins, resolve_compat, stream_openai_completions,
    websocket_debug_stats as openai_codex_websocket_debug_stats,
};
#[cfg(any(test, feature = "faux"))]
pub use providers::{FauxProvider, FauxScriptStep, faux_model, register_builtins_with_faux};

// Re-export from oauth
pub use oauth::{
    OAuthAuthInfo, OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider,
    OAuthProviderId, OAuthRegistry, github_copilot_base_url, normalize_domain,
};

// Re-export from session_resources
pub use session_resources::{SessionResourceError, SessionResources, WebSocketHandle};

// Re-export from transform
pub use transform::{
    NormalizeToolCallIdFn, normalize_tool_call_id_for_anthropic,
    supports_eager_tool_input_streaming, transform_messages,
};
