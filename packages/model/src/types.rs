//! Model interface types for the unified model system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// API identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
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
        }
    }

    /// Parse from registry key string.
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
}

/// Compatibility overrides for OpenAI Responses API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAIResponsesCompat {
    // Reserved for future use
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
    pub api: Api,
    pub provider: Provider,
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
    pub headers: Option<HashMap<String, String>>,
    /// Compatibility overrides for OpenAI-compatible APIs. If not set, auto-detected from base_url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}

/// OpenRouter provider routing preferences.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
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
    Minimal,
    Low,
    Medium,
    High,
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

/// Base options all providers share.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            content_type: "thinking".to_string(),
            thinking: thinking.into(),
            thinking_signature: None,
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
    Stop,
    Length,
    ToolUse,
    Error,
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
    pub role: String,
    pub content: UserContent,
    pub timestamp: u64,
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
#[serde(tag = "type", rename_all = "snake_case")]
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
    ToolCallStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ToolCallDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
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
        ];
        for provider in providers {
            let key = provider.as_str();
            assert_eq!(Provider::from_str(key), Some(provider));
        }
    }
}
