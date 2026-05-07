//! Model resolution — parses model patterns, thinking levels, and provider/id combos.
//!
//! Mirrors `pi-mono`'s `core/model-resolver.ts`. See module functions for
//! TS-parity helpers (`find_exact_model_reference_match`, `parse_model_pattern_full`,
//! `resolve_model_scope`, `resolve_cli_model`, `find_initial_model`,
//! `restore_model_from_session`). The legacy `resolve_model`/`parse_model_pattern`
//! signatures are preserved for existing call sites in `main` and `session_setup`.

use model::{Model, ThinkingLevel};

/// Default model id for each known provider, used as the seed for fallback
/// model construction and the priority order in `find_initial_model`.
///
/// Mirrors `defaultModelPerProvider` in `pi-mono/core/model-resolver.ts`.
/// Iteration order is the declaration order, so callers that scan for a
/// "preferred" model see the same priority as the TS reference.
pub fn default_model_per_provider() -> &'static [(&'static str, &'static str)] {
    &[
        ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
        ("anthropic", "claude-opus-4-7"),
        ("openai", "gpt-5.4"),
        ("azure-openai-responses", "gpt-5.4"),
        ("openai-codex", "gpt-5.5"),
        ("deepseek", "deepseek-v4-pro"),
        ("google", "gemini-3.1-pro-preview"),
        ("google-vertex", "gemini-3.1-pro-preview"),
        ("github-copilot", "gpt-5.4"),
        ("openrouter", "moonshotai/kimi-k2.6"),
        ("vercel-ai-gateway", "zai/glm-5.1"),
        ("xai", "grok-4.20-0309-reasoning"),
        ("groq", "openai/gpt-oss-120b"),
        ("cerebras", "zai-glm-4.7"),
        ("zai", "glm-5.1"),
        ("mistral", "devstral-medium-latest"),
        ("minimax", "MiniMax-M2.7"),
        ("minimax-cn", "MiniMax-M2.7"),
        ("moonshotai", "kimi-k2.6"),
        ("moonshotai-cn", "kimi-k2.6"),
        ("huggingface", "moonshotai/Kimi-K2.6"),
        ("fireworks", "accounts/fireworks/models/kimi-k2p6"),
        ("opencode", "kimi-k2.6"),
        ("opencode-go", "kimi-k2.6"),
        ("kimi-coding", "kimi-for-coding"),
        ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
        (
            "cloudflare-ai-gateway",
            "workers-ai/@cf/moonshotai/kimi-k2.6",
        ),
        ("xiaomi", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
    ]
}

/// Default model id for a given provider, if known.
///
/// Looks up `default_model_per_provider` by provider key. Returns `None` for
/// providers not in the table.
pub fn default_model_id_for_known_provider(provider: &str) -> Option<&'static str> {
    default_model_per_provider()
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, id)| *id)
}

/// Whether a model id looks like an alias (no date suffix).
///
/// An alias is either a `-latest` suffix or any id that does **not** end in
/// `-YYYYMMDD` (8 trailing digits after the last `-`). Mirrors `isAlias` in
/// `pi-mono/core/model-resolver.ts`.
pub fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    !has_date_suffix(id)
}

/// Pattern + thinking-level result returned by [`parse_model_pattern_full`].
///
/// Mirrors `ParsedModelResult` in the TS reference. `warning` is `Some` when
/// the pattern parsed but contained an invalid thinking-level suffix that we
/// chose to swallow (only in scope mode — see `parse_model_pattern_full`).
#[derive(Debug, Clone)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub warning: Option<String>,
}

/// A scoped model — typically the result of [`resolve_model_scope`].
///
/// Mirrors `ScopedModel` in the TS reference: `thinking_level` is set only
/// when the scope pattern explicitly named one (e.g. `"sonnet:high"`).
#[derive(Debug, Clone)]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
}

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

/// Find an exact model reference match.
///
/// Mirrors `findExactModelReferenceMatch` in the TS reference. Supports either
/// a bare model id or a canonical `provider/modelId` reference. When matching
/// by bare id, ambiguous matches across providers are rejected (returns
/// `None`).
pub fn find_exact_model_reference_match<'a>(
    model_reference: &str,
    available_models: &'a [Model],
) -> Option<&'a Model> {
    let trimmed = model_reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_lowercase();

    // 1. Canonical "provider/id" match (case-insensitive). Reject ambiguity.
    let canonical: Vec<&Model> = available_models
        .iter()
        .filter(|m| format!("{}/{}", m.provider.as_str(), m.id).to_lowercase() == normalized)
        .collect();
    if canonical.len() == 1 {
        return Some(canonical[0]);
    }
    if canonical.len() > 1 {
        return None;
    }

    // 2. Split on first "/", treat as provider/id.
    if let Some(slash_idx) = trimmed.find('/') {
        let provider = trimmed[..slash_idx].trim();
        let model_id = trimmed[slash_idx + 1..].trim();
        if !provider.is_empty() && !model_id.is_empty() {
            let provider_l = provider.to_lowercase();
            let model_id_l = model_id.to_lowercase();
            let matches: Vec<&Model> = available_models
                .iter()
                .filter(|m| {
                    m.provider.as_str().to_lowercase() == provider_l
                        && m.id.to_lowercase() == model_id_l
                })
                .collect();
            if matches.len() == 1 {
                return Some(matches[0]);
            }
            if matches.len() > 1 {
                return None;
            }
        }
    }

    // 3. Bare id match across providers — only if exactly one.
    let id_matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.id.to_lowercase() == normalized)
        .collect();
    if id_matches.len() == 1 {
        Some(id_matches[0])
    } else {
        None
    }
}

