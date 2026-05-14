//! Google Generative AI provider.
//!
//! Implements streaming chat completions for the Google Generative AI (Gemini) REST API.
//! Supports thinking/reasoning, tool use, and thought signatures. The wire-format
//! handling (request body, SSE parsing, message conversion) lives in
//! `google_shared` and is reused by `google_vertex`.

use crate::api_registry::AssistantMessageEventStream;
use crate::env_api_keys;
use crate::providers::google_shared::{
    self, GoogleThinkingLevel as SharedGoogleThinkingLevel, SharedGoogleOptions,
};
use crate::types::{
    Api, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

/// Thinking level strings for Gemini 3 models.
#[derive(Debug, Clone, Copy)]
pub enum GoogleThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GoogleThinkingLevel {
    fn to_shared(self) -> SharedGoogleThinkingLevel {
        match self {
            GoogleThinkingLevel::Minimal => SharedGoogleThinkingLevel::Minimal,
            GoogleThinkingLevel::Low => SharedGoogleThinkingLevel::Low,
            GoogleThinkingLevel::Medium => SharedGoogleThinkingLevel::Medium,
            GoogleThinkingLevel::High => SharedGoogleThinkingLevel::High,
        }
    }
}

fn shared_to_public(level: SharedGoogleThinkingLevel) -> GoogleThinkingLevel {
    match level {
        SharedGoogleThinkingLevel::Minimal => GoogleThinkingLevel::Minimal,
        SharedGoogleThinkingLevel::Low => GoogleThinkingLevel::Low,
        SharedGoogleThinkingLevel::Medium => GoogleThinkingLevel::Medium,
        SharedGoogleThinkingLevel::High => GoogleThinkingLevel::High,
    }
}

/// Google-specific stream options.
#[derive(Debug, Clone, Default)]
pub struct GoogleOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: Option<i32>,
    pub thinking_level: Option<GoogleThinkingLevel>,
}

impl GoogleOptions {
    fn into_shared(self) -> SharedGoogleOptions {
        SharedGoogleOptions {
            base: self.base,
            tool_choice: self.tool_choice,
            thinking_enabled: self.thinking_enabled,
            thinking_budget_tokens: self.thinking_budget_tokens,
            thinking_level: self.thinking_level.map(GoogleThinkingLevel::to_shared),
        }
    }
}

// =============================================================================
// Provider
// =============================================================================

/// Provider implementation for Google Generative AI.
#[derive(Debug, Clone)]
pub struct GoogleGenerativeAiProvider {
    client: reqwest::Client,
}

impl Default for GoogleGenerativeAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleGenerativeAiProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Construct a provider using the supplied HTTP client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl crate::api_registry::ApiProvider for GoogleGenerativeAiProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let google_options = GoogleOptions {
            base: options.unwrap_or_default(),
            thinking_enabled: false,
            ..Default::default()
        };
        Box::pin(stream_google(
            self.client.clone(),
            model,
            context,
            google_options,
        ))
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let google_options = build_google_options(&model, options.as_ref());
        Box::pin(stream_google(
            self.client.clone(),
            model,
            context,
            google_options,
        ))
    }
}

fn build_google_options(model: &Model, options: Option<&SimpleStreamOptions>) -> GoogleOptions {
    let (base, reasoning) = match options {
        Some(opts) => {
            let api_key = env_api_keys::get_env_api_key(&model.provider);
            let base = opts.build_base_options(model, api_key);
            (base, opts.clamp_reasoning())
        }
        None => (StreamOptions::default(), None),
    };

    let mut google_opts = GoogleOptions {
        base,
        ..Default::default()
    };

    match reasoning {
        None => {
            google_opts.thinking_enabled = false;
        }
        Some(effort) => {
            google_opts.thinking_enabled = true;

            if google_shared::is_gemini3_pro_model(&model.id)
                || google_shared::is_gemini3_flash_model(&model.id)
                || google_shared::is_gemma4_model(&model.id)
            {
                // Gemini 3 and Gemma 4 both expose the
                // `thinkingLevel` knob; the underlying mapping picks
                // the right per-family bucket. Gemma 4 collapses to
                // MINIMAL / HIGH, Gemini 3 Pro uses LOW / HIGH,
                // Gemini 3 Flash exposes all four levels.
                google_opts.thinking_level = Some(shared_to_public(
                    google_shared::get_gemini3_thinking_level(effort, &model.id),
                ));
            } else {
                google_opts.thinking_budget_tokens = Some(google_shared::get_google_budget(
                    &model.id,
                    effort,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                ));
            }
        }
    }

    google_opts
}

