//! Model resolution — parses model patterns, thinking levels, and provider/id combos.

use model::{Model, ThinkingLevel};

/// Parsed model pattern result.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
}

/// Parse a thinking level string.
pub fn parse_thinking_level(s: &str) -> Option<ThinkingLevel> {
    match s.to_lowercase().as_str() {
        "off" | "none" => Some(ThinkingLevel::Minimal), // no Off variant — use Minimal as lowest
        "minimal" | "min" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" | "med" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" | "max" => Some(ThinkingLevel::Xhigh),
        _ => None,
    }
}

/// Parse a model pattern like "sonnet", "claude-sonnet:high", "openai/gpt-4o".
/// Returns (provider_hint, model_pattern, thinking_level).
pub fn parse_model_pattern(pattern: &str) -> (Option<String>, String, Option<ThinkingLevel>) {
    let (provider_hint, rest) = if let Some(idx) = pattern.find('/') {
        (Some(pattern[..idx].to_string()), &pattern[idx + 1..])
    } else {
        (None, pattern)
    };

    // Check for thinking level suffix (last colon)
    if let Some(idx) = rest.rfind(':') {
        let (model_part, level_part) = (&rest[..idx], &rest[idx + 1..]);
        if let Some(level) = parse_thinking_level(level_part) {
            return (provider_hint, model_part.to_string(), Some(level));
        }
    }

    (provider_hint, rest.to_string(), None)
}

/// Resolve a model from provider + model_id pattern.
/// Tries exact match, then partial match across all providers.
pub fn resolve_model(provider: Option<&str>, model_id: &str) -> ResolvedModel {
    let (pattern_provider, pattern, thinking) = parse_model_pattern(model_id);

    // Effective provider: explicit > pattern > default
    let effective_provider = provider
        .map(String::from)
        .or(pattern_provider)
        .unwrap_or_else(|| "anthropic".to_string());

    // 1. Try exact match with provider
    if let Some(m) = model::get_model(&effective_provider, &pattern) {
        return ResolvedModel {
            model: m,
            thinking_level: thinking,
        };
    }

    // 2. Try partial match within the specified provider
    let provider_models = model::get_models(&effective_provider);
    if let Some(m) = find_best_match(&pattern, &provider_models) {
        return ResolvedModel {
            model: m,
            thinking_level: thinking,
        };
    }

    // 3. Try all providers for a match
    for prov_key in model::get_provider_keys() {
        let models = model::get_models(&prov_key);
        if let Some(m) = find_best_match(&pattern, &models) {
            return ResolvedModel {
                model: m,
                thinking_level: thinking,
            };
        }
    }

    // 4. Build a fallback model
    ResolvedModel {
        model: build_fallback_model(&effective_provider, &pattern),
        thinking_level: thinking,
    }
}

/// Find the best matching model from a list.
/// Priority: exact id > contains id > contains name.
fn find_best_match(pattern: &str, models: &[Model]) -> Option<Model> {
    let lower = pattern.to_lowercase();

    // Exact match by id
    if let Some(m) = models.iter().find(|m| m.id.to_lowercase() == lower) {
        return Some(m.clone());
    }

    // Collect partial matches (id contains pattern)
    let mut matches: Vec<&Model> = models
        .iter()
        .filter(|m| m.id.to_lowercase().contains(&lower) || m.name.to_lowercase().contains(&lower))
        .collect();

    if matches.is_empty() {
        return None;
    }

    // Prefer alias (no date suffix) over dated versions
    matches.sort_by(|a, b| {
        let a_is_alias = !has_date_suffix(&a.id);
        let b_is_alias = !has_date_suffix(&b.id);
        b_is_alias.cmp(&a_is_alias).then(b.id.cmp(&a.id))
    });

    Some(matches[0].clone())
}

