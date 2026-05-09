//! Print the auth-filtered model catalog with optional fuzzy search.
//!
//! TS reference: `cli/list-models.ts`. The bare `model` crate already ships
//! its own `model-cli list-models` for the unfiltered catalog; this helper
//! is the coding-agent-side enhanced view that:
//!
//! - filters by `ModelRegistry::available()` (drops models whose provider
//!   has no configured auth);
//! - surfaces any registry load error via `ModelRegistry::error()`;
//! - supports an optional fuzzy search pattern via [`hand_tui::fuzzy_filter`];
//! - renders an aligned table with `provider`, `model`, `context`, `max-out`,
//!   `thinking`, `images` columns.
//!
//! Output is written to `stdout` for the table and `stderr` for the load-
//! error warning, using ANSI escape codes (yellow warning, no other colour),
//! matching the rest of `coding-agent`'s CLI output style.

use std::path::Path;

use hand_tui::fuzzy_filter;
use model::types::{InputType, Model};

use crate::core::auth_guidance::no_models_available_message;
use crate::core::model_registry::ModelRegistry;

/// Format a token count as a short human-readable string.
///
/// `200_000 -> "200K"`, `1_000_000 -> "1M"`, `1_500_000 -> "1.5M"`.
fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        let millions = count as f64 / 1_000_000.0;
        if (millions.fract()).abs() < f64::EPSILON {
            format!("{}M", millions as u64)
        } else {
            format!("{:.1}M", millions)
        }
    } else if count >= 1_000 {
        let thousands = count as f64 / 1_000.0;
        if (thousands.fract()).abs() < f64::EPSILON {
            format!("{}K", thousands as u64)
        } else {
            format!("{:.1}K", thousands)
        }
    } else {
        count.to_string()
    }
}

/// One row in the rendered table.
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
            thinking: if m.reasoning { "yes" } else { "no" }.to_string(),
            images: if m.input.contains(&InputType::Image) {
                "yes"
            } else {
                "no"
            }
            .to_string(),
        }
    }
}

/// Print the model catalog. Returns the number of rows actually printed
/// (zero when the registry is empty or the search produces no matches).
///
/// `docs_path` is forwarded to [`no_models_available_message`] when the
/// registry yields nothing — the helper mirrors `auth-guidance` by taking
/// the docs directory as an explicit parameter rather than embedding a
/// filesystem assumption.
pub fn list_models(
    registry: &ModelRegistry,
    search_pattern: Option<&str>,
    docs_path: &Path,
) -> usize {
    if let Some(err) = registry.error() {
        // Yellow warning to stderr, matching TS `chalk.yellow(...)`.
        eprintln!("\x1b[33mWarning: errors loading models.json:\n{err}\x1b[0m");
    }

    let models = registry.available();
    if models.is_empty() {
        println!("{}", no_models_available_message(docs_path));
        return 0;
    }

    let mut filtered: Vec<Model> = match search_pattern {
        Some(pattern) if !pattern.is_empty() => {
            // Build the haystack `"<provider> <id>"` exactly as TS does so
            // search behaviour matches.
            let haystacks: Vec<String> = models
                .iter()
                .map(|m| format!("{} {}", m.provider.as_str(), m.id))
                .collect();
            let haystack_refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
            let matches = fuzzy_filter(pattern, &haystack_refs);
            matches
                .into_iter()
                .map(|(i, _)| models[i].clone())
                .collect()
        }
        _ => models,
    };

    if filtered.is_empty() {
        // The pattern is guaranteed `Some(non-empty)` here because the
        // empty/None branch returns the full list above.
        let pattern = search_pattern.unwrap_or("");
        println!("No models matching \"{pattern}\"");
        return 0;
    }

    // Sort by provider, then by id (TS `localeCompare`; the ASCII subset
    // we care about agrees with byte ordering).
    filtered.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then_with(|| a.id.cmp(&b.id))
    });

    let rows: Vec<Row> = filtered.iter().map(Row::from_model).collect();

    // Header labels (lowercase, matching TS).
    let header = Row {
        provider: "provider".to_string(),
        model: "model".to_string(),
        context: "context".to_string(),
        max_out: "max-out".to_string(),
        thinking: "thinking".to_string(),
        images: "images".to_string(),
    };

    let widths = column_widths(&header, &rows);
    print_row(&header, &widths);
    for row in &rows {
        print_row(row, &widths);
    }

    rows.len()
}

struct Widths {
    provider: usize,
    model: usize,
    context: usize,
    max_out: usize,
    thinking: usize,
    images: usize,
}

fn column_widths(header: &Row, rows: &[Row]) -> Widths {
    let mut w = Widths {
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

fn print_row(row: &Row, widths: &Widths) {
    println!(
        "{:<pw$}  {:<mw$}  {:<cw$}  {:<ow$}  {:<tw$}  {:<iw$}",
        row.provider,
        row.model,
        row.context,
        row.max_out,
        row.thinking,
        row.images,
        pw = widths.provider,
        mw = widths.model,
        cw = widths.context,
        ow = widths.max_out,
        tw = widths.thinking,
        iw = widths.images,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let m = Model {
            id: "gpt-x".into(),
            name: "GPT X".into(),
            api: model::types::Api::OpenAICompletions,
            provider: model::types::Provider::OpenAI,
            base_url: "https://example.com".into(),
            reasoning: true,
            input: vec![InputType::Text, InputType::Image],
            cost: model::types::Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 8_192,
            headers: None,
            compat: None,
            thinking_level_map: None,
        };
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
        let m = Model {
            id: "text-only".into(),
            name: "Text Only".into(),
            api: model::types::Api::OpenAICompletions,
            provider: model::types::Provider::OpenAI,
            base_url: "https://example.com".into(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: model::types::Cost {
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
        };
        let row = Row::from_model(&m);
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
}
