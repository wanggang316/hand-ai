//! Model interface types for the unified model system.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Serialize an optional string-keyed map with its keys in sorted order.
///
/// `HashMap` iterates in a random order, which makes the generated model
/// catalog (and any other serialized `Model`/`Compat`) non-deterministic on
/// disk. The in-memory type stays a `HashMap` — this only fixes the
/// serialized byte order — so the catalog round-trips to a stable,
/// reviewable form and regeneration stays churn-free.
fn serialize_sorted_map<S, V>(
    map: &Option<HashMap<String, V>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    V: Serialize,
{
    match map {
        Some(m) => {
            let sorted: BTreeMap<&String, &V> = m.iter().collect();
            sorted.serialize(serializer)
        }
        None => serializer.serialize_none(),
    }
}

/// Transport mechanism for streaming responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// Server-Sent Events (streaming).
    Sse,
    /// WebSocket connection.
    Websocket,
    /// WebSocket with caching support.
    WebsocketCached,
    /// Auto-detect best transport.
    Auto,
}

/// Cache retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    /// No caching.
    None,
    /// Short-term cache.
    Short,
    /// Long-term cache.
    Long,
}

impl CacheRetention {
    /// Resolve the effective cache retention for a stream.
    ///
    /// - If the caller passed an explicit value, honour it.
    /// - Otherwise, read the `PI_CACHE_RETENTION` env var: a value of
    ///   `"long"` (case-insensitive) opts in to long retention. Any
    ///   other value, or an unset variable, falls back to `Short`.
    ///
    /// The env-var fallback lets operators turn on 1h Anthropic caches
    /// and 24h OpenAI caches globally without threading a knob through
    /// every call site.
    pub fn resolve(explicit: Option<CacheRetention>) -> CacheRetention {
        if let Some(value) = explicit {
            return value;
        }
        match std::env::var("PI_CACHE_RETENTION") {
            Ok(v) if v.eq_ignore_ascii_case("long") => CacheRetention::Long,
            _ => CacheRetention::Short,
        }
    }
}

/// HTTP response metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
}

/// Supported input modalities for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    /// Plain text input.
    Text,
    /// Image input (base64-encoded).
    Image,
}

/// Cost per million tokens (USD).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cost {
    /// $/million tokens
    pub input: f64,
    /// $/million tokens
    pub output: f64,
    /// $/million tokens
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    /// $/million tokens
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
}

/// API identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
    /// OpenAI Chat Completions API.
    #[serde(rename = "openai-completions")]
    OpenAICompletions,
    /// OpenAI Responses API.
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    /// Azure-hosted OpenAI Responses API.
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    /// OpenAI Codex Responses API.
    #[serde(rename = "openai-codex-responses")]
    OpenAICodexResponses,
    /// Anthropic Messages API.
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    /// AWS Bedrock Converse Stream API.
    #[serde(rename = "bedrock-converse-stream")]
    BedrockConverseStream,
    /// Google Generative AI API (Gemini).
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    /// Google Gemini CLI API.
    #[serde(rename = "google-gemini-cli")]
    GoogleGeminiCli,
    /// Google Vertex AI API.
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    /// Mistral Conversations API.
    #[serde(rename = "mistral-conversations")]
    MistralConversations,
    /// In-memory faux API used by the test/parity harness. Gated behind the
    /// `faux` Cargo feature in callers; the variant itself lives in the core
    /// type so registrations can key on it.
    #[serde(rename = "faux")]
    Faux,
}

/// Provider identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[serde(rename = "amazon-bedrock")]
    AmazonBedrock,
    Anthropic,
    Google,
    #[serde(rename = "google-gemini-cli")]
    GoogleGeminiCli,
    #[serde(rename = "google-antigravity")]
    GoogleAntigravity,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    #[serde(rename = "openai-codex")]
    OpenAICodex,
    #[serde(rename = "github-copilot")]
    GitHubCopilot,
    Xai,
    Groq,
    Cerebras,
    Openrouter,
    #[serde(rename = "vercel-ai-gateway")]
    VercelAiGateway,
    Zai,
    Mistral,
    Minimax,
    #[serde(rename = "minimax-cn")]
    MinimaxCn,
    Huggingface,
    Opencode,
    #[serde(rename = "kimi-coding")]
    KimiCoding,
    #[serde(rename = "cloudflare-workers-ai")]
    CloudflareWorkersAi,
    #[serde(rename = "cloudflare-ai-gateway")]
    CloudflareAiGateway,
    Fireworks,
    Moonshotai,
    #[serde(rename = "moonshotai-cn")]
    MoonshotaiCn,
    Xiaomi,
    #[serde(rename = "xiaomi-token-plan-cn")]
    XiaomiTokenPlanCn,
    #[serde(rename = "xiaomi-token-plan-ams")]
    XiaomiTokenPlanAms,
    #[serde(rename = "xiaomi-token-plan-sgp")]
    XiaomiTokenPlanSgp,
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    Deepseek,
}

