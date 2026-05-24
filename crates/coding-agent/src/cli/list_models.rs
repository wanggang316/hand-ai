//! `--list-models` implementation.
//!
//! Resolves the available-models catalogue (filtered to providers whose
//! credentials are configured) and renders it as a six-column table
//! (`provider`, `model`, `context`, `max-out`, `thinking`, `images`).
//!
//! An optional search pattern narrows the list using a three-pass
//! strategy that avoids the false-positive avalanche of plain fuzzy
//! matching:
//!
//! 1. Exact provider-name match (case-insensitive) — `--list-models
//!    openai` returns OpenAI models, not `openrouter/*`.
//! 2. Case-insensitive substring on `<provider> <id>`.
//! 3. Fuzzy match as a last-resort "did you mean?" mode.

use model::Model;
use model::types::InputType;

use crate::core::auth_storage::AuthStorage;
use crate::core::model_registry::ModelRegistry;
use crate::core::model_resolver;

/// Resolve the catalogue shown by `--list-models`. Filters to providers
/// whose credentials are configured (env var or auth.json). Falls back
/// to the unfiltered catalogue when auth.json is unreadable so the
/// command never returns a misleading empty list.
pub fn list_models_for_cli(search: Option<&str>) -> Vec<Model> {
    let auth = match AuthStorage::new() {
        Ok(a) => a,
        Err(_) => return model_resolver::list_models(search),
    };
    let registry = ModelRegistry::create(auth);
    // Surface models.json load errors on stderr so users discover broken
    // configs instead of silently losing custom models or overrides.
    if let Some(err) = registry.error() {
        eprintln!("\x1b[33mWarning: {err}\x1b[0m");
    }
    let mut models = registry.available();
    if let Some(pattern) = search.filter(|s| !s.is_empty()) {
        models = filter_models_by_pattern(models, pattern);
    }
    models.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then_with(|| a.id.cmp(&b.id))
    });
    models
}

/// Apply the three-pass search filter (exact provider → substring →
/// fuzzy) to a candidate list. Extracted so it can be unit-tested
/// without an `AuthStorage`.
pub(crate) fn filter_models_by_pattern(models: Vec<Model>, pattern: &str) -> Vec<Model> {
    use hand_tui::fuzzy_filter;

    let needle = pattern.to_lowercase();

    let provider_exact: Vec<Model> = models
        .iter()
        .filter(|m| m.provider.as_str().eq_ignore_ascii_case(&needle))
        .cloned()
        .collect();
    if !provider_exact.is_empty() {
        return provider_exact;
    }

    let substring: Vec<Model> = models
        .iter()
        .filter(|m| {
            let haystack = format!("{} {}", m.provider.as_str(), m.id).to_lowercase();
            haystack.contains(&needle)
        })
        .cloned()
        .collect();
    if !substring.is_empty() {
        return substring;
    }

    let haystacks: Vec<String> = models
        .iter()
        .map(|m| format!("{} {}", m.provider.as_str(), m.id))
        .collect();
    let haystack_refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
    let matches = fuzzy_filter(pattern, &haystack_refs);
    matches.into_iter().map(|(i, _)| models[i].clone()).collect()
}

/// Format a token count as a short human-readable string.
///
/// `200_000 -> "200K"`, `1_000_000 -> "1M"`, `1_500_000 -> "1.5M"`.
fn format_token_count(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if (m.fract()).abs() < f64::EPSILON {
            format!("{}M", m as u64)
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if (k.fract()).abs() < f64::EPSILON {
            format!("{}K", k as u64)
        } else {
            format!("{k:.1}K")
        }
    } else {
        n.to_string()
    }
}

struct Row {
    provider: String,
    model: String,
    context: String,
    max_out: String,
    thinking: String,
    images: String,
}

impl Row {
    fn from_model(m: &Model) -> Self {
        Self {
            provider: m.provider.as_str().to_string(),
            model: m.id.clone(),
            context: format_token_count(m.context_window),
            max_out: format_token_count(m.max_tokens),
            thinking: if m.reasoning { "yes" } else { "no" }.into(),
            images: if m.input.contains(&InputType::Image) {
                "yes"
            } else {
                "no"
            }
            .into(),
        }
    }
}

struct ColumnWidths {
    provider: usize,
    model: usize,
    context: usize,
    max_out: usize,
    thinking: usize,
    images: usize,
}

fn column_widths(header: &Row, rows: &[Row]) -> ColumnWidths {
    let mut w = ColumnWidths {
        provider: header.provider.len(),
        model: header.model.len(),
        context: header.context.len(),
        max_out: header.max_out.len(),
        thinking: header.thinking.len(),
        images: header.images.len(),
    };
    for r in rows {
        w.provider = w.provider.max(r.provider.len());
        w.model = w.model.max(r.model.len());
        w.context = w.context.max(r.context.len());
        w.max_out = w.max_out.max(r.max_out.len());
        w.thinking = w.thinking.max(r.thinking.len());
        w.images = w.images.max(r.images.len());
    }
    w
}

