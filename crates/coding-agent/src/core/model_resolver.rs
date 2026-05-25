//! Model resolution — parses model patterns, thinking levels, and provider/id combos.
//!
//! Public helpers — `find_exact_model_reference_match`,
//! `parse_model_pattern_full`, `resolve_model_scope`, `resolve_cli_model`,
//! `find_initial_model`, `restore_model_from_session` — encode the
//! resolution rules. The legacy `resolve_model` / `parse_model_pattern`
//! signatures are preserved for the existing call sites in `main` and
//! `session_setup`.

use model::{Model, ThinkingLevel};

/// Default model id for each known provider, used as the seed for fallback
/// model construction and the priority order in `find_initial_model`.
///
/// Iteration order is the declaration order, so callers that scan for
/// a "preferred" model see a stable priority list.
pub fn default_model_per_provider() -> &'static [(&'static str, &'static str)] {
    &[
        ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
        ("anthropic", "claude-opus-4-7"),
        ("openai", "gpt-5.4"),
        ("azure-openai-responses", "gpt-5.4"),
        ("openai-codex", "gpt-5.5"),
        ("deepseek", "deepseek-v4-pro"),
        ("google", "gemini-2.5-pro"),
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
/// `-YYYYMMDD` (8 trailing digits after the last `-`).
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
        .or(pattern_provider.clone())
        .unwrap_or_else(|| "anthropic".to_string());

    // 0. OpenRouter-style ids naturally contain slashes (e.g.
    //    `deepseek/deepseek-r1`). If an explicit provider was given AND the
    //    raw model_id contained a slash, the model registry key on the
    //    explicit provider may be the full slashed id verbatim. Try that
    //    first so we don't downgrade to a fuzzy `contains` match. Strip a
    //    trailing `:thinking` suffix before the lookup.
    if provider.is_some() && pattern_provider.is_some() {
        let raw_no_thinking: &str = if let Some(idx) = model_id.rfind(':') {
            let suffix = &model_id[idx + 1..];
            if parse_thinking_level(suffix).is_some() {
                &model_id[..idx]
            } else {
                model_id
            }
        } else {
            model_id
        };
        if let Some(m) = model::get_model(&effective_provider, raw_no_thinking) {
            return ResolvedModel {
                model: m,
                thinking_level: thinking,
            };
        }
    }

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

    // 3. If the provider was INFERRED from a slash and we didn't find the
    //    model under that inferred provider, the slash may actually be part
    //    of a gateway-style id (e.g. `deepseek/deepseek-r1` registered under
    //    openrouter, not under `deepseek`). Try the raw input as an exact id
    //    across every provider before falling back to fuzzy contains-match,
    //    so we never silently divert to an unrelated provider. Strips a
    //    trailing `:thinking` suffix the same way step 0 does.
    if provider.is_none() && pattern_provider.is_some() {
        let raw_no_thinking: &str = if let Some(idx) = model_id.rfind(':') {
            let suffix = &model_id[idx + 1..];
            if parse_thinking_level(suffix).is_some() {
                &model_id[..idx]
            } else {
                model_id
            }
        } else {
            model_id
        };
        for prov_key in model::get_provider_keys() {
            if let Some(m) = model::get_model(&prov_key, raw_no_thinking) {
                return ResolvedModel {
                    model: m,
                    thinking_level: thinking,
                };
            }
        }
    }

    // 4. Cross-provider fuzzy fallback — only when no explicit --provider
    //    was given. With an explicit provider, drop straight to the
    //    build_fallback_model path so we don't silently route to a
    //    different provider the user has no credentials configured for:
    //    candidate filtering stays inside the requested provider and
    //    never crosses providers.
    if provider.is_none() {
        for prov_key in model::get_provider_keys() {
            let models = model::get_models(&prov_key);
            if let Some(m) = find_best_match(&pattern, &models) {
                return ResolvedModel {
                    model: m,
                    thinking_level: thinking,
                };
            }
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
///
/// Two-step resolution: parse the provider string into a
/// [`model::types::Provider`] enum (handles aliases below), then pick
/// the API protocol that provider uses.
/// Defaults to Anthropic only when the provider string is genuinely
/// unrecognised — *not* when it's a known provider we just hadn't enumerated
/// here, which was the bug the user hit with `--provider zai`.
fn build_fallback_model(provider: &str, model_id: &str) -> Model {
    use model::types::{Api, Provider};

    // Common alias normalisation so `--provider zhipu` resolves to `zai`,
    // `--provider gemini` resolves to `google`, etc.
    let normalised: &str = match provider {
        "zhipu" => "zai",
        "gemini" => "google",
        "kimi" => "moonshotai",
        "claude" => "anthropic",
        "deepseek-coder" => "deepseek",
        "bedrock" => "amazon-bedrock",
        "azure" => "azure-openai-responses",
        other => other,
    };

    // Resolve to a Provider enum, defaulting to Anthropic only for genuinely
    // unknown ids.
    let provider_enum = Provider::from_str(normalised).unwrap_or(Provider::Anthropic);

    // Pick the API protocol the provider speaks. Most OpenAI-compatible
    // providers route through `OpenAICompletions`; the rest have bespoke
    // protocols.
    let api = match provider_enum {
        Provider::Anthropic => Api::AnthropicMessages,
        Provider::Google => Api::GoogleGenerativeAi,
        Provider::GoogleGeminiCli => Api::GoogleGenerativeAi,
        Provider::GoogleAntigravity => Api::GoogleGenerativeAi,
        Provider::GoogleVertex => Api::GoogleGenerativeAi,
        Provider::AmazonBedrock => Api::BedrockConverseStream,
        Provider::AzureOpenAiResponses => Api::AzureOpenAiResponses,
        Provider::OpenAICodex => Api::OpenAICodexResponses,
        // Everything else uses OpenAI-compatible Completions: openai,
        // openrouter, xai, groq, cerebras, vercel-ai-gateway, zai, mistral,
        // minimax(-cn), huggingface, opencode(-go), kimi-coding,
        // cloudflare-*, fireworks, moonshotai(-cn), xiaomi*, deepseek,
        // github-copilot.
        _ => Api::OpenAICompletions,
    };

    Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        api,
        provider: provider_enum,
        base_url: default_base_url_for(provider_enum),
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

/// Resolve a sensible default base URL for an unrecognised model.
///
/// The OAI-compatible providers each pick a `<PROVIDER>_BASE_URL` env
/// var (matching the secrets.env convention), falling back to the
/// documented public endpoint.
fn default_base_url_for(provider: model::types::Provider) -> String {
    use model::types::Provider;
    // Candidate env var names per provider. First non-empty wins. The
    // aliases (ZHIPU_BASE_URL for zai etc.) match the conventions used in
    // the user community's secrets.env files.
    let env_keys: &[&str] = match provider {
        Provider::Anthropic => &["ANTHROPIC_BASE_URL"],
        Provider::Zai => &["ZAI_BASE_URL", "ZHIPU_BASE_URL"],
        Provider::Deepseek => &["DEEPSEEK_BASE_URL"],
        Provider::KimiCoding => &["KIMI_BASE_URL"],
        Provider::Moonshotai => &["KIMI_BASE_URL"],
        Provider::Minimax | Provider::MinimaxCn => &["MM_BASE_URL"],
        Provider::Openrouter => &["OPENROUTER_BASE_URL"],
        Provider::Google => &["GEMINI_BASE_URL"],
        Provider::OpenAI => &["OPENAI_BASE_URL"],
        Provider::Xai => &["XAI_BASE_URL"],
        Provider::Groq => &["GROQ_BASE_URL"],
        Provider::Cerebras => &["CEREBRAS_BASE_URL"],
        Provider::Mistral => &["MISTRAL_BASE_URL"],
        _ => return String::new(),
    };
    for key in env_keys {
        if let Ok(url) = std::env::var(key)
            && !url.is_empty()
        {
            // Trim trailing slashes — provider implementations append the
            // route path (e.g. `/chat/completions`) and a doubled slash
            // returns 404 on OpenRouter.
            return url.trim_end_matches('/').to_string();
        }
    }
    // Public-endpoint fallbacks for providers that have a stable URL.
    // Stored WITHOUT trailing slash to match the provider format strings.
    match provider {
        Provider::Zai => "https://open.bigmodel.cn/api/paas/v4".to_string(),
        Provider::Deepseek => "https://api.deepseek.com/v1".to_string(),
        Provider::Openrouter => "https://openrouter.ai/api/v1".to_string(),
        Provider::Google => "https://generativelanguage.googleapis.com/v1beta".to_string(),
        Provider::OpenAI => "https://api.openai.com/v1".to_string(),
        Provider::Xai => "https://api.x.ai/v1".to_string(),
        Provider::Groq => "https://api.groq.com/openai/v1".to_string(),
        _ => String::new(),
    }
}

/// Infer the provider for a bare model id by scanning the catalogue.
///
/// Used when the user passes `--model <id>` without `--provider` and
/// the id contains no slash (slashed ids drive routing through
/// `resolve_model(None, "a/b")` already). The pattern's
/// `:thinking-level` suffix and any `provider/` prefix are stripped
/// before lookup.
///
/// When exactly one provider hosts the id (e.g. `claude-opus-4-7`
/// under `anthropic`), that provider is returned. When multiple
/// providers host the same id (e.g. `gemini-2.5-flash` exists under
/// `google`, `google-vertex`, and `google-gemini-cli`), the `priority`
/// list breaks the tie deterministically — the first priority entry
/// that hosts the id wins. Pass an empty slice for a strict-only
/// lookup that returns `None` on any ambiguity.
///
/// Returns the provider key (case as registered in the catalogue), or
/// `None` when nothing matches or the ambiguity can't be resolved
/// from the priority list — the caller falls back to its own default.
pub fn infer_provider_for_model_id(
    model_pattern: &str,
    priority: &[&str],
) -> Option<String> {
    let (pattern_provider, bare_id, _thinking) = parse_model_pattern(model_pattern);
    // Slashed ids drive their own routing — don't override that here.
    if pattern_provider.is_some() {
        return None;
    }
    if bare_id.is_empty() {
        return None;
    }
    let needle = bare_id.to_lowercase();
    let hosts: Vec<String> = model::get_provider_keys()
        .into_iter()
        .filter(|p| {
            model::get_models(p)
                .iter()
                .any(|m| m.id.to_lowercase() == needle)
        })
        .collect();
    match hosts.len() {
        0 => None,
        1 => hosts.into_iter().next(),
        _ => priority
            .iter()
            .find(|p| hosts.iter().any(|h| h.eq_ignore_ascii_case(p)))
            .map(|p| (*p).to_string()),
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
    // An empty pattern would substring-match every id (`contains("")`
    // is always true). pi returns null in that case; replicate that so
    // a bug-shaped empty `--model ""` doesn't silently pick the first
    // catalog row.
    if model_pattern.is_empty() {
        return None;
    }

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

/// Whether `level` is one of the canonical thinking-level literals.
///
/// Strict — accepts only the canonical literals (`off`, `minimal`,
/// `low`, `medium`, `high`, `xhigh`). Use this for pattern parsing
/// where the suffix must be one of the documented values, distinct
/// from the more permissive [`parse_thinking_level`] which accepts
/// aliases like `min`/`med`/`max`/`none`.
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
pub const DEFAULT_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// Inputs to [`find_initial_model`].
///
/// `available_models` is the auth-configured catalog; `all_models` is
/// the full catalog used for CLI resolution (so `--api-key` can be
/// used for first-time setup).
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
        "openrouter" => "anthropic/claude-sonnet-4.5",
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

    /// Regression: OpenRouter-style model IDs naturally contain slashes
    /// (e.g. `deepseek/deepseek-r1`). Hand previously stripped the segment
    /// before the slash and looked the pattern up as `deepseek-r1`, which
    /// fell through to a fuzzy `contains` match that returned the wrong
    /// model — e.g. `tngtech/deepseek-r1t2-chimera`. The resolver now
    /// matches the full slashed id first to avoid that footgun.
    #[test]
    fn resolve_model_preserves_slashed_id_under_explicit_provider() {
        let result = resolve_model(Some("openrouter"), "deepseek/deepseek-r1");
        assert_eq!(
            result.model.id, "deepseek/deepseek-r1",
            "must match the exact slashed id on openrouter, got {}",
            result.model.id
        );
    }

    /// Same shape but with a thinking suffix: `deepseek/deepseek-r1:high`
    /// must still resolve to the exact `deepseek/deepseek-r1` model and
    /// surface `ThinkingLevel::High`.
    #[test]
    fn resolve_model_preserves_slashed_id_with_thinking_suffix() {
        let result = resolve_model(Some("openrouter"), "deepseek/deepseek-r1:high");
        assert_eq!(result.model.id, "deepseek/deepseek-r1");
        assert_eq!(result.thinking_level, Some(ThinkingLevel::High));
    }

    /// OpenRouter exposes OpenAI's GPT family under `openai/<id>`. Hand
    /// must route `openai/gpt-3.5-turbo` to the OpenRouter model with that
    /// id, not partial-match its way to a similarly-named OpenAI provider
    /// model that wouldn't actually be served via OpenRouter credentials.
    #[test]
    fn resolve_model_routes_openai_slug_to_openrouter_when_provider_explicit() {
        let result = resolve_model(Some("openrouter"), "openai/gpt-3.5-turbo");
        assert_eq!(result.model.id, "openai/gpt-3.5-turbo");
        assert_eq!(result.model.provider.as_str(), "openrouter");
    }

    /// Native deepseek provider (`https://api.deepseek.com`) must resolve
    /// when requested explicitly. Lock the registration so a future
    /// `models.json` regeneration doesn't accidentally drop the entries.
    #[test]
    fn resolve_model_finds_native_deepseek_v4_flash() {
        let result = resolve_model(Some("deepseek"), "deepseek-v4-flash");
        assert_eq!(result.model.id, "deepseek-v4-flash");
        assert_eq!(result.model.provider.as_str(), "deepseek");
        assert_eq!(result.model.base_url, "https://api.deepseek.com");
        assert!(result.model.reasoning);
    }

    #[test]
    fn resolve_model_finds_native_deepseek_v4_pro() {
        let result = resolve_model(Some("deepseek"), "deepseek-v4-pro");
        assert_eq!(result.model.id, "deepseek-v4-pro");
        assert_eq!(result.model.provider.as_str(), "deepseek");
    }

    /// Regression: when no --provider is given and the model pattern contains
    /// a slash that looks like a provider prefix (e.g. `deepseek/deepseek-r1`),
    /// the inferred-provider lookup may not have the model registered under
    /// that key. Hand previously fell through to a fuzzy `contains` match
    /// across ALL providers — including ones the user has no credentials for
    /// (Bedrock, Vertex) — and silently picked one.
    ///
    /// The resolver falls back to matching the full slashed input as
    /// an id across every provider in the registry, finding e.g.
    /// openrouter's `deepseek/deepseek-r1` and routing there.
    #[test]
    fn resolve_model_no_provider_with_slashed_id_finds_openrouter_match() {
        let result = resolve_model(None, "deepseek/deepseek-r1");
        assert_eq!(result.model.id, "deepseek/deepseek-r1");
        assert_eq!(
            result.model.provider.as_str(),
            "openrouter",
            "expected openrouter, got {}",
            result.model.provider.as_str()
        );
    }

    /// Issue #18: `--model openrouter/openai/gpt-4o-mini` (with the
    /// provider name as the first slash segment) must route to the
    /// `openrouter` provider and keep the full `openai/gpt-4o-mini` as
    /// the model id. Before the session-setup fix the slash was split
    /// only on the first `/`, so the pattern resolved as provider
    /// "openrouter" with model id "openai/gpt-4o-mini" — that part is
    /// correct here, the test pins it.
    #[test]
    fn resolve_model_no_provider_with_provider_prefix_three_segments() {
        let result = resolve_model(None, "openrouter/openai/gpt-4o-mini");
        assert_eq!(result.model.id, "openai/gpt-4o-mini");
        assert_eq!(
            result.model.provider.as_str(),
            "openrouter",
            "expected openrouter, got {}",
            result.model.provider.as_str()
        );
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

    /// Mirrors the auth-priority list session_setup uses for tie-
    /// breaking. Kept here so tests don't depend on session_setup's
    /// private constant.
    const TEST_PRIORITY: &[&str] = &[
        "anthropic",
        "openrouter",
        "google",
        "openai",
        "vercel-ai-gateway",
        "zai",
        "deepseek",
        "groq",
        "cerebras",
        "xai",
        "mistral",
        "kimi-coding",
        "huggingface",
        "fireworks",
        "minimax",
    ];

    /// `--model gemini-2.5-flash` (no `--provider`) must land on
    /// `google`, not on the historical "anthropic" fallback. Pinned
    /// against the regression in issue #10 where users hit
    /// `No API key found for Anthropic` when only their Google key
    /// was configured. `gemini-2.5-flash` is also hosted under
    /// `google-vertex` and `google-gemini-cli` in the catalogue, so
    /// this also exercises priority-based tie-breaking.
    #[test]
    fn infer_provider_routes_bare_gemini_id_to_google() {
        assert_eq!(
            infer_provider_for_model_id("gemini-2.5-flash", TEST_PRIORITY).as_deref(),
            Some("google")
        );
    }

    /// A `:thinking-level` suffix on the bare id must not block
    /// inference — strip it before catalogue lookup.
    #[test]
    fn infer_provider_ignores_thinking_suffix() {
        assert_eq!(
            infer_provider_for_model_id("gemini-2.5-flash:high", TEST_PRIORITY).as_deref(),
            Some("google")
        );
    }

    /// Slashed `provider/id` patterns are out-of-scope for inference;
    /// the slash already drives routing via `resolve_model(None, …)`.
    /// Return None so the caller defers to that path.
    #[test]
    fn infer_provider_returns_none_for_slashed_id() {
        assert_eq!(
            infer_provider_for_model_id("openai/gpt-4o", TEST_PRIORITY),
            None
        );
    }

    /// An id that no provider hosts yields None — caller falls back
    /// to its default (auto-pick from configured providers, or
    /// anthropic).
    #[test]
    fn infer_provider_returns_none_for_unknown_id() {
        assert_eq!(
            infer_provider_for_model_id(
                "definitely-not-a-real-model-zzzzzz",
                TEST_PRIORITY
            ),
            None
        );
    }

    /// Empty pattern (`--model ""`) must not match — would otherwise
    /// lowercase to "" and match every id.
    #[test]
    fn infer_provider_returns_none_for_empty_pattern() {
        assert_eq!(infer_provider_for_model_id("", TEST_PRIORITY), None);
    }

    /// Strict mode: an empty priority list returns None whenever the
    /// id is hosted by more than one provider. The caller can use
    /// this when ambiguity itself should defer to a different path.
    #[test]
    fn infer_provider_empty_priority_returns_none_on_ambiguity() {
        // `gemini-2.5-flash` is hosted by google / google-vertex /
        // google-gemini-cli, so with no priority hints there's no
        // single winner.
        assert_eq!(
            infer_provider_for_model_id("gemini-2.5-flash", &[]),
            None
        );
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

    /// Lockstep parity with upstream pi's `defaultModelPerProvider`
    /// snapshot. The default-model-per-provider map shifts each time
    /// a vendor pushes a new GA release; user scripts that omit
    /// `--model` rely on this map to land on the right model.
    /// Drifting from pi would silently route hand to an older model
    /// for the same `hand --provider X` invocation.
    ///
    /// The pi snapshot at the time of the 2026-05-16 lockstep:
    ///   openai → gpt-5.4
    ///   openai-codex → gpt-5.5
    ///   zai → glm-5.1
    ///   minimax → MiniMax-M2.7
    ///   minimax-cn → MiniMax-M2.7
    ///   cerebras → zai-glm-4.7
    ///   vercel-ai-gateway → zai/glm-5.1
    ///
    /// When pi pushes an update, refresh this test in the same commit
    /// that updates `default_model_per_provider()` so the snapshot
    /// stays a single source of truth.
    #[test]
    fn default_model_per_provider_matches_pi_snapshot() {
        let map: std::collections::HashMap<&str, &str> =
            default_model_per_provider().iter().copied().collect();
        assert_eq!(map.get("openai"), Some(&"gpt-5.4"));
        assert_eq!(map.get("openai-codex"), Some(&"gpt-5.5"));
        assert_eq!(map.get("zai"), Some(&"glm-5.1"));
        assert_eq!(map.get("minimax"), Some(&"MiniMax-M2.7"));
        assert_eq!(map.get("minimax-cn"), Some(&"MiniMax-M2.7"));
        assert_eq!(map.get("cerebras"), Some(&"zai-glm-4.7"));
        assert_eq!(map.get("vercel-ai-gateway"), Some(&"zai/glm-5.1"));
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

    // ---------- UC-mr pending closures ----------

    /// UC-mr-006 — every canonical thinking-level literal accepted by
    /// the strict parser (`off`, `minimal`, `low`, `medium`, `high`,
    /// `xhigh`) resolves through `parse_model_pattern_full` against an
    /// exact model id; the returned `thinking_level` matches the
    /// literal; no warning is emitted.
    #[test]
    fn parse_full_resolves_every_thinking_level_keyword() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let cases = [
            ("off", ThinkingLevel::Minimal),
            ("minimal", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("xhigh", ThinkingLevel::Xhigh),
        ];
        for (keyword, expected) in cases {
            let pat = format!("claude-sonnet-4:{keyword}");
            let res =
                parse_model_pattern_full(&pat, &models, ParseModelPatternOptions::permissive());
            assert_eq!(
                res.model.as_ref().map(|m| m.id.as_str()),
                Some("claude-sonnet-4"),
                "model must resolve for {keyword}"
            );
            assert_eq!(
                res.thinking_level,
                Some(expected),
                "{keyword} should map to {expected:?}"
            );
            assert!(
                res.warning.is_none(),
                "{keyword} is a valid keyword — no warning, got {:?}",
                res.warning
            );
        }
    }

    /// UC-mr-016 — an empty pattern resolves to `None` rather than
    /// returning the first model in the catalog. An empty `--model`
    /// argument is a programming error from the caller, not a request
    /// for "any model".
    #[test]
    fn parse_full_empty_pattern_returns_none() {
        let models = vec![
            fake(model::types::Provider::Anthropic, "claude-sonnet-4"),
            fake(model::types::Provider::OpenAI, "gpt-4o"),
        ];
        let res = parse_model_pattern_full("", &models, ParseModelPatternOptions::permissive());
        assert!(
            res.model.is_none(),
            "empty pattern must not silently pick a model, got {:?}",
            res.model.as_ref().map(|m| &m.id)
        );
        assert!(res.thinking_level.is_none());
        assert!(res.warning.is_none());
    }

    /// UC-mr-017 — a trailing colon (`claude-sonnet-4:`) means the
    /// suffix is empty. Empty is NOT a valid thinking level; in
    /// permissive mode the recursion still resolves the bare prefix
    /// but a warning is surfaced; in strict mode the call returns
    /// `model: None`.
    #[test]
    fn parse_full_trailing_colon_empty_suffix_warns_permissive() {
        let models = vec![fake(model::types::Provider::Anthropic, "claude-sonnet-4")];
        let permissive = parse_model_pattern_full(
            "claude-sonnet-4:",
            &models,
            ParseModelPatternOptions::permissive(),
        );
        assert!(
            permissive.model.is_some(),
            "permissive mode falls back to the bare model: {permissive:?}"
        );
        assert!(permissive.thinking_level.is_none());
        assert!(
            permissive
                .warning
                .as_deref()
                .map(|w| w.contains("Invalid thinking level"))
                .unwrap_or(false),
            "warning must flag the empty suffix, got: {:?}",
            permissive.warning
        );

        let strict = parse_model_pattern_full(
            "claude-sonnet-4:",
            &models,
            ParseModelPatternOptions::strict(),
        );
        assert!(strict.model.is_none(), "strict mode rejects empty suffix");
    }

    /// UC-mr-023 — when an explicit provider is given and the model id
    /// is custom (not in the registry), the fallback model carries the
    /// id verbatim — no provider prefix is glued on. e.g. `--provider
    /// openai --model my-fine-tune` resolves to a model whose `id` is
    /// `my-fine-tune`, NOT `openai/my-fine-tune`.
    #[test]
    fn resolve_model_explicit_provider_custom_id_keeps_raw_id() {
        let resolved = resolve_model(Some("anthropic"), "totally-custom-not-in-registry");
        assert_eq!(
            resolved.model.id, "totally-custom-not-in-registry",
            "raw id must be preserved verbatim under explicit provider"
        );
        assert_eq!(
            resolved.model.provider.as_str(),
            "anthropic",
            "explicit provider must stick"
        );
        // No double-prefix.
        assert!(
            !resolved.model.id.starts_with("anthropic/"),
            "id must not gain a provider prefix"
        );
    }

    /// UC-mr-030 — `find_initial_model` accepts an explicit custom id
    /// supplied via CLI even when the registry does not contain it.
    /// The resulting model carries the raw id under the requested
    /// provider (custom fine-tunes / private models).
    #[test]
    fn find_initial_accepts_explicit_custom_id_via_cli() {
        let available = vec![fake(model::types::Provider::Anthropic, "claude-opus-4-7")];
        let outcome = find_initial_model(FindInitialModelArgs {
            cli_provider: Some("anthropic"),
            cli_model: Some("my-custom-fine-tune"),
            available_models: &available,
            all_models: &available,
            ..Default::default()
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                let m = r.model.expect("model must resolve under explicit provider");
                assert_eq!(m.id, "my-custom-fine-tune");
                assert_eq!(m.provider.as_str(), "anthropic");
            }
            FindInitialModelOutcome::CliError(e) => {
                panic!("custom id under explicit provider must not error: {e}")
            }
        }
    }

    /// UC-mr-031 — when no CLI / scoped / settings default exists but
    /// the auth-configured catalog includes the AI-Gateway provider's
    /// default, `find_initial_model` picks it up via the
    /// `default_model_per_provider` table.
    #[test]
    fn find_initial_picks_ai_gateway_default_when_available() {
        // Build the catalog so `vercel-ai-gateway` is present with its
        // pi-snapshotted default model. The known-default lookup loops
        // over `default_model_per_provider()` and returns the first
        // matching available row.
        let gateway_default = default_model_per_provider()
            .iter()
            .find(|(p, _)| *p == "vercel-ai-gateway")
            .map(|(_, m)| *m)
            .expect("ai-gateway entry exists in pi snapshot");
        let mut row = fake(model::types::Provider::Anthropic, gateway_default);
        // Force provider field to vercel-ai-gateway via a custom build.
        row.provider = model::types::Provider::from_str("vercel-ai-gateway")
            .unwrap_or(model::types::Provider::Anthropic);
        let available = vec![row.clone()];
        let outcome = find_initial_model(FindInitialModelArgs {
            available_models: &available,
            all_models: &available,
            ..Default::default()
        });
        match outcome {
            FindInitialModelOutcome::Resolved(r) => {
                let m = r.model.expect("resolved to a model");
                assert_eq!(m.id, gateway_default);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }
}