impl Provider {
    /// Serialized key (e.g. for JSON / registry lookup).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::AmazonBedrock => "amazon-bedrock",
            Provider::Anthropic => "anthropic",
            Provider::Google => "google",
            Provider::GoogleGeminiCli => "google-gemini-cli",
            Provider::GoogleAntigravity => "google-antigravity",
            Provider::GoogleVertex => "google-vertex",
            Provider::OpenAI => "openai",
            Provider::AzureOpenAiResponses => "azure-openai-responses",
            Provider::OpenAICodex => "openai-codex",
            Provider::GitHubCopilot => "github-copilot",
            Provider::Xai => "xai",
            Provider::Groq => "groq",
            Provider::Cerebras => "cerebras",
            Provider::Openrouter => "openrouter",
            Provider::VercelAiGateway => "vercel-ai-gateway",
            Provider::Zai => "zai",
            Provider::Mistral => "mistral",
            Provider::Minimax => "minimax",
            Provider::MinimaxCn => "minimax-cn",
            Provider::Huggingface => "huggingface",
            Provider::Opencode => "opencode",
            Provider::KimiCoding => "kimi-coding",
            Provider::CloudflareWorkersAi => "cloudflare-workers-ai",
            Provider::CloudflareAiGateway => "cloudflare-ai-gateway",
            Provider::Fireworks => "fireworks",
            Provider::Moonshotai => "moonshotai",
            Provider::MoonshotaiCn => "moonshotai-cn",
            Provider::Xiaomi => "xiaomi",
            Provider::XiaomiTokenPlanCn => "xiaomi-token-plan-cn",
            Provider::XiaomiTokenPlanAms => "xiaomi-token-plan-ams",
            Provider::XiaomiTokenPlanSgp => "xiaomi-token-plan-sgp",
            Provider::OpencodeGo => "opencode-go",
            Provider::Deepseek => "deepseek",
        }
    }

    /// Parse from registry key string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "amazon-bedrock" => Some(Provider::AmazonBedrock),
            "anthropic" => Some(Provider::Anthropic),
            "google" => Some(Provider::Google),
            "google-gemini-cli" => Some(Provider::GoogleGeminiCli),
            "google-antigravity" => Some(Provider::GoogleAntigravity),
            "google-vertex" => Some(Provider::GoogleVertex),
            "openai" => Some(Provider::OpenAI),
            "azure-openai-responses" => Some(Provider::AzureOpenAiResponses),
            "openai-codex" => Some(Provider::OpenAICodex),
            "github-copilot" => Some(Provider::GitHubCopilot),
            "xai" => Some(Provider::Xai),
            "groq" => Some(Provider::Groq),
            "cerebras" => Some(Provider::Cerebras),
            "openrouter" => Some(Provider::Openrouter),
            "vercel-ai-gateway" => Some(Provider::VercelAiGateway),
            "zai" => Some(Provider::Zai),
            "mistral" => Some(Provider::Mistral),
            "minimax" => Some(Provider::Minimax),
            "minimax-cn" => Some(Provider::MinimaxCn),
            "huggingface" => Some(Provider::Huggingface),
            "opencode" => Some(Provider::Opencode),
            "kimi-coding" => Some(Provider::KimiCoding),
            "cloudflare-workers-ai" => Some(Provider::CloudflareWorkersAi),
            "cloudflare-ai-gateway" => Some(Provider::CloudflareAiGateway),
            "fireworks" => Some(Provider::Fireworks),
            "moonshotai" => Some(Provider::Moonshotai),
            "moonshotai-cn" => Some(Provider::MoonshotaiCn),
            "xiaomi" => Some(Provider::Xiaomi),
            "xiaomi-token-plan-cn" => Some(Provider::XiaomiTokenPlanCn),
            "xiaomi-token-plan-ams" => Some(Provider::XiaomiTokenPlanAms),
            "xiaomi-token-plan-sgp" => Some(Provider::XiaomiTokenPlanSgp),
            "opencode-go" => Some(Provider::OpencodeGo),
            "deepseek" => Some(Provider::Deepseek),
            _ => None,
        }
    }
}