/// Try to match a pattern to a model from `available_models`.
///
/// Mirrors `tryMatchModel` in the TS reference. Tries
/// [`find_exact_model_reference_match`] first, then falls back to substring
/// matching against id and name. Among partial matches, aliases (no date
/// suffix) are preferred over dated versions; ties break by id descending.
fn try_match_model<'a>(model_pattern: &str, available_models: &'a [Model]) -> Option<&'a Model> {
    if let Some(exact) = find_exact_model_reference_match(model_pattern, available_models) {
        return Some(exact);
    }

    let lower = model_pattern.to_lowercase();
    let matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.id.to_lowercase().contains(&lower) || m.name.to_lowercase().contains(&lower))
        .collect();
    if matches.is_empty() {
        return None;
    }

    let mut aliases: Vec<&Model> = matches
        .iter()
        .copied()
        .filter(|m| is_alias(&m.id))
        .collect();
    let mut dated: Vec<&Model> = matches
        .iter()
        .copied()
        .filter(|m| !is_alias(&m.id))
        .collect();

    if !aliases.is_empty() {
        aliases.sort_by(|a, b| b.id.cmp(&a.id));
        Some(aliases[0])
    } else {
        dated.sort_by(|a, b| b.id.cmp(&a.id));
        Some(dated[0])
    }
}

/// Whether `level` is a TS-parity thinking level literal.
///
/// Mirrors `isValidThinkingLevel` in `pi-mono/cli/args.ts`. Strict — accepts
/// only the canonical literals (`off`, `minimal`, `low`, `medium`, `high`,
/// `xhigh`). Use this for pattern parsing where the suffix must be one of the
/// documented values, distinct from the more permissive [`parse_thinking_level`]
/// which accepts aliases like `min`/`med`/`max`/`none`.
pub fn is_valid_thinking_level_literal(level: &str) -> bool {
    matches!(
        level,
        "off" | "minimal" | "low" | "medium" | "high" | "xhigh"
    )
}

/// Map a TS-parity thinking-level literal to a [`ThinkingLevel`].
///
/// Returns `None` for non-literal inputs. `"off"` maps to `ThinkingLevel::Minimal`
/// (the lowest variant) since the Rust enum has no `Off`.
fn parse_thinking_level_literal(level: &str) -> Option<ThinkingLevel> {
    match level {
        "off" => Some(ThinkingLevel::Minimal),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        _ => None,
    }
}

/// Options for [`parse_model_pattern_full`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseModelPatternOptions {
    /// When `true` (default) and the pattern's last colon-suffix is not a
    /// valid thinking level, fall back to recursing on the prefix and emit a
    /// warning. When `false`, treat the suffix as part of the model id —
    /// useful for strict CLI parsing where we don't want to silently resolve
    /// to a different model.
    pub allow_invalid_thinking_level_fallback: bool,
}

impl ParseModelPatternOptions {
    /// Permissive mode (the default in TS). Allows fallback with warning.
    pub fn permissive() -> Self {
        Self {
            allow_invalid_thinking_level_fallback: true,
        }
    }

    /// Strict mode (used by `resolve_cli_model`). Disables warning fallback.
    pub fn strict() -> Self {
        Self {
            allow_invalid_thinking_level_fallback: false,
        }
    }
}

/// Parse a pattern to extract model and thinking level.
///
/// Mirrors `parseModelPattern` in the TS reference. The algorithm:
///
/// 1. Try to match the full pattern as a model.
/// 2. If found, return it without a thinking level.
/// 3. If not found and the pattern contains `:`, split on the **last** colon:
///    - If the suffix is a valid thinking level, recurse on the prefix and
///      attach this level.
///    - If the suffix is invalid and `allow_invalid_thinking_level_fallback`
///      is set, recurse on the prefix anyway and surface a warning. Otherwise
///      bail with `model: None`.
///
/// This is the public TS-parity equivalent. The legacy
/// [`parse_model_pattern`] (tuple-returning) variant is preserved for
/// existing in-tree callers.
pub fn parse_model_pattern_full(
    pattern: &str,
    available_models: &[Model],
    options: ParseModelPatternOptions,
) -> ParsedModelResult {
    if let Some(exact) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(exact.clone()),
            thinking_level: None,
            warning: None,
        };
    }

    let last_colon = match pattern.rfind(':') {
        Some(idx) => idx,
        None => {
            return ParsedModelResult {
                model: None,
                thinking_level: None,
                warning: None,
            };
        }
    };

    let prefix = &pattern[..last_colon];
    let suffix = &pattern[last_colon + 1..];

    if is_valid_thinking_level_literal(suffix) {
        let level = parse_thinking_level_literal(suffix);
        let inner = parse_model_pattern_full(prefix, available_models, options);
        if inner.model.is_some() {
            // Suppress the explicit thinking level if the recursive call
            // already produced a warning — the inner ambiguity wins.
            let thinking_level = if inner.warning.is_some() { None } else { level };
            return ParsedModelResult {
                model: inner.model,
                thinking_level,
                warning: inner.warning,
            };
        }
        return inner;
    }

    if !options.allow_invalid_thinking_level_fallback {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    }

    let inner = parse_model_pattern_full(prefix, available_models, options);
    if inner.model.is_some() {
        let warning = format!(
            "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
        );
        return ParsedModelResult {
            model: inner.model,
            thinking_level: None,
            warning: Some(warning),
        };
    }
    inner
}

