//! Model interface types for the unified model system.

use serde::{Deserialize, Serialize};

/// Supported input modalities for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    Text,
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

/// Known API identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownApi {
    #[serde(rename = "openai-completions")]
    OpenAICompletions,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    #[serde(rename = "openai-codex-responses")]
    OpenAICodexResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "bedrock-converse-stream")]
    BedrockConverseStream,
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    #[serde(rename = "google-gemini-cli")]
    GoogleGeminiCli,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
}

/// Known provider identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownProvider {
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
}

impl KnownProvider {
    /// Serialized key (e.g. for JSON / registry lookup).
    pub fn as_str(self) -> &'static str {
        match self {
            KnownProvider::AmazonBedrock => "amazon-bedrock",
            KnownProvider::Anthropic => "anthropic",
            KnownProvider::Google => "google",
            KnownProvider::GoogleGeminiCli => "google-gemini-cli",
            KnownProvider::GoogleAntigravity => "google-antigravity",
            KnownProvider::GoogleVertex => "google-vertex",
            KnownProvider::OpenAI => "openai",
            KnownProvider::AzureOpenAiResponses => "azure-openai-responses",
            KnownProvider::OpenAICodex => "openai-codex",
            KnownProvider::GitHubCopilot => "github-copilot",
            KnownProvider::Xai => "xai",
            KnownProvider::Groq => "groq",
            KnownProvider::Cerebras => "cerebras",
            KnownProvider::Openrouter => "openrouter",
            KnownProvider::VercelAiGateway => "vercel-ai-gateway",
            KnownProvider::Zai => "zai",
            KnownProvider::Mistral => "mistral",
            KnownProvider::Minimax => "minimax",
            KnownProvider::MinimaxCn => "minimax-cn",
            KnownProvider::Huggingface => "huggingface",
            KnownProvider::Opencode => "opencode",
            KnownProvider::KimiCoding => "kimi-coding",
        }
    }

    /// Parse from registry key string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "amazon-bedrock" => Some(KnownProvider::AmazonBedrock),
            "anthropic" => Some(KnownProvider::Anthropic),
            "google" => Some(KnownProvider::Google),
            "google-gemini-cli" => Some(KnownProvider::GoogleGeminiCli),
            "google-antigravity" => Some(KnownProvider::GoogleAntigravity),
            "google-vertex" => Some(KnownProvider::GoogleVertex),
            "openai" => Some(KnownProvider::OpenAI),
            "azure-openai-responses" => Some(KnownProvider::AzureOpenAiResponses),
            "openai-codex" => Some(KnownProvider::OpenAICodex),
            "github-copilot" => Some(KnownProvider::GitHubCopilot),
            "xai" => Some(KnownProvider::Xai),
            "groq" => Some(KnownProvider::Groq),
            "cerebras" => Some(KnownProvider::Cerebras),
            "openrouter" => Some(KnownProvider::Openrouter),
            "vercel-ai-gateway" => Some(KnownProvider::VercelAiGateway),
            "zai" => Some(KnownProvider::Zai),
            "mistral" => Some(KnownProvider::Mistral),
            "minimax" => Some(KnownProvider::Minimax),
            "minimax-cn" => Some(KnownProvider::MinimaxCn),
            "huggingface" => Some(KnownProvider::Huggingface),
            "opencode" => Some(KnownProvider::Opencode),
            "kimi-coding" => Some(KnownProvider::KimiCoding),
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
}

/// Compatibility overrides for OpenAI Responses API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAIResponsesCompat {
    // Add fields as needed when defined in TypeScript
}

/// API-specific compatibility overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Compat {
    #[serde(rename = "openai-completions")]
    OpenAICompletions(OpenAICompletionsCompat),
    #[serde(rename = "openai-responses")]
    OpenAIResponses(OpenAIResponsesCompat),
}

/// Token usage and cost for a completion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    #[serde(rename = "cacheRead")]
    pub cache_read: u64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
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
    pub id: String,
    pub name: String,
    pub api: KnownApi,
    pub provider: KnownProvider,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub reasoning: bool,
    pub input: Vec<InputType>,
    pub cost: Cost,
    #[serde(rename = "contextWindow")]
    pub context_window: u64,
    #[serde(rename = "maxTokens")]
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Compatibility overrides for OpenAI-compatible APIs. If not set, auto-detected from base_url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}