/// Render the model catalogue as a six-column table. Header labels are
/// lowercase so the output stays stable for downstream diff/snapshot
/// harnesses.
pub fn print_models_table(models: &[Model]) {
    let header = Row {
        provider: "provider".into(),
        model: "model".into(),
        context: "context".into(),
        max_out: "max-out".into(),
        thinking: "thinking".into(),
        images: "images".into(),
    };
    let rows: Vec<Row> = models.iter().map(Row::from_model).collect();
    let w = column_widths(&header, &rows);
    let print = |r: &Row| {
        println!(
            "{:<pw$}  {:<mw$}  {:<cw$}  {:<ow$}  {:<tw$}  {:<iw$}",
            r.provider,
            r.model,
            r.context,
            r.max_out,
            r.thinking,
            r.images,
            pw = w.provider,
            mw = w.model,
            cw = w.context,
            ow = w.max_out,
            tw = w.thinking,
            iw = w.images,
        );
    };
    print(&header);
    for r in &rows {
        print(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{Api, Cost, Provider};

    fn mk_model(provider: Provider, id: &str) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            api: Api::OpenAICompletions,
            provider,
            base_url: "https://example.com".into(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 1_024,
            max_tokens: 256,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    #[test]
    fn formats_token_counts_below_thousand() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
    }

    #[test]
    fn formats_token_counts_in_thousands() {
        assert_eq!(format_token_count(1_000), "1K");
        assert_eq!(format_token_count(8_192), "8.2K");
        assert_eq!(format_token_count(200_000), "200K");
    }

    #[test]
    fn formats_token_counts_in_millions() {
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(1_500_000), "1.5M");
        assert_eq!(format_token_count(2_000_000), "2M");
    }

    #[test]
    fn row_from_model_renders_thinking_and_image_flags() {
        let mut m = mk_model(Provider::OpenAI, "gpt-x");
        m.reasoning = true;
        m.input = vec![InputType::Text, InputType::Image];
        m.context_window = 200_000;
        m.max_tokens = 8_192;
        let row = Row::from_model(&m);
        assert_eq!(row.provider, "openai");
        assert_eq!(row.model, "gpt-x");
        assert_eq!(row.context, "200K");
        assert_eq!(row.max_out, "8.2K");
        assert_eq!(row.thinking, "yes");
        assert_eq!(row.images, "yes");
    }

    #[test]
    fn row_without_image_input_reports_no() {
        let row = Row::from_model(&mk_model(Provider::OpenAI, "text-only"));
        assert_eq!(row.thinking, "no");
        assert_eq!(row.images, "no");
        assert_eq!(row.context, "1.0K");
        assert_eq!(row.max_out, "256");
    }

    #[test]
    fn column_widths_track_longest_value_per_column() {
        let header = Row {
            provider: "provider".into(),
            model: "model".into(),
            context: "context".into(),
            max_out: "max-out".into(),
            thinking: "thinking".into(),
            images: "images".into(),
        };
        let rows = vec![
            Row {
                provider: "anthropic".into(),
                model: "claude-3-haiku".into(),
                context: "200K".into(),
                max_out: "8K".into(),
                thinking: "no".into(),
                images: "yes".into(),
            },
            Row {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                context: "128K".into(),
                max_out: "16K".into(),
                thinking: "yes".into(),
                images: "yes".into(),
            },
        ];
        let widths = column_widths(&header, &rows);
        assert_eq!(widths.provider, "anthropic".len());
        assert_eq!(widths.model, "claude-3-haiku".len());
        // Header longer than any data row.
        assert_eq!(widths.context, "context".len());
        assert_eq!(widths.thinking, "thinking".len());
    }

    /// `--list-models openai` must return OpenAI models only. The
    /// pre-fix fuzzy implementation matched `o-p-e-n-a-i` scattered
    /// across `openrouter/*` ids and returned hundreds of false
    /// positives.
    #[test]
    fn pattern_exact_provider_match_wins_over_substring() {
        let models = vec![
            mk_model(Provider::OpenAI, "gpt-4o"),
            mk_model(Provider::Openrouter, "anthropic/claude-3-opus"),
            mk_model(Provider::Openrouter, "openai/gpt-4o"),
        ];
        let kept = filter_models_by_pattern(models, "openai");
        let ids: Vec<&str> = kept.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-4o"], "exact provider match must drop openrouter/*");
    }

    /// When no provider matches the needle exactly, fall through to a
    /// case-insensitive substring on `<provider> <id>`. A user typing
    /// a partial id like `claude` should get every claude-family model
    /// regardless of provider, but not unrelated entries.
    #[test]
    fn pattern_substring_match_catches_id_fragments() {
        let models = vec![
            mk_model(Provider::Anthropic, "claude-3-opus"),
            mk_model(Provider::Openrouter, "anthropic/claude-3-haiku"),
            mk_model(Provider::OpenAI, "gpt-4o"),
        ];
        let kept = filter_models_by_pattern(models, "claude");
        let ids: Vec<&str> = kept.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-3-opus", "anthropic/claude-3-haiku"]);
    }

    /// Fuzzy is the last-resort "did you mean…?" pass — it should
    /// only fire when both exact and substring return nothing.
    #[test]
    fn pattern_falls_through_to_fuzzy_only_when_strict_passes_miss() {
        // "gpO" matches neither provider exactly nor as a substring of
        // "openai gpt-4o", but fuzzy `g-p-O` finds it.
        let models = vec![mk_model(Provider::OpenAI, "gpt-4o")];
        let kept = filter_models_by_pattern(models, "gpO");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "gpt-4o");
    }

    /// A pattern that matches nothing at any tier returns an empty
    /// list (so the caller can emit "No models matching …").
    #[test]
    fn pattern_with_no_match_at_any_tier_returns_empty() {
        let models = vec![mk_model(Provider::OpenAI, "gpt-4o")];
        let kept = filter_models_by_pattern(models, "zzzzz-does-not-exist");
        assert!(kept.is_empty());
    }
}