/// Build a fallback model by cloning a real model from the same provider
/// in `available_models` and overriding its id/name.
///
/// Mirrors `buildFallbackModel` in the TS reference. Returns `None` when no
/// model from `provider` is in the catalog (in which case the caller should
/// surface "unknown provider"). Prefers the provider's
/// [`default_model_id_for_known_provider`] when present, otherwise the first
/// model from that provider.
pub fn build_fallback_model_from_available(
    provider: &str,
    model_id: &str,
    available_models: &[Model],
) -> Option<Model> {
    let provider_models: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.provider.as_str() == provider)
        .collect();
    if provider_models.is_empty() {
        return None;
    }
    let default_id = default_model_id_for_known_provider(provider);
    let base = match default_id {
        Some(did) => provider_models
            .iter()
            .copied()
            .find(|m| m.id == did)
            .unwrap_or(provider_models[0]),
        None => provider_models[0],
    };
    let mut clone = base.clone();
    clone.id = model_id.to_string();
    clone.name = model_id.to_string();
    Some(clone)
}

/// Outcome diagnostics produced by [`resolve_model_scope`].
///
/// `models` is the deduplicated, ordered list of [`ScopedModel`]s that
/// matched. `warnings` collects per-pattern messages (e.g. "no models
/// match"). The TS reference logs these to `stderr`; in Rust we surface them
/// for the caller to render.
#[derive(Debug, Clone, Default)]
pub struct ResolveModelScopeResult {
    pub models: Vec<ScopedModel>,
    pub warnings: Vec<String>,
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches_with(
            candidate,
            glob::MatchOptions {
                case_sensitive: false,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            },
        ),
        Err(_) => false,
    }
}

/// Resolve a list of model patterns into [`ScopedModel`]s.
///
/// Mirrors `resolveModelScope` in the TS reference. Each pattern may be:
/// - a glob (contains `*`, `?`, or `[`) — matched against `provider/id` and
///   bare `id`, with an optional `:level` suffix.
/// - a non-glob — handed to [`parse_model_pattern_full`] in permissive mode.
///
/// Duplicates (same provider+id) are dropped. Per-pattern warnings (e.g. no
/// matches, invalid thinking level) are collected in
/// [`ResolveModelScopeResult::warnings`] rather than printed.
pub fn resolve_model_scope(
    patterns: &[String],
    available_models: &[Model],
) -> ResolveModelScopeResult {
    let mut result = ResolveModelScopeResult::default();

    for pattern in patterns {
        let is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        if is_glob {
            // Optional ":level" suffix.
            let mut glob_pattern = pattern.as_str();
            let mut thinking_level: Option<ThinkingLevel> = None;
            if let Some(colon_idx) = pattern.rfind(':') {
                let suffix = &pattern[colon_idx + 1..];
                if is_valid_thinking_level_literal(suffix) {
                    thinking_level = parse_thinking_level_literal(suffix);
                    glob_pattern = &pattern[..colon_idx];
                }
            }

            let matching: Vec<&Model> = available_models
                .iter()
                .filter(|m| {
                    let full = format!("{}/{}", m.provider.as_str(), m.id);
                    glob_matches(glob_pattern, &full) || glob_matches(glob_pattern, &m.id)
                })
                .collect();

            if matching.is_empty() {
                result
                    .warnings
                    .push(format!("No models match pattern \"{pattern}\""));
                continue;
            }

            for m in matching {
                let already = result.models.iter().any(|sm| {
                    sm.model.provider.as_str() == m.provider.as_str() && sm.model.id == m.id
                });
                if !already {
                    result.models.push(ScopedModel {
                        model: m.clone(),
                        thinking_level,
                    });
                }
            }
            continue;
        }

        let parsed = parse_model_pattern_full(
            pattern,
            available_models,
            ParseModelPatternOptions::permissive(),
        );
        if let Some(w) = parsed.warning {
            result.warnings.push(w);
        }
        let model = match parsed.model {
            Some(m) => m,
            None => {
                result
                    .warnings
                    .push(format!("No models match pattern \"{pattern}\""));
                continue;
            }
        };
        let already = result.models.iter().any(|sm| {
            sm.model.provider.as_str() == model.provider.as_str() && sm.model.id == model.id
        });
        if !already {
            result.models.push(ScopedModel {
                model,
                thinking_level: parsed.thinking_level,
            });
        }
    }

    result
}