/// Header convention used to carry the session id for cache affinity.
///
/// Only consulted when session-affinity headers are enabled for the
/// model; the format decides *which* headers are emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAffinityFormat {
    /// OpenAI-style set: `session_id`, `x-client-request-id`,
    /// `x-session-affinity`.
    #[serde(rename = "openai")]
    OpenAI,
    /// OpenAI-style set minus the underscore-containing `session_id`
    /// header, for proxies that reject non-token header names.
    #[serde(rename = "openai-nosession")]
    OpenAINoSession,
    /// Single `x-session-id` header, as read by OpenRouter's
    /// prompt-cache routing.
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// Compatibility overrides for OpenAI Completions API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAICompletionsCompat {
    #[serde(skip_serializing_if = "Option::is_none", rename = "supportsStore")]
    pub supports_store: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsDeveloperRole"
    )]
    pub supports_developer_role: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsReasoningEffort"
    )]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "thinkingFormat")]
    pub thinking_format: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsUsageInStreaming"
    )]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxTokensField")]
    pub max_tokens_field: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "requiresToolResultName"
    )]
    pub requires_tool_result_name: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "requiresAssistantAfterToolResult"
    )]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "requiresThinkingAsText"
    )]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "requiresMistralToolIds"
    )]
    pub requires_mistral_tool_ids: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "openRouterRouting")]
    pub open_router_routing: Option<OpenRouterRouting>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "vercelGatewayRouting"
    )]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "supportsStrictMode")]
    pub supports_strict_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "cacheControlFormat")]
    pub cache_control_format: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "sendSessionAffinityHeaders"
    )]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "sessionAffinityFormat"
    )]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsLongCacheRetention"
    )]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "zaiToolStream")]
    pub zai_tool_stream: Option<bool>,
    /// When `true`, the upstream requires every assistant message in
    /// replayed context to carry a `reasoning_content` field. The
    /// native DeepSeek API enforces this; OpenRouter's DeepSeek V3/V4
    /// shim does the same. Without the flag, replayed turns lose
    /// their reasoning blocks and the model refuses the request.
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "requiresReasoningContentOnAssistantMessages"
    )]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
}

/// Compatibility overrides for OpenAI Responses API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAIResponsesCompat {
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "sendSessionIdHeader"
    )]
    pub send_session_id_header: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsLongCacheRetention"
    )]
    pub supports_long_cache_retention: Option<bool>,
}

/// Compatibility overrides for Anthropic Messages API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnthropicMessagesCompat {
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsEagerToolInputStreaming"
    )]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "supportsLongCacheRetention"
    )]
    pub supports_long_cache_retention: Option<bool>,
}

/// API-specific compatibility overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Compat {
    #[serde(rename = "openai-completions")]
    OpenAICompletions(Box<OpenAICompletionsCompat>),
    #[serde(rename = "openai-responses")]
    OpenAIResponses(OpenAIResponsesCompat),
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages(AnthropicMessagesCompat),
}

/// Token usage and cost for a completion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input (prompt) tokens.
    pub input: u64,
    /// Number of output (completion) tokens.
    pub output: u64,
    /// Tokens read from cache.
    #[serde(rename = "cacheRead")]
    pub cache_read: u64,
    /// Tokens written to cache.
    #[serde(rename = "cacheWrite")]
    pub cache_write: u64,
    /// Total tokens (input + output + cache).
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    /// Cost breakdown in USD.
    pub cost: UsageCost,
}

/// Cost breakdown (USD).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
    pub total: f64,
}