// =============================================================================
// Streaming
// =============================================================================

fn stream_google(
    client: reqwest::Client,
    model: Model,
    context: Context,
    options: GoogleOptions,
) -> impl futures::Stream<Item = AssistantMessageEvent> + Send + 'static {
    async_stream::stream! {
        // Emit `Start` unconditionally so consumers always see
        // `Start -> ... -> (Done | Error)`, including on early failure paths
        // (auth, network) where SSE never opens. `parse_sse_stream` does NOT
        // emit its own `Start` to avoid duplicates.
        let initial = crate::types::AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::GoogleGenerativeAi,
            provider: model.provider,
            model: model.id.clone(),
            usage: crate::types::Usage::default(),
            stop_reason: crate::types::StopReason::Stop,
            error_message: None,
            timestamp: google_shared::current_timestamp_ms(),
            response_model: None,
            response_id: None,
            diagnostics: None,
        };
        yield AssistantMessageEvent::Start { partial: initial.clone() };

        let result = stream_google_inner(client, model, context, options).await;
        match result {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(e) => {
                let mut error_msg = initial;
                error_msg.stop_reason = crate::types::StopReason::Error;
                error_msg.error_message = Some(e);
                yield AssistantMessageEvent::Error {
                    reason: crate::types::StopReason::Error,
                    error: error_msg,
                };
            }
        }
    }
}