/// Result of [`resolve_cli_model`].
///
/// Mirrors `ResolveCliModelResult` in the TS reference. When `error` is
/// `Some`, `model` is always `None`. `warning` is non-fatal (e.g. fallback
/// custom-id construction).
#[derive(Debug, Clone, Default)]
pub struct ResolveCliModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// Resolve a single model from CLI flags.
///
/// Mirrors `resolveCliModel` in the TS reference. Supports:
/// - `--provider <provider> --model <pattern>`
/// - `--model <provider>/<pattern>`
/// - Fuzzy matching (same rules as [`parse_model_pattern_full`] in strict
///   mode, falling back to a custom-id model for known providers).
///
/// `available_models` should be the *full* catalog (not just auth-configured
/// models) so `--api-key` can be used for first-time setup.
pub fn resolve_cli_model(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    available_models: &[Model],
) -> ResolveCliModelResult {
    let cli_model = match cli_model {
        Some(m) => m,
        None => return ResolveCliModelResult::default(),
    };

    if available_models.is_empty() {
        return ResolveCliModelResult {
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_string(),
            ),
            ..Default::default()
        };
    }

    // Build canonical provider lookup (case-insensitive).
    let canonical_provider = |needle: &str| -> Option<String> {
        let lower = needle.to_lowercase();
        available_models
            .iter()
            .find(|m| m.provider.as_str().to_lowercase() == lower)
            .map(|m| m.provider.as_str().to_string())
    };

    let mut provider: Option<String> = None;
    if let Some(cp) = cli_provider {
        match canonical_provider(cp) {
            Some(p) => provider = Some(p),
            None => {
                return ResolveCliModelResult {
                    error: Some(format!(
                        "Unknown provider \"{cp}\". Use --list-models to see available providers/models."
                    )),
                    ..Default::default()
                };
            }
        }
    }

    let mut pattern = cli_model.to_string();
    let mut inferred_provider = false;

    if provider.is_none()
        && let Some(slash_idx) = cli_model.find('/')
    {
        let maybe_provider = &cli_model[..slash_idx];
        if let Some(canonical) = canonical_provider(maybe_provider) {
            provider = Some(canonical);
            pattern = cli_model[slash_idx + 1..].to_string();
            inferred_provider = true;
        }
    }

    // No provider inferred — try exact match without provider inference.
    if provider.is_none() {
        let lower = cli_model.to_lowercase();
        let exact = available_models.iter().find(|m| {
            m.id.to_lowercase() == lower
                || format!("{}/{}", m.provider.as_str(), m.id).to_lowercase() == lower
        });
        if let Some(exact) = exact {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                ..Default::default()
            };
        }
    }

    // Both provided — tolerate `--model <provider>/<pattern>`.
    if cli_provider.is_some()
        && let Some(prov) = provider.as_deref()
    {
        let prefix = format!("{prov}/");
        if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
            pattern = cli_model[prefix.len()..].to_string();
        }
    }

    let candidates: Vec<Model> = match provider.as_deref() {
        Some(p) => available_models
            .iter()
            .filter(|m| m.provider.as_str() == p)
            .cloned()
            .collect(),
        None => available_models.to_vec(),
    };

    let parsed =
        parse_model_pattern_full(&pattern, &candidates, ParseModelPatternOptions::strict());

    if let Some(model) = parsed.model {
        return ResolveCliModelResult {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }

    // Inferred provider with no match — try the full input across all models.
    if inferred_provider {
        let lower = cli_model.to_lowercase();
        if let Some(exact) = available_models.iter().find(|m| {
            m.id.to_lowercase() == lower
                || format!("{}/{}", m.provider.as_str(), m.id).to_lowercase() == lower
        }) {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                ..Default::default()
            };
        }
        let fallback = parse_model_pattern_full(
            cli_model,
            available_models,
            ParseModelPatternOptions::strict(),
        );
        if fallback.model.is_some() {
            return ResolveCliModelResult {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }

    if let Some(prov) = provider.as_deref()
        && let Some(fallback) =
            build_fallback_model_from_available(prov, &pattern, available_models)
    {
        let msg = format!(
            "Model \"{pattern}\" not found for provider \"{prov}\". Using custom model id."
        );
        let warning = match parsed.warning {
            Some(w) => Some(format!("{w} {msg}")),
            None => Some(msg),
        };
        return ResolveCliModelResult {
            model: Some(fallback),
            thinking_level: None,
            warning,
            error: None,
        };
    }

    let display = match provider.as_deref() {
        Some(p) => format!("{p}/{pattern}"),
        None => cli_model.to_string(),
    };
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

/// Result of [`find_initial_model`].
///
/// Mirrors `InitialModelResult` in the TS reference.
#[derive(Debug, Clone)]
pub struct InitialModelResult {
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    pub fallback_message: Option<String>,
}

/// Default thinking level when no other source supplies one.
///
/// Mirrors `DEFAULT_THINKING_LEVEL = "medium"` in `pi-mono/core/defaults.ts`.
pub const DEFAULT_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// Inputs to [`find_initial_model`].
///
/// Mirrors the option bag in the TS reference. `available_models` is the
/// auth-configured catalog; `all_models` is the full catalog used for CLI
/// resolution (so `--api-key` can be used for first-time setup).
#[derive(Debug, Clone, Default)]
pub struct FindInitialModelArgs<'a> {
    pub cli_provider: Option<&'a str>,
    pub cli_model: Option<&'a str>,
    pub scoped_models: &'a [ScopedModel],
    pub is_continuing: bool,
    pub default_provider: Option<&'a str>,
    pub default_model_id: Option<&'a str>,
    pub default_thinking_level: Option<ThinkingLevel>,
    pub available_models: &'a [Model],
    pub all_models: &'a [Model],
}

/// Outcome variants for [`find_initial_model`].
///
/// The [`Resolved`](Self::Resolved) variant is boxed to avoid a wide size gap
/// with [`CliError`](Self::CliError) (which is just a string). Callers should
/// use the helper accessors or `match` directly.
#[derive(Debug, Clone)]
pub enum FindInitialModelOutcome {
    /// Resolution succeeded.
    Resolved(Box<InitialModelResult>),
    /// CLI flags requested a model that the registry rejected. The TS
    /// reference exits the process here; in Rust we surface the error to the
    /// caller so it can decide.
    CliError(String),
}

/// Find the initial model to use, following the TS-reference priority:
///
/// 1. CLI args (provider + model). Returns [`FindInitialModelOutcome::CliError`]
///    on failure rather than panicking.
/// 2. First model from `scoped_models` (skipped when `is_continuing`).
/// 3. Saved default from settings.
/// 4. First available auth-configured model, preferring known-provider
///    defaults from [`default_model_per_provider`].
/// 5. None.
pub fn find_initial_model(args: FindInitialModelArgs<'_>) -> FindInitialModelOutcome {
    // 1. CLI args take priority.
    if let (Some(cp), Some(cm)) = (args.cli_provider, args.cli_model) {
        let resolved = resolve_cli_model(Some(cp), Some(cm), args.all_models);
        if let Some(err) = resolved.error {
            return FindInitialModelOutcome::CliError(err);
        }
        if let Some(m) = resolved.model {
            return FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
                model: Some(m),
                thinking_level: DEFAULT_THINKING_LEVEL,
                fallback_message: None,
            }));
        }
    }

    // 2. First scoped model — skip if continuing.
    if !args.is_continuing && !args.scoped_models.is_empty() {
        let sm = &args.scoped_models[0];
        let level = sm
            .thinking_level
            .or(args.default_thinking_level)
            .unwrap_or(DEFAULT_THINKING_LEVEL);
        return FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
            model: Some(sm.model.clone()),
            thinking_level: level,
            fallback_message: None,
        }));
    }

    // 3. Saved default from settings.
    if let (Some(prov), Some(id)) = (args.default_provider, args.default_model_id)
        && let Some(found) = args
            .all_models
            .iter()
            .find(|m| m.provider.as_str() == prov && m.id == id)
    {
        return FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
            model: Some(found.clone()),
            thinking_level: args
                .default_thinking_level
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        }));
    }

    // 4. First auth-configured model — prefer known provider defaults.
    if !args.available_models.is_empty() {
        for (prov, default_id) in default_model_per_provider() {
            if let Some(found) = args
                .available_models
                .iter()
                .find(|m| m.provider.as_str() == *prov && m.id == *default_id)
            {
                return FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
                    model: Some(found.clone()),
                    thinking_level: DEFAULT_THINKING_LEVEL,
                    fallback_message: None,
                }));
            }
        }
        return FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
            model: Some(args.available_models[0].clone()),
            thinking_level: DEFAULT_THINKING_LEVEL,
            fallback_message: None,
        }));
    }

    FindInitialModelOutcome::Resolved(Box::new(InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    }))
}