/// Model interface for the unified model system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    /// Unique model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Which API this model uses.
    pub api: Api,
    /// Which provider hosts this model.
    pub provider: Provider,
    /// Base URL for API requests.
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    /// Whether the model supports reasoning/thinking.
    pub reasoning: bool,
    /// Supported input modalities.
    pub input: Vec<InputType>,
    /// Cost per million tokens.
    pub cost: Cost,
    /// Maximum context window size in tokens.
    #[serde(rename = "contextWindow")]
    pub context_window: u64,
    /// Maximum output tokens per request.
    #[serde(rename = "maxTokens")]
    pub max_tokens: u64,
    /// Custom headers to include in API requests.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_sorted_map"
    )]
    pub headers: Option<HashMap<String, String>>,
    /// Compatibility overrides for OpenAI-compatible APIs. If not set, auto-detected from base_url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
    /// Mapping of thinking levels to provider-specific format strings.
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "thinkingLevelMap",
        serialize_with = "serialize_sorted_map"
    )]
    pub thinking_level_map: Option<ThinkingLevelMap>,
}

/// OpenRouter provider routing preferences. Field names mirror the
/// OpenRouter API directly so the struct can be serialized straight
/// into the `provider` object without per-field translation.
/// Polymorphic fields (`sort`, `max_price`, `preferred_min_throughput`,
/// `preferred_max_latency`) accept either a primitive or a nested
/// object and are stored as raw `serde_json::Value` so callers can
/// pass them through without modeling every variant up front.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    /// Sort strategy. Accepts a string (`"price"`, `"throughput"`,
    /// `"latency"`) or an object with `by` / `partition` fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<serde_json::Value>,
    /// Maximum price per million tokens, keyed by modality
    /// (`prompt`, `completion`, `image`, `audio`, `request`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_price: Option<serde_json::Value>,
    /// Preferred minimum throughput (tokens/second). Number or
    /// percentile-keyed object (`p50`, `p75`, `p90`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<serde_json::Value>,
    /// Preferred maximum latency. Number or percentile-keyed object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<serde_json::Value>,
}

/// Vercel AI Gateway routing preferences.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

// =============================================================================
// Message Types
// =============================================================================

/// Thinking level for reasoning models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// Minimal reasoning (1024 token budget).
    Minimal,
    /// Low reasoning (2048 token budget).
    Low,
    /// Medium reasoning (8192 token budget).
    Medium,
    /// High reasoning (16384 token budget).
    High,
    /// Extra high reasoning (clamped to High for most providers).
    Xhigh,
}

/// Token budgets for each thinking level (token-based providers only).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    pub minimal: Option<u32>,
    pub low: Option<u32>,
    pub medium: Option<u32>,
    pub high: Option<u32>,
}

/// Mapping of thinking levels to format strings.
pub type ThinkingLevelMap = HashMap<String, Option<String>>;

/// Callback invoked with each outbound request payload before it is sent.
pub type OnPayloadCallback = Arc<dyn Fn(serde_json::Value, &Model) + Send + Sync>;

/// Callback invoked with the HTTP response status and headers from the provider.
pub type OnResponseCallback = Arc<dyn Fn(u16, HashMap<String, String>, &Model) + Send + Sync>;

/// Base options all providers share.
#[derive(Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_sorted_map"
    )]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_sorted_map"
    )]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(skip)]
    pub signal: Option<tokio_util::sync::CancellationToken>,
    /// TODO(M12): revisit return type when wiring up SSE pipeline — TS equivalent
    /// returns `Promise<unknown>` for async payload mutation; this Rust signature
    /// is sync-only and can be upgraded to `-> Pin<Box<dyn Future<Output = ()>>>`
    /// when the streaming pipeline lands.
    #[serde(skip)]
    pub on_payload: Option<OnPayloadCallback>,
    #[serde(skip)]
    pub on_response: Option<OnResponseCallback>,
}

impl std::fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOptions")
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("session_id", &self.session_id)
            .field("headers", &self.headers)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("transport", &self.transport)
            .field("cache_retention", &self.cache_retention)
            .field("metadata", &self.metadata)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field(
                "signal",
                &self.signal.as_ref().map(|_| "<CancellationToken>"),
            )
            .field("on_payload", &self.on_payload.as_ref().map(|_| "<Fn>"))
            .field("on_response", &self.on_response.as_ref().map(|_| "<Fn>"))
            .finish()
    }
}

/// Provider-specific stream options.
pub type ProviderStreamOptions = StreamOptions;

