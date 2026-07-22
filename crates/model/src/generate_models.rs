//! Generate models list from OpenRouter, Vercel AI Gateway, and models.dev.
//! Run: cargo run --bin generate_models

use model::types::*;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Constants
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

const AI_GATEWAY_MODELS_URL: &str = "https://ai-gateway.vercel.sh/v1";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh";
const OPENROUTER_API: &str = "https://openrouter.ai/api/v1";
const MODELS_DEV_API: &str = "https://models.dev/api.json";

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// API Response Type Definitions
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

// --- OpenRouter API response types ---
#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    name: String,
    #[serde(default)]
    supported_parameters: Vec<String>,
    architecture: Option<OpenRouterArchitecture>,
    pricing: Option<OpenRouterPricing>,
    context_length: Option<u64>,
    top_provider: Option<OpenRouterTopProvider>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    prompt: Option<String>,
    completion: Option<String>,
    input_cache_read: Option<String>,
    input_cache_write: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTopProvider {
    max_completion_tokens: Option<u64>,
}

// --- Vercel AI Gateway API response types ---

#[derive(Debug, Deserialize)]
struct AiGatewayResponse {
    data: Option<Vec<AiGatewayModel>>,
}

#[derive(Debug, Deserialize)]
struct AiGatewayModel {
    id: String,
    name: Option<String>,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
    tags: Option<Vec<String>>,
    pricing: Option<AiGatewayPricing>,
}

#[derive(Debug, Deserialize)]
struct AiGatewayPricing {
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    input_cache_read: Option<serde_json::Value>,
    input_cache_write: Option<serde_json::Value>,
}