/// Result of [`restore_model_from_session`].
#[derive(Debug, Clone)]
pub struct RestoreSessionModelResult {
    pub model: Option<Model>,
    pub fallback_message: Option<String>,
    /// Informational lines the caller may want to render. Mirrors the TS
    /// reference's `console.log` / `console.error` calls — in Rust we surface
    /// them so the caller (CLI vs RPC) decides how to present them.
    pub messages: Vec<String>,
}

/// Restore model from session, with fallback to an available model.
///
/// Mirrors `restoreModelFromSession` in the TS reference. The caller supplies
/// an `auth_check` predicate because the Rust [`crate::core::model_registry::ModelRegistry`]
/// does not currently track per-model auth state — see the module docs for
/// the registry. Returning `false` from `auth_check` is equivalent to "no
/// auth configured" in the TS path.
pub fn restore_model_from_session(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<&Model>,
    available_models: &[Model],
    all_models: &[Model],
    auth_check: impl Fn(&Model) -> bool,
) -> RestoreSessionModelResult {
    let restored = all_models
        .iter()
        .find(|m| m.provider.as_str() == saved_provider && m.id == saved_model_id);
    let has_auth = restored.is_some_and(&auth_check);

    if let (Some(r), true) = (restored, has_auth) {
        return RestoreSessionModelResult {
            model: Some(r.clone()),
            fallback_message: None,
            messages: vec![format!("Restored model: {saved_provider}/{saved_model_id}")],
        };
    }

    let reason = if restored.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };
    let mut messages = vec![format!(
        "Warning: Could not restore model {saved_provider}/{saved_model_id} ({reason})."
    )];

    if let Some(curr) = current_model {
        let curr_provider = curr.provider.as_str().to_string();
        let curr_id = curr.id.clone();
        messages.push(format!("Falling back to: {curr_provider}/{curr_id}"));
        return RestoreSessionModelResult {
            model: Some(curr.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {curr_provider}/{curr_id}."
            )),
            messages,
        };
    }

    if !available_models.is_empty() {
        let mut fallback: Option<&Model> = None;
        for (prov, default_id) in default_model_per_provider() {
            if let Some(found) = available_models
                .iter()
                .find(|m| m.provider.as_str() == *prov && m.id == *default_id)
            {
                fallback = Some(found);
                break;
            }
        }
        let chosen = fallback.unwrap_or(&available_models[0]);
        let chosen_provider = chosen.provider.as_str().to_string();
        let chosen_id = chosen.id.clone();
        messages.push(format!("Falling back to: {chosen_provider}/{chosen_id}"));
        return RestoreSessionModelResult {
            model: Some(chosen.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {chosen_provider}/{chosen_id}."
            )),
            messages,
        };
    }

    RestoreSessionModelResult {
        model: None,
        fallback_message: None,
        messages,
    }
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

    // -------------------------------------------------------------------
    // TS-parity helpers: synthetic-model fixtures and unit tests.
    // -------------------------------------------------------------------

    fn fake(provider: model::types::Provider, id: &str) -> Model {
        fake_named(provider, id, id)
    }

    fn fake_named(provider: model::types::Provider, id: &str, name: &str) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            api: model::types::Api::AnthropicMessages,
            provider,
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

    #[test]
    fn is_alias_recognises_latest_and_undated_ids() {
        assert!(is_alias("claude-sonnet-latest"));
        assert!(is_alias("claude-sonnet-4"));
        assert!(is_alias("gpt-4o"));
        assert!(!is_alias("claude-sonnet-4-20250514"));
        assert!(!is_alias("gpt-4o-20240513"));
    }

    #[test]
    fn default_model_id_for_known_provider_returns_some_for_anthropic() {
        assert_eq!(
            default_model_id_for_known_provider("anthropic"),
            Some("claude-opus-4-7")
        );
        assert!(default_model_id_for_known_provider("not-a-provider").is_none());
    }

    #[test]
    fn find_exact_canonical_match_returns_unique_match() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::OpenAI, "gpt-4o"),
        ];
        let m = find_exact_model_reference_match("anthropic/claude-sonnet-4", &models);
        assert!(m.is_some());
        assert_eq!(m.unwrap().id, "claude-sonnet-4");
    }

    #[test]
    fn find_exact_bare_id_match_rejects_ambiguity_across_providers() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "shared-id"),
            fake(model::types::Provider::OpenAI, "shared-id"),
        ];
        // Bare ambiguous → None
        assert!(find_exact_model_reference_match("shared-id", &models).is_none());
        // Canonical disambiguates
        let m = find_exact_model_reference_match("openai/shared-id", &models);
        assert!(m.is_some());
        assert_eq!(m.unwrap().provider, model::types::Provider::OpenAI);
    }

    #[test]
    fn find_exact_returns_none_for_blank_input() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude")];
        assert!(find_exact_model_reference_match("", &models).is_none());
        assert!(find_exact_model_reference_match("   ", &models).is_none());
    }

    #[test]
    fn try_match_partial_prefers_alias_over_dated() {
        let models = vec![
            fake(
                model::types::Provider::Anthropic,
                "claude-sonnet-4-20250514",
            ),
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
        ];
        let m = try_match_model("sonnet", &models).expect("partial match");
        assert_eq!(m.id, "claude-sonnet-4", "alias should win over dated");
    }

    #[test]
    fn try_match_dated_picks_latest_when_no_alias() {
        let models = vec![
            fake(
                model::types::Provider::Anthropic,
                "claude-sonnet-4-20240101",
            ),
            fake(
                model::types::Provider::Anthropic,
                "claude-sonnet-4-20250514",
            ),
        ];
        let m = try_match_model("sonnet", &models).expect("partial match");
        assert_eq!(m.id, "claude-sonnet-4-20250514");
    }

    #[test]
    fn parse_full_returns_exact_match_with_no_thinking_level() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let res = parse_model_pattern_full(
            "claude-sonnet-4",
            &models,
            ParseModelPatternOptions::permissive(),
        );
        assert!(res.model.is_some());
        assert!(res.thinking_level.is_none());
        assert!(res.warning.is_none());
    }

    #[test]
    fn parse_full_extracts_thinking_level_from_suffix() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let res = parse_model_pattern_full(
            "claude-sonnet-4:high",
            &models,
            ParseModelPatternOptions::permissive(),
        );
        assert_eq!(
            res.model.as_ref().map(|m| m.id.as_str()),
            Some("claude-sonnet-4")
        );
        assert_eq!(res.thinking_level, Some(ThinkingLevel::High));
    }

    #[test]
    fn parse_full_invalid_suffix_warns_in_permissive_mode() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let res = parse_model_pattern_full(
            "claude-sonnet-4:bogus",
            &models,
            ParseModelPatternOptions::permissive(),
        );
        assert!(res.model.is_some(), "permissive mode should still resolve");
        assert!(res.thinking_level.is_none());
        assert!(
            res.warning
                .as_deref()
                .map(|w| w.contains("Invalid thinking level"))
                .unwrap_or(false),
            "warning must mention invalid thinking level: {:?}",
            res.warning
        );
    }

    #[test]
    fn parse_full_invalid_suffix_strict_returns_none() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let res = parse_model_pattern_full(
            "claude-sonnet-4:bogus",
            &models,
            ParseModelPatternOptions::strict(),
        );
        assert!(
            res.model.is_none(),
            "strict mode must not silently fall back"
        );
        assert!(res.thinking_level.is_none());
    }

    #[test]
    fn parse_full_handles_colon_in_id_with_thinking_level() {
        // OpenRouter-style ID containing a colon, plus a thinking suffix.
        let models = vec![fake(
            model::types::Provider::Openrouter,
            "openai/gpt-4o:extended",
        )];
        let res = parse_model_pattern_full(
            "openai/gpt-4o:extended:high",
            &models,
            ParseModelPatternOptions::permissive(),
        );
        assert_eq!(
            res.model.as_ref().map(|m| m.id.as_str()),
            Some("openai/gpt-4o:extended")
        );
        assert_eq!(res.thinking_level, Some(ThinkingLevel::High));
    }

    #[test]
    fn build_fallback_clones_provider_default() {
        let mut anthropic_default = fake(model::types::Provider::Anthropic, "claude-opus-4-7");
        anthropic_default.context_window = 999_999;
        let models = vec![
            anthropic_default.clone(),
            fake(model::types::Provider::Anthropic, "other-model"),
        ];
        let fallback =
            build_fallback_model_from_available("anthropic", "custom-id", &models).unwrap();
        assert_eq!(fallback.id, "custom-id");
        assert_eq!(fallback.name, "custom-id");
        // Cloned from default — should preserve overrides like context_window.
        assert_eq!(fallback.context_window, 999_999);
    }

    #[test]
    fn build_fallback_returns_none_for_unknown_provider() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude")];
        assert!(build_fallback_model_from_available("nope", "x", &models).is_none());
    }

    #[test]
    fn resolve_scope_dedupes_and_collects_warnings() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::OpenAI, "gpt-4o"),
        ];
        let patterns = vec![
            "claude-sonnet-4".to_string(),
            "claude-sonnet-4:high".to_string(),
            "no-such-model".to_string(),
        ];
        let res = resolve_model_scope(&patterns, &models);
        // Dedup: first pattern wins, second is dropped (same id).
        assert_eq!(res.models.len(), 1);
        assert_eq!(res.models[0].model.id, "claude-sonnet-4");
        // First pattern had no thinking level → carried through.
        assert!(res.models[0].thinking_level.is_none());
        // Third pattern produced a warning.
        assert_eq!(res.warnings.len(), 1);
        assert!(res.warnings[0].contains("no-such-model"));
    }

    #[test]
    fn resolve_scope_glob_matches_provider_id() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::Anthropic, "claude-opus-4"),
            fake(model::types::Provider::OpenAI, "gpt-4o"),
        ];
        let patterns = vec!["anthropic/*".to_string()];
        let res = resolve_model_scope(&patterns, &models);
        assert_eq!(res.models.len(), 2);
        assert!(
            res.models
                .iter()
                .all(|m| m.model.provider == model::types::Provider::Anthropic)
        );
    }

    #[test]
    fn resolve_scope_glob_with_thinking_suffix() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::Anthropic, "claude-opus-4"),
        ];
        let patterns = vec!["*sonnet*:high".to_string()];
        let res = resolve_model_scope(&patterns, &models);
        assert_eq!(res.models.len(), 1);
        assert_eq!(res.models[0].model.id, "claude-sonnet-4");
        assert_eq!(res.models[0].thinking_level, Some(ThinkingLevel::High));
    }

    #[test]
    fn resolve_scope_glob_warns_on_no_match() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let res = resolve_model_scope(&["*nope*".to_string()], &models);
        assert!(res.models.is_empty());
        assert_eq!(res.warnings.len(), 1);
    }

    #[test]
    fn resolve_cli_no_model_is_empty_result() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude")];
        let res = resolve_cli_model(None, None, &models);
        assert!(res.model.is_none());
        assert!(res.error.is_none());
        assert!(res.warning.is_none());
    }

    #[test]
    fn resolve_cli_unknown_provider_is_error() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude")];
        let res = resolve_cli_model(Some("nope"), Some("anything"), &models);
        assert!(res.model.is_none());
        assert!(res.error.is_some());
        assert!(res.error.unwrap().contains("Unknown provider"));
    }

    #[test]
    fn resolve_cli_provider_slash_model_infers_provider() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::OpenAI, "gpt-4o"),
        ];
        let res = resolve_cli_model(None, Some("anthropic/claude-sonnet-4"), &models);
        assert!(res.error.is_none());
        let m = res.model.expect("model resolved");
        assert_eq!(m.provider, model::types::Provider::Anthropic);
        assert_eq!(m.id, "claude-sonnet-4");
    }

    #[test]
    fn resolve_cli_falls_back_to_custom_id_for_known_provider() {
        // A registered Anthropic model exists, so custom-id construction is allowed.
        let models = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let res = resolve_cli_model(Some("anthropic"), Some("brand-new-future-id"), &models);
        assert!(res.error.is_none(), "should not error: {:?}", res.error);
        let m = res.model.expect("custom fallback model");
        assert_eq!(m.id, "brand-new-future-id");
        assert_eq!(m.provider, model::types::Provider::Anthropic);
        assert!(res.warning.is_some());
    }

    #[test]
    fn resolve_cli_strict_rejects_invalid_thinking_suffix_then_falls_back() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let res = resolve_cli_model(Some("anthropic"), Some("claude-opus-4-7:bogus"), &models);
        // parse_model_pattern_full(strict) returns None for ":bogus"; we then
        // build a custom-id fallback from the literal pattern.
        assert!(res.error.is_none());
        let m = res.model.expect("fallback");
        assert_eq!(m.id, "claude-opus-4-7:bogus");
    }

    #[test]
    fn resolve_cli_openrouter_style_id_with_slash_resolves_via_full_input() {
        // "openai/gpt-4o" looks like a provider, but "openai" is not the
        // provider here (model is on openrouter). The fallback path should
        // recover by matching the full string.
        let models = vec![fake(model::types::Provider::Openrouter, "openai/gpt-4o")];
        let res = resolve_cli_model(None, Some("openai/gpt-4o"), &models);
        assert!(res.error.is_none(), "expected resolution: {:?}", res.error);
        let m = res.model.expect("resolved");
        assert_eq!(m.id, "openai/gpt-4o");
        assert_eq!(m.provider, model::types::Provider::Openrouter);
    }

    #[test]
    fn resolve_cli_no_models_available_is_error() {
        let res = resolve_cli_model(None, Some("anything"), &[]);
        assert!(res.model.is_none());
        assert!(
            res.error
                .as_deref()
                .map(|e| e.contains("No models available"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn find_initial_cli_takes_priority() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let scoped = vec![ScopedModel {
            model: fake(model::types::Provider::OpenAI, "gpt-4o"),
            thinking_level: None,
        }];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: Some("anthropic"),
            cli_model: Some("claude-opus-4-7"),
            scoped_models: &scoped,
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: None,
            available_models: &models,
            all_models: &models,
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                assert_eq!(r.model.unwrap().id, "claude-opus-4-7");
            }
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    #[test]
    fn find_initial_cli_error_propagates() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: Some("nope"),
            cli_model: Some("x"),
            scoped_models: &[],
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: None,
            available_models: &models,
            all_models: &models,
        });
        assert!(matches!(outcome, FindInitialModelOutcome::CliError(_)));
    }

    #[test]
    fn find_initial_uses_first_scoped_model_when_not_continuing() {
        let scoped = vec![ScopedModel {
            model: fake(model::types::Provider::OpenAI, "gpt-4o"),
            thinking_level: Some(ThinkingLevel::High),
        }];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: None,
            cli_model: None,
            scoped_models: &scoped,
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: None,
            available_models: &[],
            all_models: &[],
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                assert_eq!(r.model.unwrap().id, "gpt-4o");
                assert_eq!(r.thinking_level, ThinkingLevel::High);
            }
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    #[test]
    fn find_initial_skips_scoped_when_continuing_and_falls_through_to_default() {
        let saved = fake(model::types::Provider::Anthropic, "claude-opus-4-7");
        let scoped = vec![ScopedModel {
            model: fake(model::types::Provider::OpenAI, "gpt-4o"),
            thinking_level: None,
        }];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: None,
            cli_model: None,
            scoped_models: &scoped,
            is_continuing: true,
            default_provider: Some("anthropic"),
            default_model_id: Some("claude-opus-4-7"),
            default_thinking_level: None,
            available_models: &[],
            all_models: &[saved],
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                assert_eq!(r.model.unwrap().id, "claude-opus-4-7");
            }
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    #[test]
    fn find_initial_falls_back_to_known_provider_default() {
        // Simulate: anthropic default is auth-configured; pick it over an
        // arbitrary first-available model.
        let available = vec![
            fake(model::types::Provider::OpenAI, "some-openai-model"),
            fake(model::types::Provider::Anthropic, "claude-opus-4-7"),
        ];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: None,
            cli_model: None,
            scoped_models: &[],
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: None,
            available_models: &available,
            all_models: &available,
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                assert_eq!(r.model.unwrap().id, "claude-opus-4-7");
            }
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    #[test]
    fn find_initial_returns_none_when_nothing_available() {
        let outcome = find_initial_model(FindInitialModelArgs::default());
        match outcome {
            FindInitialModelOutcome::Resolved(r) => assert!(r.model.is_none()),
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    #[test]
    fn restore_session_returns_saved_when_auth_present() {
        let saved = fake(model::types::Provider::Anthropic, "claude-opus-4-7");
        let all = vec![saved.clone()];
        let res =
            restore_model_from_session("anthropic", "claude-opus-4-7", None, &all, &all, |_| true);
        assert_eq!(res.model.as_ref().unwrap().id, "claude-opus-4-7");
        assert!(res.fallback_message.is_none());
    }

    #[test]
    fn restore_session_falls_back_to_current_when_auth_missing() {
        let saved = fake(model::types::Provider::Anthropic, "claude-opus-4-7");
        let curr = fake(model::types::Provider::OpenAI, "gpt-4o");
        let all = vec![saved.clone()];
        let res = restore_model_from_session(
            "anthropic",
            "claude-opus-4-7",
            Some(&curr),
            &[],
            &all,
            |_| false, // auth not configured
        );
        assert_eq!(res.model.unwrap().id, "gpt-4o");
        let msg = res.fallback_message.expect("fallback message expected");
        assert!(msg.contains("no auth configured"));
    }

    #[test]
    fn restore_session_falls_back_to_known_provider_default() {
        // Saved model gone; no current; available has an Anthropic default.
        let available = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let res = restore_model_from_session(
            "openai",
            "vanished-model",
            None,
            &available,
            &available,
            |_| true,
        );
        assert_eq!(res.model.unwrap().id, "claude-opus-4-7");
        let msg = res.fallback_message.unwrap();
        assert!(msg.contains("model no longer exists"));
    }

    #[test]
    fn restore_session_returns_none_when_no_models_available() {
        let res = restore_model_from_session("openai", "x", None, &[], &[], |_| true);
        assert!(res.model.is_none());
        assert!(res.fallback_message.is_none());
        assert!(!res.messages.is_empty());
    }
}