/// Unified options with reasoning passed to stream_simple() and complete_simple().
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SimpleStreamOptions {
    #[serde(flatten)]
    pub base: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl SimpleStreamOptions {
    pub fn temperature(&self) -> Option<f32> {
        self.base.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.base.max_tokens
    }

    pub fn api_key(&self) -> Option<&str> {
        self.base.api_key.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.base.session_id.as_deref()
    }

    pub fn headers(&self) -> Option<&HashMap<String, String>> {
        self.base.headers.as_ref()
    }

    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        self.base.max_retry_delay_ms
    }

    /// Build base stream options with defaults applied.
    ///
    /// If max_tokens is not specified, defaults to min(model.max_tokens, 32000).
    /// When `model.max_tokens == 0` (catalog entry hasn't pinned a
    /// ceiling), the field stays `None` so the wire request omits it
    /// rather than transmitting a literal `0` — providers that gate
    /// on the absence of `max_tokens` (Bedrock's `inferenceConfig`,
    /// for one) need to see the field dropped, not zeroed.
    pub fn build_base_options(&self, model: &Model, api_key: Option<String>) -> StreamOptions {
        StreamOptions {
            temperature: self.base.temperature,
            max_tokens: self.base.max_tokens.or_else(|| {
                if model.max_tokens == 0 {
                    None
                } else {
                    Some(model.max_tokens.min(32000) as u32)
                }
            }),
            api_key: api_key.or_else(|| self.base.api_key.clone()),
            session_id: self.base.session_id.clone(),
            headers: self.base.headers.clone(),
            max_retry_delay_ms: self.base.max_retry_delay_ms,
            transport: self.base.transport,
            cache_retention: self.base.cache_retention,
            metadata: self.base.metadata.clone(),
            timeout_ms: self.base.timeout_ms,
            max_retries: self.base.max_retries,
            signal: self.base.signal.clone(),
            on_payload: self.base.on_payload.clone(),
            on_response: self.base.on_response.clone(),
        }
    }

    /// Clamp thinking level to exclude "xhigh".
    pub fn clamp_reasoning(&self) -> Option<ThinkingLevel> {
        self.reasoning.map(|r| match r {
            ThinkingLevel::Xhigh => ThinkingLevel::High,
            _ => r,
        })
    }

    /// Adjust max_tokens for thinking/reasoning models.
    ///
    /// Returns the adjusted max_tokens and thinking budget.
    pub fn adjust_max_tokens_for_thinking(
        &self,
        base_max_tokens: u32,
        model_max_tokens: u64,
    ) -> (u32, u32) {
        if self.reasoning.is_none() {
            return (base_max_tokens, 0);
        }

        let level = self.clamp_reasoning().unwrap_or(ThinkingLevel::High);

        // Default budgets for each thinking level
        let default_budgets = ThinkingBudgets {
            minimal: Some(1024),
            low: Some(2048),
            medium: Some(8192),
            high: Some(16384),
        };

        // Merge with custom budgets if provided
        let budgets = self
            .thinking_budgets
            .as_ref()
            .map(|custom| ThinkingBudgets {
                minimal: custom.minimal.or(default_budgets.minimal),
                low: custom.low.or(default_budgets.low),
                medium: custom.medium.or(default_budgets.medium),
                high: custom.high.or(default_budgets.high),
            })
            .unwrap_or(default_budgets);

        let thinking_budget = match level {
            ThinkingLevel::Minimal => budgets.minimal.unwrap_or(1024),
            ThinkingLevel::Low => budgets.low.unwrap_or(2048),
            ThinkingLevel::Medium => budgets.medium.unwrap_or(8192),
            ThinkingLevel::High | ThinkingLevel::Xhigh => budgets.high.unwrap_or(16384),
        };

        const MIN_OUTPUT_TOKENS: u32 = 1024;

        let max_tokens = (base_max_tokens + thinking_budget).min(model_max_tokens as u32);

        let thinking_budget = if max_tokens <= thinking_budget {
            max_tokens.saturating_sub(MIN_OUTPUT_TOKENS)
        } else {
            thinking_budget
        };

        (max_tokens, thinking_budget)
    }
}

/// Text content block.
///
/// `content_type` is `#[serde(skip)]` because every place that
/// serialises a `TextContent` wraps it in a `tag = "type"` enum
/// (`UserContentBlock`, `AssistantContentBlock`, `ToolResultContent`)
/// that already emits `"type":"text"` on the outer object. Emitting
/// the inner field too produced a duplicate JSON key and broke
/// deserialization of every session that contained an assistant text
/// block (issue #19). The field stays in the struct so construction-
/// site code that reads it keeps compiling; on deserialize the
/// default fills it back in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    #[serde(skip, default = "default_text_content_type")]
    pub content_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "textSignature")]
    pub text_signature: Option<String>,
}