/// Check if a model ID ends with a date-like suffix (-YYYYMMDD).
fn has_date_suffix(id: &str) -> bool {
    if let Some(last) = id.rsplit('-').next() {
        last.len() == 8 && last.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// Build a fallback model for an unknown model ID.
fn build_fallback_model(provider: &str, model_id: &str) -> Model {
    let (provider_enum, api) = match provider {
        "anthropic" => (
            model::types::Provider::Anthropic,
            model::types::Api::AnthropicMessages,
        ),
        "openai" => (
            model::types::Provider::OpenAI,
            model::types::Api::OpenAICompletions,
        ),
        "google" => (
            model::types::Provider::Google,
            model::types::Api::GoogleGenerativeAi,
        ),
        "mistral" => (
            model::types::Provider::Mistral,
            model::types::Api::OpenAICompletions,
        ),
        "groq" => (
            model::types::Provider::Groq,
            model::types::Api::OpenAICompletions,
        ),
        "xai" => (
            model::types::Provider::Xai,
            model::types::Api::OpenAICompletions,
        ),
        "openrouter" => (
            model::types::Provider::Openrouter,
            model::types::Api::OpenAICompletions,
        ),
        "deepseek" | "together" | "fireworks" => (
            model::types::Provider::Openrouter, // Route through OpenRouter for now
            model::types::Api::OpenAICompletions,
        ),
        "azure" => (
            model::types::Provider::AzureOpenAiResponses,
            model::types::Api::OpenAICompletions,
        ),
        "bedrock" | "amazon-bedrock" => (
            model::types::Provider::AmazonBedrock,
            model::types::Api::BedrockConverseStream,
        ),
        "github-copilot" => (
            model::types::Provider::GitHubCopilot,
            model::types::Api::OpenAICompletions,
        ),
        _ => (
            model::types::Provider::Anthropic,
            model::types::Api::AnthropicMessages,
        ),
    };

    Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        api,
        provider: provider_enum,
        base_url: String::new(),
        reasoning: false,
        input: vec![model::InputType::Text],
        cost: model::Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

/// List all available models, optionally filtered by a search pattern.
pub fn list_models(search: Option<&str>) -> Vec<Model> {
    let mut all_models = Vec::new();
    for provider_key in model::get_provider_keys() {
        all_models.extend(model::get_models(&provider_key));
    }

    if let Some(search) = search {
        let lower = search.to_lowercase();
        all_models.retain(|m| {
            m.id.to_lowercase().contains(&lower)
                || m.name.to_lowercase().contains(&lower)
                || m.provider.as_str().to_lowercase().contains(&lower)
        });
    }

    all_models.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then(a.id.cmp(&b.id))
    });
    all_models
}

/// Get the default model ID for a given provider.
pub fn default_model_for_provider(provider: &str) -> &str {
    match provider {
        "anthropic" => "claude-sonnet-4-20250514",
        "openai" => "gpt-4o",
        "google" => "gemini-2.5-pro",
        "mistral" => "mistral-large-latest",
        "groq" => "llama-3.3-70b-versatile",
        "xai" => "grok-3",
        "openrouter" => "anthropic/claude-sonnet-4-20250514",
        "deepseek" => "deepseek-chat",
        "together" => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        "fireworks" => "accounts/fireworks/models/llama-v3p1-70b-instruct",
        "github-copilot" => "gpt-4o",
        _ => "claude-sonnet-4-20250514",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thinking_level() {
        assert_eq!(parse_thinking_level("high"), Some(ThinkingLevel::High));
        assert_eq!(parse_thinking_level("off"), Some(ThinkingLevel::Minimal));
        assert_eq!(parse_thinking_level("xhigh"), Some(ThinkingLevel::Xhigh));
        assert_eq!(parse_thinking_level("max"), Some(ThinkingLevel::Xhigh));
        assert!(parse_thinking_level("invalid").is_none());
    }

    #[test]
    fn test_parse_model_pattern_simple() {
        let (prov, pat, think) = parse_model_pattern("sonnet");
        assert!(prov.is_none());
        assert_eq!(pat, "sonnet");
        assert!(think.is_none());
    }

    #[test]
    fn test_parse_model_pattern_with_provider() {
        let (prov, pat, think) = parse_model_pattern("openai/gpt-4o");
        assert_eq!(prov.as_deref(), Some("openai"));
        assert_eq!(pat, "gpt-4o");
        assert!(think.is_none());
    }

    #[test]
    fn test_parse_model_pattern_with_thinking() {
        let (prov, pat, think) = parse_model_pattern("claude-sonnet:high");
        assert!(prov.is_none());
        assert_eq!(pat, "claude-sonnet");
        assert_eq!(think, Some(ThinkingLevel::High));
    }

    #[test]
    fn test_parse_model_pattern_provider_and_thinking() {
        let (prov, pat, think) = parse_model_pattern("anthropic/sonnet:medium");
        assert_eq!(prov.as_deref(), Some("anthropic"));
        assert_eq!(pat, "sonnet");
        assert!(matches!(think, Some(ThinkingLevel::Medium)));
    }

    #[test]
    fn test_has_date_suffix() {
        assert!(has_date_suffix("claude-sonnet-4-20250514"));
        assert!(!has_date_suffix("claude-sonnet-4"));
        assert!(!has_date_suffix("gpt-4o"));
    }

    #[test]
    fn test_resolve_model_fallback() {
        let result = resolve_model(Some("anthropic"), "nonexistent-model-xyz");
        assert_eq!(result.model.id, "nonexistent-model-xyz");
        assert!(result.thinking_level.is_none());
    }

    #[test]
    fn test_resolve_model_with_thinking_suffix() {
        let result = resolve_model(None, "nonexistent:high");
        assert_eq!(result.model.id, "nonexistent");
        assert_eq!(result.thinking_level, Some(ThinkingLevel::High));
    }

    #[test]
    fn test_list_models_no_filter() {
        let models = list_models(None);
        // Should return all registered models
        assert!(!models.is_empty());
    }

    #[test]
    fn test_list_models_filtered() {
        let models = list_models(Some("claude"));
        for m in &models {
            let matches = m.id.to_lowercase().contains("claude")
                || m.name.to_lowercase().contains("claude")
                || m.provider.as_str().to_lowercase().contains("claude");
            assert!(matches, "model {} should match 'claude'", m.id);
        }
    }

    #[test]
    fn test_default_model_for_provider() {
        assert!(default_model_for_provider("anthropic").contains("claude"));
        assert!(default_model_for_provider("openai").contains("gpt"));
        assert!(default_model_for_provider("google").contains("gemini"));
    }
}
