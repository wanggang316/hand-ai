//! Model interface types for the unified model system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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
        rename = "supportsLongCacheRetention"
    )]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "zaiToolStream")]
    pub zai_tool_stream: Option<bool>,
    /// When `true`, the upstream requires every assistant message in
    /// replayed context to carry a `reasoning_content` field. Native
    /// deepseek API enforces this; mirrors pi-mono's
    /// `requiresReasoningContentOnAssistantMessages`.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Compatibility overrides for OpenAI-compatible APIs. If not set, auto-detected from base_url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
    /// Mapping of thinking levels to provider-specific format strings.
    #[serde(skip_serializing_if = "Option::is_none", rename = "thinkingLevelMap")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
}

/// OpenRouter provider routing preferences.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowFallbacks")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "dataCollection")]
    pub data_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
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
pub struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    pub fn build_base_options(&self, model: &Model, api_key: Option<String>) -> StreamOptions {
        StreamOptions {
            temperature: self.base.temperature,
            max_tokens: self.base.max_tokens.or_else(|| {
                let default_max = model.max_tokens.min(32000) as u32;
                Some(default_max)
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "textSignature")]
    pub text_signature: Option<String>,
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

/// Thinking content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "thinkingSignature")]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
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

/// Image content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub data: String,
    pub mime_type: String,
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

/// Tool call content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "type")]
    pub content_type: String,
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none", rename = "thoughtSignature")]
    pub thought_signature: Option<String>,
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
    use super::Provider;

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
}