fn default_text_content_type() -> String {
    "text".to_string()
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            content_type: "text".to_string(),
            text: text.into(),
            text_signature: None,
        }
    }
}

/// Thinking content block. See [`TextContent`] for the rationale
/// behind `#[serde(skip)]` on `content_type` — same duplicate-tag
/// issue, same fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingContent {
    #[serde(skip, default = "default_thinking_content_type")]
    pub content_type: String,
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "thinkingSignature")]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

fn default_thinking_content_type() -> String {
    "thinking".to_string()
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            content_type: "thinking".to_string(),
            thinking: thinking.into(),
            thinking_signature: None,
            redacted: None,
        }
    }
}

/// Image content block. See [`TextContent`] for the rationale
/// behind `#[serde(skip)]` on `content_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    #[serde(skip, default = "default_image_content_type")]
    pub content_type: String,
    pub data: String,
    pub mime_type: String,
}

fn default_image_content_type() -> String {
    "image".to_string()
}

impl ImageContent {
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            content_type: "image".to_string(),
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

/// Tool call content block. See [`TextContent`] for the rationale
/// behind `#[serde(skip)]` on `content_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(skip, default = "default_tool_call_content_type")]
    pub content_type: String,
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none", rename = "thoughtSignature")]
    pub thought_signature: Option<String>,
}

fn default_tool_call_content_type() -> String {
    "toolCall".to_string()
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            content_type: "toolCall".to_string(),
            id: id.into(),
            name: name.into(),
            arguments,
            thought_signature: None,
        }
    }
}

/// Reason why the assistant stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// Model finished naturally.
    Stop,
    /// Max tokens reached.
    Length,
    /// Model wants to call tools.
    ToolUse,
    /// An error occurred.
    Error,
    /// Request was aborted by the caller.
    Aborted,
}

/// User content variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

/// User content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// A message from the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(skip, default = "default_user_role")]
    pub role: String,
    pub content: UserContent,
    pub timestamp: u64,
}

fn default_user_role() -> String {
    "user".to_string()
}

impl UserMessage {
    pub fn new_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: UserContent::Text(text.into()),
            timestamp: current_timestamp_ms(),
        }
    }

    pub fn new_blocks(blocks: Vec<UserContentBlock>) -> Self {
        Self {
            role: "user".to_string(),
            content: UserContent::Blocks(blocks),
            timestamp: current_timestamp_ms(),
        }
    }
}

// `AssistantMessageDiagnostic` lives in `crate::utils::diagnostics`; it is
// re-exported below for backwards compatibility with M1 callers.
pub use crate::utils::diagnostics::{AssistantMessageDiagnostic, DiagnosticKind};

/// Assistant content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AssistantContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

/// A message from the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    #[serde(skip, default = "default_assistant_role")]
    pub role: String,
    pub content: Vec<AssistantContentBlock>,
    pub api: Api,
    pub provider: Provider,
    pub model: String,
    pub usage: Usage,
    #[serde(rename = "stopReason")]
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none", rename = "errorMessage")]
    pub error_message: Option<String>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none", rename = "responseModel")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "responseId")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
}

fn default_assistant_role() -> String {
    "assistant".to_string()
}

/// Tool result content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultContent {
    Text(TextContent),
    Image(ImageContent),
}

/// A tool result message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    #[serde(skip, default = "default_tool_result_role")]
    pub role: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    pub content: Vec<ToolResultContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(rename = "isError")]
    pub is_error: bool,
    pub timestamp: u64,
}

fn default_tool_result_role() -> String {
    "toolResult".to_string()
}