async fn stream_google_inner(
    client: reqwest::Client,
    model: Model,
    context: Context,
    options: GoogleOptions,
) -> Result<Vec<AssistantMessageEvent>, String> {
    let api_key = options
        .base
        .api_key
        .clone()
        .or_else(|| env_api_keys::get_env_api_key(&model.provider))
        .ok_or_else(|| format!("No API key found for provider: {}", model.provider.as_str()))?;

    let base_url = if model.base_url.is_empty() {
        "https://generativelanguage.googleapis.com".to_string()
    } else {
        model.base_url.trim_end_matches('/').to_string()
    };

    let url = format!(
        "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
        base_url, model.id, api_key
    );

    let shared_options = options.into_shared();
    let body = google_shared::build_request_body(&model, &context, &shared_options)?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    if let Some(model_headers) = &model.headers {
        for (key, value) in model_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    if let Some(custom_headers) = &shared_options.base.headers {
        for (key, value) in custom_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }
    }

    let response = client
        .post(&url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Failed to read error body".to_string());
        return Err(format!("Google API error ({}): {}", status, body));
    }

    google_shared::parse_sse_stream(response, &model, Api::GoogleGenerativeAi).await
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::google_shared as shared;
    use crate::types::{
        AssistantContentBlock, AssistantMessage, Cost, InputType, Message, Provider, StopReason,
        TextContent, ThinkingContent, ThinkingLevel, Tool, ToolResultContent, Usage, UserMessage,
    };

    fn test_model() -> Model {
        Model {
            id: "gemini-2.5-flash".into(),
            name: "Gemini 2.5 Flash".into(),
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            base_url: String::new(),
            reasoning: true,
            input: vec![InputType::Text, InputType::Image],
            cost: Cost {
                input: 0.15,
                output: 0.6,
                cache_read: 0.0375,
                cache_write: 0.0,
            },
            context_window: 1_048_576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn test_convert_messages_user_text() {
        let model = test_model();
        let messages = vec![Message::User(UserMessage::new_text("Hello"))];
        let contents = shared::convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn test_convert_messages_assistant() {
        let model = test_model();
        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new("Hi"))],
            api: Api::GoogleGenerativeAi,
            provider: Provider::Google,
            model: "gemini-2.5-flash".into(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];
        let contents = shared::convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["text"], "Hi");
    }

    #[test]
    fn test_convert_messages_tool_result_merges() {
        let model = test_model();
        let messages = vec![
            Message::ToolResult(crate::types::ToolResultMessage::new(
                "tc1",
                "read",
                vec![ToolResultContent::Text(TextContent::new("file contents"))],
            )),
            Message::ToolResult(crate::types::ToolResultMessage::new(
                "tc2",
                "ls",
                vec![ToolResultContent::Text(TextContent::new("dir listing"))],
            )),
        ];
        let contents = shared::convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].get("functionResponse").is_some());
        assert!(parts[1].get("functionResponse").is_some());
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![Tool::new(
            "read",
            "Read a file",
            serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];
        let result = shared::convert_tools(&tools);
        let decls = result["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "read");
    }

    #[test]
    fn test_map_stop_reason() {
        assert_eq!(shared::map_stop_reason("STOP"), StopReason::Stop);
        assert_eq!(shared::map_stop_reason("MAX_TOKENS"), StopReason::Length);
        assert_eq!(shared::map_stop_reason("SAFETY"), StopReason::Error);
    }

    #[test]
    fn test_is_valid_thought_signature() {
        assert!(shared::is_valid_thought_signature("YWJj"));
        assert!(!shared::is_valid_thought_signature(""));
        assert!(!shared::is_valid_thought_signature("abc"));
    }

    #[test]
    fn test_requires_tool_call_id() {
        assert!(shared::requires_tool_call_id("claude-3-haiku"));
        assert!(shared::requires_tool_call_id("gpt-oss-4o"));
        assert!(!shared::requires_tool_call_id("gemini-2.5-flash"));
    }

    #[test]
    fn test_is_gemini3_models() {
        assert!(shared::is_gemini3_pro_model("gemini-3-pro"));
        assert!(shared::is_gemini3_pro_model("gemini-3.1-pro-latest"));
        assert!(!shared::is_gemini3_pro_model("gemini-2.5-pro"));
        assert!(shared::is_gemini3_flash_model("gemini-3-flash"));
        assert!(!shared::is_gemini3_flash_model("gemini-2.5-flash"));
    }

    #[test]
    fn test_get_google_budget() {
        assert_eq!(
            shared::get_google_budget("gemini-2.5-pro", ThinkingLevel::Low, None),
            2048
        );
        assert_eq!(
            shared::get_google_budget("gemini-2.5-flash", ThinkingLevel::High, None),
            24576
        );
        assert_eq!(
            shared::get_google_budget("gemini-2.5-pro", ThinkingLevel::Minimal, None),
            128
        );

        let budgets = crate::types::ThinkingBudgets {
            low: Some(4096),
            ..Default::default()
        };
        assert_eq!(
            shared::get_google_budget("gemini-2.5-pro", ThinkingLevel::Low, Some(&budgets)),
            4096
        );
    }

    #[test]
    fn test_get_gemini3_thinking_level() {
        let level = shared::get_gemini3_thinking_level(ThinkingLevel::Low, "gemini-3-pro");
        assert!(matches!(level, shared::GoogleThinkingLevel::Low));
        let level = shared::get_gemini3_thinking_level(ThinkingLevel::High, "gemini-3-pro");
        assert!(matches!(level, shared::GoogleThinkingLevel::High));

        let level = shared::get_gemini3_thinking_level(ThinkingLevel::Minimal, "gemini-3-flash");
        assert!(matches!(level, shared::GoogleThinkingLevel::Minimal));
        let level = shared::get_gemini3_thinking_level(ThinkingLevel::Medium, "gemini-3-flash");
        assert!(matches!(level, shared::GoogleThinkingLevel::Medium));
    }

    #[test]
    fn test_disabled_thinking_config() {
        let config = shared::get_disabled_thinking_config("gemini-3-pro");
        assert_eq!(config["thinkingLevel"], "LOW");

        let config = shared::get_disabled_thinking_config("gemini-3-flash");
        assert_eq!(config["thinkingLevel"], "MINIMAL");

        let config = shared::get_disabled_thinking_config("gemini-2.5-flash");
        assert_eq!(config["thinkingBudget"], 0);
    }

    #[test]
    fn test_build_request_body_basic() {
        let model = test_model();
        let context = Context {
            system_prompt: Some("You are helpful.".into()),
            messages: vec![Message::User(UserMessage::new_text("Hi"))],
            tools: None,
        };
        let options = SharedGoogleOptions::default();
        let body = shared::build_request_body(&model, &context, &options).unwrap();

        assert!(body.get("contents").is_some());
        assert!(body.get("systemInstruction").is_some());
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
    }

    #[test]
    fn test_build_request_body_with_thinking() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("Think about this"))],
            tools: None,
        };
        let options = SharedGoogleOptions {
            thinking_enabled: true,
            thinking_budget_tokens: Some(8192),
            ..Default::default()
        };
        let body = shared::build_request_body(&model, &context, &options).unwrap();

        let gen_config = &body["generationConfig"];
        assert_eq!(gen_config["thinkingConfig"]["includeThoughts"], true);
        assert_eq!(gen_config["thinkingConfig"]["thinkingBudget"], 8192);
    }

    #[test]
    fn test_thinking_block_cross_provider() {
        let model = test_model();
        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Thinking(ThinkingContent::new(
                "reasoning here",
            ))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude-sonnet-4-20250514".into(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];
        let contents = shared::convert_messages(&messages, &model);
        assert_eq!(contents.len(), 1);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "reasoning here");
        assert!(parts[0].get("thought").is_none());
    }
}