// --- models.dev API response types ---
#[derive(Debug, Deserialize)]
struct ModelsDevModel {
    name: Option<String>,
    tool_call: Option<bool>,
    reasoning: Option<bool>,
    limit: Option<ModelsDevLimit>,
    cost: Option<ModelsDevCost>,
    modalities: Option<ModelsDevModalities>,
    provider: Option<ModelsDevProvider>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevLimit {
    context: Option<u64>,
    output: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevModalities {
    input: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevProvider {
    npm: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevProviderData {
    models: HashMap<String, ModelsDevModel>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevData {
    #[serde(rename = "amazon-bedrock")]
    amazon_bedrock: Option<ModelsDevProviderData>,
    anthropic: Option<ModelsDevProviderData>,
    google: Option<ModelsDevProviderData>,
    openai: Option<ModelsDevProviderData>,
    groq: Option<ModelsDevProviderData>,
    cerebras: Option<ModelsDevProviderData>,
    xai: Option<ModelsDevProviderData>,
    zai: Option<ModelsDevProviderData>,
    /// models.dev renamed the zAi catalog to `zai-coding-plan` so the
    /// generator reads from whichever key the snapshot carries.
    /// Either source feeds the same hand-ai `Provider::Zai` entries
    /// downstream — only the JSON key differs.
    #[serde(rename = "zai-coding-plan")]
    zai_coding_plan: Option<ModelsDevProviderData>,
    mistral: Option<ModelsDevProviderData>,
    huggingface: Option<ModelsDevProviderData>,
    opencode: Option<ModelsDevProviderData>,
    #[serde(rename = "github-copilot")]
    github_copilot: Option<ModelsDevProviderData>,
    minimax: Option<ModelsDevProviderData>,
    #[serde(rename = "minimax-cn")]
    minimax_cn: Option<ModelsDevProviderData>,
    #[serde(rename = "kimi-for-coding")]
    kimi_for_coding: Option<ModelsDevProviderData>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helper Functions
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn to_number(v: Option<&serde_json::Value>) -> f64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn cost(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Cost {
    Cost {
        input,
        output,
        cache_read,
        cache_write,
    }
}

fn input_text() -> Vec<InputType> {
    vec![InputType::Text]
}

fn input_text_image() -> Vec<InputType> {
    vec![InputType::Text, InputType::Image]
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// API Data Fetching Functions
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

async fn fetch_openrouter_models(client: &reqwest::Client) -> Vec<Model> {
    println!("Fetching models from OpenRouter API...");
    let resp = match client
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to fetch OpenRouter models: {e}");
            return vec![];
        }
    };
    let data: OpenRouterResponse = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to parse OpenRouter response: {e}");
            return vec![];
        }
    };
    let mut models = Vec::new();
    for m in data.data {
        if !m.supported_parameters.iter().any(|p| p == "tools") {
            continue;
        }
        let input = if m
            .architecture
            .as_ref()
            .and_then(|a| a.input_modalities.as_ref())
            .map(|mods| mods.iter().any(|m| m == "image"))
            == Some(true)
        {
            input_text_image()
        } else {
            input_text()
        };
        let pricing = m.pricing.as_ref();
        let input_cost = pricing
            .and_then(|p| p.prompt.as_ref())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            * 1_000_000.0;
        let output_cost = pricing
            .and_then(|p| p.completion.as_ref())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            * 1_000_000.0;
        let cache_read_cost = pricing
            .and_then(|p| p.input_cache_read.as_ref())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            * 1_000_000.0;
        let cache_write_cost = pricing
            .and_then(|p| p.input_cache_write.as_ref())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            * 1_000_000.0;
        let compat = openrouter_compat(&m.id);
        models.push(Model {
            id: m.id.clone(),
            name: m.name,
            api: Api::OpenAICompletions,
            provider: Provider::Openrouter,
            base_url: OPENROUTER_API.to_string(),
            reasoning: m.supported_parameters.iter().any(|p| p == "reasoning"),
            input,
            cost: cost(input_cost, output_cost, cache_read_cost, cache_write_cost),
            context_window: m.context_length.unwrap_or(4096),
            max_tokens: m
                .top_provider
                .as_ref()
                .and_then(|t| t.max_completion_tokens)
                .unwrap_or(4096),
            headers: None,
            compat,
            thinking_level_map: None,
        });
    }
    println!(
        "Fetched {} tool-capable models from OpenRouter",
        models.len()
    );
    models
}

async fn fetch_ai_gateway_models(client: &reqwest::Client) -> Vec<Model> {
    println!("Fetching models from Vercel AI Gateway API...");
    let url = format!("{AI_GATEWAY_MODELS_URL}/models");
    let Ok(resp) = client.get(&url).send().await else {
        eprintln!("Failed to fetch AI Gateway models");
        return vec![];
    };
    let Ok(data): Result<AiGatewayResponse, _> = resp.json().await else {
        eprintln!("Failed to parse AI Gateway response");
        return vec![];
    };
    let items = data.data.unwrap_or_default();
    let mut models = Vec::new();
    for m in items {
        let tags = m.tags.as_deref().unwrap_or(&[]);
        if !tags.iter().any(|t| t.as_str() == "tool-use") {
            continue;
        }
        let input = if tags.iter().any(|t| t.as_str() == "vision") {
            input_text_image()
        } else {
            input_text()
        };
        let pr = m.pricing.as_ref();
        let input_cost = to_number(pr.and_then(|p| p.input.as_ref())) * 1_000_000.0;
        let output_cost = to_number(pr.and_then(|p| p.output.as_ref())) * 1_000_000.0;
        let cr = to_number(pr.and_then(|p| p.input_cache_read.as_ref())) * 1_000_000.0;
        let cw = to_number(pr.and_then(|p| p.input_cache_write.as_ref())) * 1_000_000.0;
        models.push(Model {
            id: m.id.clone(),
            name: m.name.unwrap_or(m.id),
            api: Api::AnthropicMessages,
            provider: Provider::VercelAiGateway,
            base_url: AI_GATEWAY_BASE_URL.to_string(),
            reasoning: tags.iter().any(|t| t.as_str() == "reasoning"),
            input,
            cost: cost(input_cost, output_cost, cr, cw),
            context_window: m.context_window.unwrap_or(4096),
            max_tokens: m.max_tokens.unwrap_or(4096),
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }
    println!(
        "Fetched {} tool-capable models from Vercel AI Gateway",
        models.len()
    );
    models
}

/// Return true for known versioned aliases of the canonical
/// `kimi-for-coding` model. models.dev exposes new aliases over time
/// (`k2p5`, `k2p6`, ...) that all point at the same model family; we
/// fold them onto the canonical id rather than shipping duplicates.
fn is_kimi_alias(model_id: &str) -> bool {
    matches!(model_id, "k2p5" | "k2p6")
}

/// Return true for GitHub Copilot Claude 4.x models, which route
/// through the Anthropic Messages API rather than the Copilot OpenAI
/// surface. Matches the upstream regex `^claude-(haiku|sonnet|opus)-4(?:[.\-]|$)`
/// — so `claude-haiku-4`, `claude-sonnet-4.5`, `claude-opus-4-7`, ...
/// all hit the Anthropic branch. The "4" must be followed by `.`,
/// `-`, or end-of-string so we don't false-positive on a future
/// `claude-haiku-40` family.
fn is_copilot_claude_4_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    let stripped = lower
        .strip_prefix("claude-haiku-4")
        .or_else(|| lower.strip_prefix("claude-sonnet-4"))
        .or_else(|| lower.strip_prefix("claude-opus-4"));
    match stripped {
        Some(rest) => rest.is_empty() || rest.starts_with('.') || rest.starts_with('-'),
        None => false,
    }
}

/// Return true for the legacy z.ai coding-plan models that do not
/// accept the OpenAI-compatible `tool_stream: true` flag. The four
/// GLM 4.5 ids reject the field with HTTP 400; every newer z.ai id
/// (glm-4.6 family, future vision siblings, ...) supports streaming
/// and falls back to the `is_zai` default.
fn is_zai_tool_stream_unsupported(model_id: &str) -> bool {
    matches!(
        model_id,
        "glm-4.5" | "glm-4.5-air" | "glm-4.5-flash" | "glm-4.5v"
    )
}

/// Return true for GitHub Copilot Anthropic-branch Claude models whose
/// proxied Messages endpoint rejects the per-tool `eager_input_streaming`
/// flag. The default for `AnthropicMessagesCompat` is to enable eager
/// streaming; these specific Copilot snapshots opt back out so the
/// transformer falls through to the legacy fine-grained tool streaming
/// beta header instead.
fn is_copilot_eager_streaming_unsupported(model_id: &str) -> bool {
    matches!(
        model_id,
        "claude-haiku-4.5" | "claude-sonnet-4" | "claude-sonnet-4.5"
    )
}

/// Static headers attached to every Kimi-for-coding request. The
/// upstream Kimi API gates traffic on a recognised `User-Agent`
/// string — without it the SDK is rejected as an unknown client.
fn kimi_static_headers() -> std::collections::HashMap<String, String> {
    let mut h = std::collections::HashMap::new();
    h.insert("User-Agent".to_string(), "KimiCLI/1.5".to_string());
    h
}

/// OpenRouter routes DeepSeek V3/V4 reasoning models through OpenAI-compatible
/// completions but expects DeepSeek's native thinking-format conventions on
/// the wire (assistant turns must echo `reasoning_content`, reasoning blocks
/// use the deepseek tag layout). Return the compat block to attach to those
/// model entries during generation; return `None` for unrelated ids so the
/// helper stays a pure mapping.
fn openrouter_compat(model_id: &str) -> Option<Compat> {
    if !model_id.starts_with("deepseek/deepseek-v3")
        && !model_id.starts_with("deepseek/deepseek-v4")
    {
        return None;
    }
    Some(Compat::OpenAICompletions(Box::new(
        OpenAICompletionsCompat {
            thinking_format: Some("deepseek".to_string()),
            requires_reasoning_content_on_assistant_messages: Some(true),
            ..Default::default()
        },
    )))
}

fn provider_has_tool_call(m: &ModelsDevModel) -> bool {
    m.tool_call == Some(true)
}

fn has_image(m: &ModelsDevModel) -> bool {
    m.modalities
        .as_ref()
        .and_then(|mods| mods.input.as_ref())
        .map(|v| v.contains(&"image".to_string()))
        == Some(true)
}

fn limit_context(m: &ModelsDevModel) -> u64 {
    m.limit.as_ref().and_then(|l| l.context).unwrap_or(4096)
}

fn limit_output(m: &ModelsDevModel) -> u64 {
    m.limit.as_ref().and_then(|l| l.output).unwrap_or(4096)
}

fn cost_from_model(m: &ModelsDevModel) -> Cost {
    let c = m.cost.as_ref();
    cost(
        c.and_then(|x| x.input).unwrap_or(0.0),
        c.and_then(|x| x.output).unwrap_or(0.0),
        c.and_then(|x| x.cache_read).unwrap_or(0.0),
        c.and_then(|x| x.cache_write).unwrap_or(0.0),
    )
}

async fn load_models_dev_data(client: &reqwest::Client) -> Vec<Model> {
    println!("Fetching models from models.dev API...");
    let resp = match client.get(MODELS_DEV_API).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to fetch models.dev: {e}");
            return vec![];
        }
    };
    let data: ModelsDevData = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to parse models.dev response: {e}");
            return vec![];
        }
    };
    let mut models = Vec::new();

    // Amazon Bedrock
    if let Some(ref prov) = data.amazon_bedrock {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            let mut id = model_id.clone();
            if id.starts_with("ai21.jamba") {
                continue;
            }
            if id.starts_with("amazon.titan-text-express")
                || id.starts_with("mistral.mistral-7b-instruct-v0")
            {
                continue;
            }
            if id.starts_with("anthropic.claude-haiku-4-5")
                || id.starts_with("anthropic.claude-sonnet-4")
                || id.starts_with("anthropic.claude-opus-4-5")
                || id.starts_with("amazon.nova-2-lite")
                || id.starts_with("cohere.embed-v4")
                || id.starts_with("twelvelabs.pegasus-1-2")
            {
                id = format!("global.{id}");
            }
            if id.starts_with("amazon.nova-lite")
                || id.starts_with("amazon.nova-micro")
                || id.starts_with("amazon.nova-premier")
                || id.starts_with("amazon.nova-pro")
                || id.starts_with("anthropic.claude-3-7-sonnet")
                || id.starts_with("anthropic.claude-opus-4-1")
                || id.starts_with("anthropic.claude-opus-4-20250514")
                || id.starts_with("deepseek.r1")
                || id.starts_with("meta.llama3-2")
                || id.starts_with("meta.llama3-3")
                || id.starts_with("meta.llama4")
            {
                id = format!("us.{id}");
            }
            let bedrock_model = Model {
                id: id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::BedrockConverseStream,
                provider: Provider::AmazonBedrock,
                base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            };
            models.push(bedrock_model.clone());
            if model_id.starts_with("anthropic.claude-haiku-4-5")
                || model_id.starts_with("anthropic.claude-sonnet-4-5")
                || model_id.starts_with("anthropic.claude-opus-4-5")
            {
                models.push(Model {
                    id: format!("eu.{model_id}"),
                    name: format!("{} (EU)", m.name.as_deref().unwrap_or(model_id)),
                    ..bedrock_model.clone()
                });
            }
        }
    }