impl ToolResultMessage {
    pub fn new(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: Vec<ToolResultContent>,
    ) -> Self {
        Self {
            role: "toolResult".to_string(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content,
            details: None,
            is_error: false,
            timestamp: current_timestamp_ms(),
        }
    }

    /// Create a tool result that represents an error.
    pub fn new_error(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        error_text: impl Into<String>,
    ) -> Self {
        Self {
            role: "toolResult".to_string(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: vec![ToolResultContent::Text(TextContent::new(error_text))],
            details: None,
            is_error: true,
            timestamp: current_timestamp_ms(),
        }
    }
}

/// Any message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

/// Tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl Tool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// Context for a streaming request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none", rename = "systemPrompt")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// Events emitted during streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    TextDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: u32,
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod types_tests {
    use super::{
        Api, CacheRetention, Cost, InputType, Model, Provider, SimpleStreamOptions, StreamOptions,
        Transport,
    };

    /// `build_base_options` must propagate every transport-shaping option the
    /// caller set on `SimpleStreamOptions` into the inner `StreamOptions`.
    /// Forgetting to forward `transport` is the bug behind #4083 — a caller
    /// asks for websocket-cached and silently gets SSE because the option
    /// never crosses the boundary.
    #[test]
    fn build_base_options_forwards_transport_and_cache_retention() {
        let model = Model {
            id: "test-model".to_string(),
            name: "Test".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: "https://example.com".to_string(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 1000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        };
        let opts = SimpleStreamOptions {
            base: StreamOptions {
                transport: Some(Transport::WebsocketCached),
                cache_retention: Some(CacheRetention::Long),
                ..Default::default()
            },
            ..Default::default()
        };
        let built = opts.build_base_options(&model, None);
        assert_eq!(built.transport, Some(Transport::WebsocketCached));
        assert_eq!(built.cache_retention, Some(CacheRetention::Long));
    }

    /// When `model.max_tokens == 0` (the catalog entry hasn't pinned a
    /// ceiling) and the caller didn't specify a value, `max_tokens`
    /// must stay `None` so the wire request omits the field. Sending
    /// a literal `0` would make Bedrock's `inferenceConfig.maxTokens`
    /// reserve no output capacity at all from the TPM quota window.
    #[test]
    fn build_base_options_omits_max_tokens_when_model_default_is_zero() {
        let mut model = Model {
            id: "test-zero-max".to_string(),
            name: "Test".to_string(),
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            base_url: "https://example.com".to_string(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
            thinking_level_map: None,
        };
        let opts = SimpleStreamOptions::default();
        let built = opts.build_base_options(&model, None);
        assert_eq!(built.max_tokens, None);

        // Non-zero model max gets the min(.., 32000) default.
        model.max_tokens = 100_000;
        let built = opts.build_base_options(&model, None);
        assert_eq!(built.max_tokens, Some(32_000));

        // Caller-supplied value wins regardless of model default.
        let opts = SimpleStreamOptions {
            base: StreamOptions {
                max_tokens: Some(512),
                ..Default::default()
            },
            ..Default::default()
        };
        let built = opts.build_base_options(&model, None);
        assert_eq!(built.max_tokens, Some(512));
    }

    /// An explicit caller value always wins over `PI_CACHE_RETENTION`,
    /// even when the env var is set. The env-var fallback only kicks in
    /// when the caller passed `None`.
    #[test]
    fn cache_retention_resolve_honours_explicit_value() {
        let prior = std::env::var("PI_CACHE_RETENTION").ok();
        unsafe {
            std::env::set_var("PI_CACHE_RETENTION", "long");
        }
        assert_eq!(
            CacheRetention::resolve(Some(CacheRetention::None)),
            CacheRetention::None,
        );
        assert_eq!(
            CacheRetention::resolve(Some(CacheRetention::Short)),
            CacheRetention::Short,
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("PI_CACHE_RETENTION", v),
                None => std::env::remove_var("PI_CACHE_RETENTION"),
            }
        }
    }

    /// With no explicit value, `PI_CACHE_RETENTION=long` (any case)
    /// promotes the default to `Long`. Any other value or an unset
    /// variable falls back to `Short`.
    #[test]
    fn cache_retention_resolve_reads_pi_cache_retention_env() {
        let prior = std::env::var("PI_CACHE_RETENTION").ok();

        unsafe {
            std::env::set_var("PI_CACHE_RETENTION", "long");
        }
        assert_eq!(CacheRetention::resolve(None), CacheRetention::Long);

        unsafe {
            std::env::set_var("PI_CACHE_RETENTION", "LONG");
        }
        assert_eq!(CacheRetention::resolve(None), CacheRetention::Long);

        unsafe {
            std::env::set_var("PI_CACHE_RETENTION", "short");
        }
        assert_eq!(CacheRetention::resolve(None), CacheRetention::Short);

        unsafe {
            std::env::remove_var("PI_CACHE_RETENTION");
        }
        assert_eq!(CacheRetention::resolve(None), CacheRetention::Short);

        unsafe {
            match prior {
                Some(v) => std::env::set_var("PI_CACHE_RETENTION", v),
                None => std::env::remove_var("PI_CACHE_RETENTION"),
            }
        }
    }

    #[test]
    fn provider_roundtrip() {
        let providers = [
            Provider::AmazonBedrock,
            Provider::Anthropic,
            Provider::Google,
            Provider::GoogleGeminiCli,
            Provider::GoogleAntigravity,
            Provider::GoogleVertex,
            Provider::OpenAI,
            Provider::AzureOpenAiResponses,
            Provider::OpenAICodex,
            Provider::GitHubCopilot,
            Provider::Xai,
            Provider::Groq,
            Provider::Cerebras,
            Provider::Openrouter,
            Provider::VercelAiGateway,
            Provider::Zai,
            Provider::Mistral,
            Provider::Minimax,
            Provider::MinimaxCn,
            Provider::Huggingface,
            Provider::Opencode,
            Provider::KimiCoding,
            Provider::CloudflareWorkersAi,
            Provider::CloudflareAiGateway,
            Provider::Fireworks,
            Provider::Moonshotai,
            Provider::MoonshotaiCn,
            Provider::Xiaomi,
            Provider::XiaomiTokenPlanCn,
            Provider::XiaomiTokenPlanAms,
            Provider::XiaomiTokenPlanSgp,
            Provider::OpencodeGo,
            Provider::Deepseek,
        ];
        for provider in providers {
            let key = provider.as_str();
            assert_eq!(Provider::from_str(key), Some(provider));
        }
    }

    /// `OpenRouterRouting` serializes every set field with its
    /// upstream API name. The whole struct goes directly into the
    /// `provider` object of an OpenRouter request — any field-name
    /// drift here would silently break routing preferences.
    #[test]
    fn open_router_routing_serializes_with_snake_case_api_names() {
        let routing = super::OpenRouterRouting {
            only: Some(vec!["anthropic".to_string()]),
            order: Some(vec!["anthropic".to_string(), "openai".to_string()]),
            allow_fallbacks: Some(false),
            require_parameters: Some(true),
            data_collection: Some("deny".to_string()),
            zdr: Some(true),
            enforce_distillable_text: Some(true),
            ignore: Some(vec!["mistralai".to_string()]),
            quantizations: Some(vec!["bf16".to_string(), "fp16".to_string()]),
            sort: Some(serde_json::json!("throughput")),
            max_price: Some(serde_json::json!({ "prompt": 5, "completion": 10 })),
            preferred_min_throughput: Some(serde_json::json!(50)),
            preferred_max_latency: Some(serde_json::json!({ "p90": 500 })),
        };
        let body = serde_json::to_value(&routing).expect("serialize ok");
        // All keys must match OpenRouter's snake_case wire form
        // (NOT the legacy `allowFallbacks` / `dataCollection`
        // camelCase that we mistakenly used before).
        let obj = body.as_object().expect("object");
        for key in [
            "only",
            "order",
            "allow_fallbacks",
            "require_parameters",
            "data_collection",
            "zdr",
            "enforce_distillable_text",
            "ignore",
            "quantizations",
            "sort",
            "max_price",
            "preferred_min_throughput",
            "preferred_max_latency",
        ] {
            assert!(obj.contains_key(key), "missing key {key}: {body}");
        }
        // No camelCase leftovers.
        for legacy in ["allowFallbacks", "dataCollection"] {
            assert!(
                !obj.contains_key(legacy),
                "legacy camelCase key {legacy} must not be emitted: {body}"
            );
        }
    }

    /// `skip_serializing_if = "Option::is_none"` is on every field,
    /// so a default (empty) `OpenRouterRouting` must serialize to
    /// `{}`. Callers can detect "nothing set" and omit the
    /// `provider` block entirely instead of submitting an empty one.
    #[test]
    fn open_router_routing_default_serializes_to_empty_object() {
        let body = serde_json::to_value(super::OpenRouterRouting::default()).expect("serialize");
        assert_eq!(body, serde_json::json!({}));
    }
}