    // Anthropic
    if let Some(ref prov) = data.anthropic {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::AnthropicMessages,
                provider: Provider::Anthropic,
                base_url: "https://api.anthropic.com".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // Google
    if let Some(ref prov) = data.google {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::GoogleGenerativeAi,
                provider: Provider::Google,
                base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // OpenAI
    if let Some(ref prov) = data.openai {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAIResponses,
                provider: Provider::OpenAI,
                base_url: "https://api.openai.com/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // Groq
    if let Some(ref prov) = data.groq {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Groq,
                base_url: "https://api.groq.com/openai/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // Cerebras
    if let Some(ref prov) = data.cerebras {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Cerebras,
                base_url: "https://api.cerebras.ai/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // xAi
    if let Some(ref prov) = data.xai {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Xai,
                base_url: "https://api.x.ai/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // zAi: accept either the legacy `zai` key or the newer
    // `zai-coding-plan` key from the models.dev snapshot. Whichever
    // is present feeds the same hand-ai `Provider::Zai` output.
    let zai_source = data.zai_coding_plan.as_ref().or(data.zai.as_ref());
    if let Some(prov) = zai_source {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            // The legacy GLM 4.5 family on z.ai's coding-plan endpoint
            // does not accept the `tool_stream: true` flag — it returns
            // a 400 on those models. Newer ids (glm-4.6+, vision
            // siblings, ...) support tool streaming. Explicitly pin the
            // legacy four to `zai_tool_stream = false` so the per-zai
            // default `true` in `detect_compat` does not flip them on.
            let zai_tool_stream = if is_zai_tool_stream_unsupported(model_id) {
                Some(false)
            } else {
                None
            };
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Zai,
                base_url: "https://api.z.ai/api/coding/paas/v4".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: Some(Compat::OpenAICompletions(Box::new(
                    OpenAICompletionsCompat {
                        supports_store: None,
                        supports_developer_role: Some(false),
                        supports_reasoning_effort: None,
                        thinking_format: Some("zai".to_string()),
                        zai_tool_stream,
                        ..Default::default()
                    },
                ))),
                thinking_level_map: None,
            });
        }
    }

    // Mistral
    if let Some(ref prov) = data.mistral {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Mistral,
                base_url: "https://api.mistral.ai/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // Huggingface
    if let Some(ref prov) = data.huggingface {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api: Api::OpenAICompletions,
                provider: Provider::Huggingface,
                base_url: "https://router.huggingface.co/v1".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: Some(Compat::OpenAICompletions(Box::new(
                    OpenAICompletionsCompat {
                        supports_store: None,
                        supports_developer_role: Some(false),
                        supports_reasoning_effort: None,
                        thinking_format: None,
                        ..Default::default()
                    },
                ))),
                thinking_level_map: None,
            });
        }
    }

    // OpenCode
    if let Some(ref prov) = data.opencode {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            if m.status.as_deref() == Some("deprecated") {
                continue;
            }
            let (api, base_url) = match m.provider.as_ref().and_then(|p| p.npm.as_deref()) {
                Some("@ai-sdk/openai") => (Api::OpenAIResponses, "https://opencode.ai/zen/v1"),
                Some("@ai-sdk/anthropic") => (Api::AnthropicMessages, "https://opencode.ai/zen"),
                Some("@ai-sdk/google") => (Api::GoogleGenerativeAi, "https://opencode.ai/zen/v1"),
                _ => (Api::OpenAICompletions, "https://opencode.ai/zen/v1"),
            };
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api,
                provider: Provider::Opencode,
                base_url: base_url.to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: None,
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    // GitHub Copilot
    let copilot_headers: HashMap<String, String> = [
        ("User-Agent", "GitHubCopilotChat/0.35.0"),
        ("Editor-Version", "vscode/1.107.0"),
        ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
        ("Copilot-Integration-Id", "vscode-chat"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    if let Some(ref prov) = data.github_copilot {
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }
            if m.status.as_deref() == Some("deprecated") {
                continue;
            }
            // Claude 4.x models on GitHub Copilot route through the
            // Anthropic Messages API (haiku/sonnet/opus). gpt-5* and
            // oswe* use OpenAI Responses; everything else falls back
            // to OpenAI Completions with the copilot compat overrides.
            let is_copilot_claude_4 = is_copilot_claude_4_model(model_id);
            let needs_responses_api = !is_copilot_claude_4
                && (model_id.starts_with("gpt-5") || model_id.starts_with("oswe"));
            let api = if is_copilot_claude_4 {
                Api::AnthropicMessages
            } else if needs_responses_api {
                Api::OpenAIResponses
            } else {
                Api::OpenAICompletions
            };
            // OpenAI Completions is the only branch that needs the
            // copilot-specific compat overrides; the Responses branch
            // uses its provider defaults. The Anthropic Messages branch
            // attaches a compat block only for snapshots whose Copilot
            // proxy rejects the per-tool `eager_input_streaming` flag.
            let compat = if api == Api::OpenAICompletions {
                Some(Compat::OpenAICompletions(Box::new(
                    OpenAICompletionsCompat {
                        supports_store: Some(false),
                        supports_developer_role: Some(false),
                        supports_reasoning_effort: Some(false),
                        thinking_format: None,
                        ..Default::default()
                    },
                )))
            } else if api == Api::AnthropicMessages
                && is_copilot_eager_streaming_unsupported(model_id)
            {
                Some(Compat::AnthropicMessages(AnthropicMessagesCompat {
                    supports_eager_tool_input_streaming: Some(false),
                    ..Default::default()
                }))
            } else {
                None
            };
            models.push(Model {
                id: model_id.clone(),
                name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                api,
                provider: Provider::GitHubCopilot,
                base_url: "https://api.individual.githubcopilot.com".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: m.limit.as_ref().and_then(|l| l.context).unwrap_or(128000),
                max_tokens: m.limit.as_ref().and_then(|l| l.output).unwrap_or(8192),
                headers: Some(copilot_headers.clone()),
                compat,
                thinking_level_map: None,
            });
        }
    }

    // MiniMax
    let minimax_configs = [
        (
            "minimax",
            Provider::Minimax,
            "https://api.minimax.io/anthropic",
        ),
        (
            "minimax-cn",
            Provider::MinimaxCn,
            "https://api.minimaxi.com/anthropic",
        ),
    ];
    for (key, provider, base_url) in minimax_configs {
        let prov = match key {
            "minimax" => data.minimax.as_ref(),
            _ => data.minimax_cn.as_ref(),
        };
        if let Some(prov) = prov {
            for (model_id, m) in &prov.models {
                if !provider_has_tool_call(m) {
                    continue;
                }
                models.push(Model {
                    id: model_id.clone(),
                    name: m.name.clone().unwrap_or_else(|| model_id.clone()),
                    api: Api::AnthropicMessages,
                    provider,
                    base_url: base_url.to_string(),
                    reasoning: m.reasoning == Some(true),
                    input: if has_image(m) {
                        input_text_image()
                    } else {
                        input_text()
                    },
                    cost: cost_from_model(m),
                    context_window: limit_context(m),
                    max_tokens: limit_output(m),
                    headers: None,
                    compat: None,
                    thinking_level_map: None,
                });
            }
        }
    }

    // Kimi for coding
    if let Some(ref prov) = data.kimi_for_coding {
        let has_canonical = prov.models.contains_key("kimi-for-coding");
        for (model_id, m) in &prov.models {
            if !provider_has_tool_call(m) {
                continue;
            }

            // models.dev exposes versioned aliases (e.g. `k2p5`, `k2p6`) for
            // the canonical `kimi-for-coding` snapshot. Drop aliases when
            // the canonical id is also present so we don't ship duplicate
            // entries; otherwise normalize the alias to the canonical id
            // and human-readable name.
            let (normalized_id, normalized_name) = if is_kimi_alias(model_id) {
                if has_canonical {
                    continue;
                }
                ("kimi-for-coding".to_string(), "Kimi For Coding".to_string())
            } else {
                (
                    model_id.clone(),
                    m.name.clone().unwrap_or_else(|| model_id.clone()),
                )
            };

            models.push(Model {
                id: normalized_id,
                name: normalized_name,
                api: Api::AnthropicMessages,
                provider: Provider::KimiCoding,
                base_url: "https://api.kimi.com/coding".to_string(),
                reasoning: m.reasoning == Some(true),
                input: if has_image(m) {
                    input_text_image()
                } else {
                    input_text()
                },
                cost: cost_from_model(m),
                context_window: limit_context(m),
                max_tokens: limit_output(m),
                headers: Some(kimi_static_headers()),
                compat: None,
                thinking_level_map: None,
            });
        }
    }

    println!(
        "Loaded {} tool-capable models from models.dev",
        models.len()
    );
    models
}

fn provider_key(p: Provider) -> &'static str {
    match p {
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

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Static Model Definitions
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn static_codex_models() -> Vec<Model> {
    const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
    const CODEX_CONTEXT: u64 = 272000;
    const CODEX_MAX_TOKENS: u64 = 128000;
    let c = |i: f64, o: f64, cr: f64, cw: f64| cost(i, o, cr, cw);
    vec![
        Model {
            id: "gpt-5.1".to_string(),
            name: "GPT-5.1".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: CODEX_BASE_URL.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: c(1.25, 10.0, 0.125, 0.0),
            context_window: CODEX_CONTEXT,
            max_tokens: CODEX_MAX_TOKENS,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gpt-5.1-codex-max".to_string(),
            name: "GPT-5.1 Codex Max".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: CODEX_BASE_URL.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: c(1.25, 10.0, 0.125, 0.0),
            context_window: CODEX_CONTEXT,
            max_tokens: CODEX_MAX_TOKENS,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gpt-5.1-codex-mini".to_string(),
            name: "GPT-5.1 Codex Mini".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: CODEX_BASE_URL.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: c(0.25, 2.0, 0.025, 0.0),
            context_window: CODEX_CONTEXT,
            max_tokens: CODEX_MAX_TOKENS,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gpt-5.2".to_string(),
            name: "GPT-5.2".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: CODEX_BASE_URL.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: c(1.75, 14.0, 0.175, 0.0),
            context_window: CODEX_CONTEXT,
            max_tokens: CODEX_MAX_TOKENS,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gpt-5.2-codex".to_string(),
            name: "GPT-5.2 Codex".to_string(),
            api: Api::OpenAICodexResponses,
            provider: Provider::OpenAICodex,
            base_url: CODEX_BASE_URL.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: c(1.75, 14.0, 0.175, 0.0),
            context_window: CODEX_CONTEXT,
            max_tokens: CODEX_MAX_TOKENS,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
    ]
}

fn static_cloud_code_assist_models() -> Vec<Model> {
    // Google Cloud Code Assist models (Gemini CLI)
    // Uses production endpoint, standard Gemini models only
    const CLOUD_CODE_ASSIST: &str = "https://cloudcode-pa.googleapis.com";

    vec![
        Model {
            id: "gemini-2.5-pro".to_string(),
            name: "Gemini 2.5 Pro (Cloud Code Assist)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleGeminiCli,
            base_url: CLOUD_CODE_ASSIST.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.5-flash".to_string(),
            name: "Gemini 2.5 Flash (Cloud Code Assist)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleGeminiCli,
            base_url: CLOUD_CODE_ASSIST.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.0-flash".to_string(),
            name: "Gemini 2.0 Flash (Cloud Code Assist)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleGeminiCli,
            base_url: CLOUD_CODE_ASSIST.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 1048576,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-3-pro-preview".to_string(),
            name: "Gemini 3 Pro Preview (Cloud Code Assist)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleGeminiCli,
            base_url: CLOUD_CODE_ASSIST.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-3-flash-preview".to_string(),
            name: "Gemini 3 Flash Preview (Cloud Code Assist)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleGeminiCli,
            base_url: CLOUD_CODE_ASSIST.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
    ]
}

fn static_antigravity_models() -> Vec<Model> {
    // Antigravity models (Gemini 3, Claude, GPT-OSS via Google Cloud)
    // Uses sandbox endpoint and different OAuth credentials for access to additional models
    const ANTIGRAVITY: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";

    vec![
        Model {
            id: "gemini-3-pro-high".to_string(),
            name: "Gemini 3 Pro High (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(2.0, 12.0, 0.2, 2.375),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-3-pro-low".to_string(),
            name: "Gemini 3 Pro Low (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(2.0, 12.0, 0.2, 2.375),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-3-flash".to_string(),
            name: "Gemini 3 Flash (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.5, 3.0, 0.5, 0.0),
            context_window: 1048576,
            max_tokens: 65535,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "claude-sonnet-4-5".to_string(),
            name: "Claude Sonnet 4.5 (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(3.0, 15.0, 0.3, 3.75),
            context_window: 200000,
            max_tokens: 64000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "claude-sonnet-4-5-thinking".to_string(),
            name: "Claude Sonnet 4.5 Thinking (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(3.0, 15.0, 0.3, 3.75),
            context_window: 200000,
            max_tokens: 64000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "claude-opus-4-5-thinking".to_string(),
            name: "Claude Opus 4.5 Thinking (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(5.0, 25.0, 0.5, 6.25),
            context_window: 200000,
            max_tokens: 64000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gpt-oss-120b-medium".to_string(),
            name: "GPT-OSS 120B Medium (Antigravity)".to_string(),
            api: Api::GoogleGeminiCli,
            provider: Provider::GoogleAntigravity,
            base_url: ANTIGRAVITY.to_string(),
            reasoning: false,
            input: input_text(),
            cost: cost(0.09, 0.36, 0.0, 0.0),
            context_window: 131072,
            max_tokens: 32768,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
    ]
}

fn static_vertex_models() -> Vec<Model> {
    const VERTEX: &str = "https://{location}-aiplatform.googleapis.com";

    vec![
        Model {
            id: "gemini-3-pro-preview".to_string(),
            name: "Gemini 3 Pro Preview (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(2.0, 12.0, 0.2, 0.0),
            context_window: 1000000,
            max_tokens: 64000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-3-flash-preview".to_string(),
            name: "Gemini 3 Flash Preview (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.5, 3.0, 0.05, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.0-flash".to_string(),
            name: "Gemini 2.0 Flash (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(0.15, 0.6, 0.0375, 0.0),
            context_window: 1048576,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.0-flash-lite".to_string(),
            name: "Gemini 2.0 Flash Lite (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.075, 0.3, 0.01875, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.5-pro".to_string(),
            name: "Gemini 2.5 Pro (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(1.25, 10.0, 0.125, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.5-flash".to_string(),
            name: "Gemini 2.5 Flash (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.3, 2.5, 0.03, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.5-flash-lite-preview-09-2025".to_string(),
            name: "Gemini 2.5 Flash Lite Preview 09-25 (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.1, 0.4, 0.01, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-2.5-flash-lite".to_string(),
            name: "Gemini 2.5 Flash Lite (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.1, 0.4, 0.01, 0.0),
            context_window: 1048576,
            max_tokens: 65536,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-1.5-pro".to_string(),
            name: "Gemini 1.5 Pro (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(1.25, 5.0, 0.3125, 0.0),
            context_window: 1000000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-1.5-flash".to_string(),
            name: "Gemini 1.5 Flash (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(0.075, 0.3, 0.01875, 0.0),
            context_window: 1000000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "gemini-1.5-flash-8b".to_string(),
            name: "Gemini 1.5 Flash-8B (Vertex)".to_string(),
            api: Api::GoogleVertex,
            provider: Provider::GoogleVertex,
            base_url: VERTEX.to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(0.0375, 0.15, 0.01, 0.0),
            context_window: 1000000,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        },
    ]
}

fn static_deepseek_models() -> Vec<Model> {
    // Native DeepSeek API. Maintained as first-party entries because
    // they predate (and outlive) any single models.dev snapshot.
    // Single-turn use works out of the box; multi-turn replay requires
    // the `requiresReasoningContentOnAssistantMessages` compat path
    // (encoded here but not yet enforced on the request side — see
    // ResolvedCompat::requires_reasoning_content_on_assistant_messages).
    const BASE_URL: &str = "https://api.deepseek.com";

    let mut thinking_map = std::collections::HashMap::new();
    thinking_map.insert("minimal".to_string(), None);
    thinking_map.insert("low".to_string(), None);
    thinking_map.insert("medium".to_string(), None);
    thinking_map.insert("high".to_string(), Some("high".to_string()));
    // DeepSeek's native top effort is `max` — surface it as the `max`
    // level; `xhigh` stays unmapped and clamps up to it.
    thinking_map.insert("max".to_string(), Some("max".to_string()));

    let compat = Compat::OpenAICompletions(Box::new(OpenAICompletionsCompat {
        thinking_format: Some("deepseek".to_string()),
        requires_reasoning_content_on_assistant_messages: Some(true),
        ..Default::default()
    }));

    vec![
        Model {
            id: "deepseek-v4-flash".to_string(),
            name: "DeepSeek V4 Flash".to_string(),
            api: Api::OpenAICompletions,
            provider: Provider::Deepseek,
            base_url: BASE_URL.to_string(),
            reasoning: true,
            input: input_text(),
            cost: cost(0.14, 0.28, 0.0028, 0.0),
            context_window: 1_000_000,
            max_tokens: 384_000,
            headers: None,
            compat: Some(compat.clone()),
            thinking_level_map: Some(thinking_map.clone()),
        },
        Model {
            id: "deepseek-v4-pro".to_string(),
            name: "DeepSeek V4 Pro".to_string(),
            api: Api::OpenAICompletions,
            provider: Provider::Deepseek,
            base_url: BASE_URL.to_string(),
            reasoning: true,
            input: input_text(),
            cost: cost(0.435, 0.87, 0.003625, 0.0),
            context_window: 1_000_000,
            max_tokens: 384_000,
            headers: None,
            compat: Some(compat),
            thinking_level_map: Some(thinking_map),
        },
    ]
}

fn static_kimi_coding_models() -> Vec<Model> {
    // Kimi For Coding models (Moonshot AI's Anthropic-compatible coding API)
    // Static fallback in case models.dev doesn't have them yet
    const KIMI_CODING_BASE_URL: &str = "https://api.kimi.com/coding";

    vec![
        Model {
            id: "kimi-k2-thinking".to_string(),
            name: "Kimi K2 Thinking".to_string(),
            api: Api::AnthropicMessages,
            provider: Provider::KimiCoding,
            base_url: KIMI_CODING_BASE_URL.to_string(),
            reasoning: true,
            input: input_text(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 262144,
            max_tokens: 32768,
            headers: Some(kimi_static_headers()),
            compat: None,
            thinking_level_map: None,
        },
        Model {
            id: "k2p5".to_string(),
            name: "Kimi K2.5".to_string(),
            api: Api::AnthropicMessages,
            provider: Provider::KimiCoding,
            base_url: KIMI_CODING_BASE_URL.to_string(),
            reasoning: true,
            input: input_text(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 262144,
            max_tokens: 32768,
            headers: Some(kimi_static_headers()),
            compat: None,
            thinking_level_map: None,
        },
    ]
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Main Entry Point & File Generation
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn write_generated(
    all_models: &[Model],
    out_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Dedupe: group by provider, then by id (first wins = models.dev priority).
    // BTreeMap keeps both levels key-sorted so each regeneration produces a
    // deterministic, reviewable diff instead of HashMap's random iteration order.
    let mut by_provider: BTreeMap<String, BTreeMap<String, Model>> = BTreeMap::new();
    for m in all_models {
        let key = provider_key(m.provider).to_string();
        by_provider
            .entry(key)
            .or_default()
            .entry(m.id.clone())
            .or_insert_with(|| m.clone());
    }

    // Serialize, then fold every value back through serde_json's (non
    // round-tripping) f64 parser — the same one the runtime uses to load
    // MODELS_JSON. Price arithmetic leaves 1-ULP artifacts (e.g.
    // 0.0000002 * 1e6 -> 0.19999999999999998) that the parser collapses to
    // the neighbouring value (0.2) on read. Folding here makes the on-disk
    // catalog equal what every consumer actually loads, so regeneration is
    // churn-free and the committed file is the canonical form.
    let json = serde_json::to_string_pretty(&by_provider)?;
    let folded: BTreeMap<String, BTreeMap<String, Model>> = serde_json::from_str(&json)?;
    let json = serde_json::to_string_pretty(&folded)?;
    fs::write(out_path, json)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let models_dev = load_models_dev_data(&client).await;
    let open_router = fetch_openrouter_models(&client).await;
    let ai_gateway = fetch_ai_gateway_models(&client).await;

    let mut all: Vec<Model> = vec![];
    all.extend(models_dev);
    all.extend(open_router);
    all.extend(ai_gateway);

    // Fix Claude Opus 4.5 cache pricing
    if let Some(opus) = all
        .iter_mut()
        .find(|m| m.provider == Provider::Anthropic && m.id == "claude-opus-4-5")
    {
        opus.cost.cache_read = 0.5;
        opus.cost.cache_write = 6.25;
    }

    // Add missing OpenAI models (only if not already present)
    if !all
        .iter()
        .any(|m| m.provider == Provider::OpenAI && m.id == "gpt-5-chat-latest")
    {
        all.push(Model {
            id: "gpt-5-chat-latest".to_string(),
            name: "GPT-5 Chat Latest".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: "https://api.openai.com/v1".to_string(),
            reasoning: false,
            input: input_text_image(),
            cost: cost(1.25, 10.0, 0.125, 0.0),
            context_window: 128000,
            max_tokens: 16384,
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }

    if !all
        .iter()
        .any(|m| m.provider == Provider::OpenAI && m.id == "gpt-5.1-codex")
    {
        all.push(Model {
            id: "gpt-5.1-codex".to_string(),
            name: "GPT-5.1 Codex".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: "https://api.openai.com/v1".to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(1.25, 5.0, 0.125, 1.25),
            context_window: 400000,
            max_tokens: 128000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }

    if !all
        .iter()
        .any(|m| m.provider == Provider::OpenAI && m.id == "gpt-5.1-codex-max")
    {
        all.push(Model {
            id: "gpt-5.1-codex-max".to_string(),
            name: "GPT-5.1 Codex Max".to_string(),
            api: Api::OpenAIResponses,
            provider: Provider::OpenAI,
            base_url: "https://api.openai.com/v1".to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(1.25, 10.0, 0.125, 0.0),
            context_window: 400000,
            max_tokens: 128000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }

    // Add OpenAI Codex (ChatGPT OAuth) models
    // NOTE: These are not fetched from models.dev; we keep a small, explicit list to avoid aliases.
    // Context window is based on observed server limits (400s above ~272k), not marketing numbers.
    all.extend(static_codex_models());

    // Add missing Grok model (only if not already present)
    if !all
        .iter()
        .any(|m| m.provider == Provider::Xai && m.id == "grok-code-fast-1")
    {
        all.push(Model {
            id: "grok-code-fast-1".to_string(),
            name: "Grok Code Fast 1".to_string(),
            api: Api::OpenAICompletions,
            provider: Provider::Xai,
            base_url: "https://api.x.ai/v1".to_string(),
            reasoning: false,
            input: input_text(),
            cost: cost(0.2, 1.5, 0.02, 0.0),
            context_window: 32768,
            max_tokens: 8192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }

    // Add missing OpenRouter model (only if not already present)
    if !all
        .iter()
        .any(|m| m.provider == Provider::Openrouter && m.id == "openrouter/auto")
    {
        all.push(Model {
            id: "openrouter/auto".to_string(),
            name: "OpenRouter: Auto Router".to_string(),
            api: Api::OpenAICompletions,
            provider: Provider::Openrouter,
            base_url: OPENROUTER_API.to_string(),
            reasoning: true,
            input: input_text_image(),
            cost: cost(0.0, 0.0, 0.0, 0.0),
            context_window: 2_000_000,
            max_tokens: 30000,
            headers: None,
            compat: None,
            thinking_level_map: None,
        });
    }

    // Add Google Cloud Code Assist models
    all.extend(static_cloud_code_assist_models());

    // Add Google Antigravity models
    all.extend(static_antigravity_models());

    // Add Google Vertex models
    all.extend(static_vertex_models());

    // Add Kimi Coding models (fallback - only if not already present from models.dev)
    for model in static_kimi_coding_models() {
        if !all
            .iter()
            .any(|m| m.provider == Provider::KimiCoding && m.id == model.id)
        {
            all.push(model);
        }
    }

    // Add native DeepSeek models (fallback — models.dev doesn't currently
    // include them under the `deepseek` provider key, so we ship the
    // entries as first-party catalog records).
    for model in static_deepseek_models() {
        if !all
            .iter()
            .any(|m| m.provider == Provider::Deepseek && m.id == model.id)
        {
            all.push(model);
        }
    }

    // Azure OpenAI variants (copy from openai openai-responses)
    let azure_models: Vec<Model> = all
        .iter()
        .filter(|m| m.provider == Provider::OpenAI && m.api == Api::OpenAIResponses)
        .cloned()
        .map(|m| Model {
            api: Api::AzureOpenAiResponses,
            provider: Provider::AzureOpenAiResponses,
            base_url: String::new(),
            ..m
        })
        .collect();
    all.extend(azure_models);

    // Drop catalog entries that upstream aggregators still list but the
    // provider no longer serves on its public API endpoint, so embedders
    // that trust the catalog don't surface phantom models that fail at send
    // time with model_not_found. cerebras retired
    // qwen-3-235b-a22b-instruct-2507 (issue #94).
    all.retain(|m| !(m.provider == Provider::Cerebras && m.id == "qwen-3-235b-a22b-instruct-2507"));

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/models.json");
    write_generated(&all, &out_path)?;
    println!("Generated {}", out_path.display());

    let total = all.len();
    let reasoning = all.iter().filter(|m| m.reasoning).count();
    println!("\nModel Statistics:");
    println!("  Total tool-capable models: {total}");
    println!("  Reasoning-capable models: {reasoning}");

    let mut by_provider: HashMap<String, usize> = HashMap::new();
    for m in &all {
        *by_provider
            .entry(provider_key(m.provider).to_string())
            .or_default() += 1;
    }
    let mut keys: Vec<_> = by_provider.keys().collect();
    keys.sort();
    for k in keys {
        println!("  {}: {} models", k, by_provider[k]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_alias_set_matches_known_versions() {
        assert!(is_kimi_alias("k2p5"));
        assert!(is_kimi_alias("k2p6"));
    }

    #[test]
    fn kimi_alias_set_excludes_canonical_and_unrelated_ids() {
        assert!(!is_kimi_alias("kimi-for-coding"));
        assert!(!is_kimi_alias("kimi-k2-thinking"));
        assert!(!is_kimi_alias(""));
        assert!(!is_kimi_alias("k2"));
    }

    /// The four legacy GLM 4.5 ids reject the OpenAI-compatible
    /// `tool_stream: true` flag, so the generator must mark them as
    /// tool-stream-unsupported. Future z.ai ids fall back to the
    /// `is_zai` default `true` and are not in this set.
    #[test]
    fn zai_tool_stream_set_covers_legacy_glm_4_5_family() {
        for id in ["glm-4.5", "glm-4.5-air", "glm-4.5-flash", "glm-4.5v"] {
            assert!(
                is_zai_tool_stream_unsupported(id),
                "{id} must be marked tool-stream-unsupported"
            );
        }
    }

    /// Newer z.ai ids — glm-4.6 family and any non-listed coding-plan
    /// model — must NOT be in the unsupported set so the per-provider
    /// `is_zai = true` default leaves `zai_tool_stream` enabled.
    #[test]
    fn zai_tool_stream_set_excludes_newer_and_unrelated_ids() {
        for id in ["glm-4.6", "glm-4.6-air", "glm-4.6-thinking", "gpt-4o", ""] {
            assert!(
                !is_zai_tool_stream_unsupported(id),
                "{id} must NOT be marked tool-stream-unsupported"
            );
        }
    }

    /// GitHub Copilot Claude 4.x models route through the Anthropic
    /// Messages API. The matcher must cover every published variant —
    /// `claude-haiku-4`, `claude-sonnet-4.5`, `claude-opus-4-7`,
    /// dotted/dashed minor versions, and the bare top-level id.
    #[test]
    fn copilot_claude_4_matcher_recognises_published_variants() {
        for id in [
            "claude-haiku-4",
            "claude-haiku-4.5",
            "claude-sonnet-4",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5",
            "claude-opus-4",
            "claude-opus-4.6",
            "claude-opus-4-7",
            "claude-opus-4-7-20260101",
        ] {
            assert!(
                is_copilot_claude_4_model(id),
                "{id} should route through anthropic-messages"
            );
        }
    }

    /// The Copilot Anthropic-branch eager-streaming opt-out must cover
    /// every snapshot whose proxy rejects the per-tool
    /// `eager_input_streaming` flag. Today that is exactly haiku-4.5 plus
    /// the two sonnet-4 variants; unrelated ids (opus, older claude,
    /// gpt) must keep the default eager streaming path.
    #[test]
    fn copilot_eager_streaming_opt_out_covers_known_snapshots() {
        for id in ["claude-haiku-4.5", "claude-sonnet-4", "claude-sonnet-4.5"] {
            assert!(
                is_copilot_eager_streaming_unsupported(id),
                "{id} should opt out of eager_input_streaming"
            );
        }
    }

    #[test]
    fn copilot_eager_streaming_opt_out_excludes_unrelated_ids() {
        for id in [
            "claude-opus-4",
            "claude-opus-4.6",
            "claude-opus-4-7",
            "claude-haiku-4",
            "claude-sonnet-4-5",
            "claude-haiku-3-5",
            "gpt-5",
            "",
        ] {
            assert!(
                !is_copilot_eager_streaming_unsupported(id),
                "{id} must keep the default eager streaming path"
            );
        }
    }

    /// The matcher must NOT false-positive on older Claude generations
    /// (3.5, 3.7), unrelated providers, or a hypothetical `claude-haiku-40`
    /// family (the "4" must be followed by `.`, `-`, or end-of-string).
    #[test]
    fn copilot_claude_4_matcher_excludes_older_and_unrelated_ids() {
        for id in [
            "claude-haiku-3-5",
            "claude-sonnet-3-7",
            "claude-opus-3",
            "claude-haiku-40",
            "claude-sonnet-40-preview",
            "gpt-5",
            "gpt-5-codex",
            "oswe-preview",
            "",
        ] {
            assert!(
                !is_copilot_claude_4_model(id),
                "{id} must NOT route through anthropic-messages"
            );
        }
    }

    /// OpenRouter routes DeepSeek V3/V4 reasoning through openai-completions
    /// but the upstream still speaks DeepSeek's wire conventions (echo
    /// `reasoning_content` on every assistant turn, deepseek-shaped think
    /// blocks). Without the compat block the model returns reasoning tokens
    /// the agent can't replay back in context.
    #[test]
    fn openrouter_compat_targets_deepseek_v3_and_v4_models() {
        for id in [
            "deepseek/deepseek-v3-thinking",
            "deepseek/deepseek-v4-flash",
            "deepseek/deepseek-v4-pro",
        ] {
            let compat = openrouter_compat(id).unwrap_or_else(|| panic!("{id} should compat"));
            match compat {
                Compat::OpenAICompletions(c) => {
                    assert_eq!(c.thinking_format.as_deref(), Some("deepseek"), "{id}");
                    assert_eq!(
                        c.requires_reasoning_content_on_assistant_messages,
                        Some(true),
                        "{id}"
                    );
                }
                _ => panic!("{id}: expected OpenAICompletions compat"),
            }
        }
    }

    #[test]
    fn openrouter_compat_skips_unrelated_models() {
        assert!(openrouter_compat("openai/gpt-4o").is_none());
        assert!(openrouter_compat("anthropic/claude-sonnet").is_none());
        // V2 DeepSeek did not use the same wire conventions — only V3+ get
        // the compat block.
        assert!(openrouter_compat("deepseek/deepseek-v2").is_none());
        assert!(openrouter_compat("").is_none());
    }

    /// models.dev renamed the zAi entry from `zai` to `zai-coding-plan`.
    /// The generator must accept either key on the snapshot so a
    /// catalog refresh against either shape still produces zAi
    /// entries. The serde rename gives us the new key; the bare
    /// `zai` field still maps the legacy snapshots.
    #[test]
    fn models_dev_data_accepts_both_zai_and_zai_coding_plan_keys() {
        let new_shape = serde_json::json!({
            "zai-coding-plan": { "id": "zai-coding-plan", "models": {} }
        });
        let parsed: ModelsDevData = serde_json::from_value(new_shape).expect("new-shape parse ok");
        assert!(parsed.zai_coding_plan.is_some());
        assert!(parsed.zai.is_none());

        let legacy_shape = serde_json::json!({
            "zai": { "id": "zai", "models": {} }
        });
        let parsed: ModelsDevData =
            serde_json::from_value(legacy_shape).expect("legacy-shape parse ok");
        assert!(parsed.zai.is_some());
        assert!(parsed.zai_coding_plan.is_none());
    }
}
