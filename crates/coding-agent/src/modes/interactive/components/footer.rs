//! Two- or three-line footer summarising session state.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/footer.ts`.
//!
//! pi-mono's footer reads directly from `AgentSession`, the model registry,
//! and the [`crate::core::footer_data_provider::FooterDataProvider`]. To keep
//! this Phase-2 port decoupled from those still-evolving APIs (and to stay
//! within the brief's scope rule that components only depend on
//! `model::Message`, `hand_agent`, `hand_tui`), the renderer accepts a
//! plain-data [`FooterViewModel`]. The driver port (queued) is responsible
//! for populating the view-model from session state.
//!
//! Theming caveat: pi-mono reads `dim`, `error`, `warning` slots from the
//! coding-agent theme. Until the theme port lands (see parent module docs)
//! we hardcode ANSI defaults: dim is `\x1b[2m`, warning is yellow,
//! error is red.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::Component;
use hand_tui::utils::{truncate_to_width_with, visible_width};

/// ANSI dim SGR.
const DIM: &str = "\x1b[2m";
/// ANSI yellow (warning).
const WARNING_FG: &str = "\x1b[33m";
/// ANSI red (error).
const ERROR_FG: &str = "\x1b[31m";
/// ANSI reset.
const RESET: &str = "\x1b[0m";

/// Aggregated token / cost statistics shown on the second footer line.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenUsageSummary {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    /// True when the active model is being billed via OAuth subscription
    /// (renders the `(sub)` indicator).
    pub using_subscription: bool,
}

/// Plain-data view-model populated by the driver and consumed by
/// [`FooterComponent`].
#[derive(Debug, Default, Clone)]
pub struct FooterViewModel {
    /// Working directory; the renderer applies the `~` substitution itself
    /// when [`Self::home_dir`] is supplied.
    pub cwd: String,
    /// Optional home directory for the `~` substitution. Pass `None` to
    /// disable.
    pub home_dir: Option<String>,
    /// Optional git branch shown after the cwd.
    pub git_branch: Option<String>,
    /// Optional human session label shown after a `•` separator.
    pub session_name: Option<String>,
    /// Token / cost stats. All-zero values render no stats segments.
    pub usage: TokenUsageSummary,
    /// Active model id; falls back to `"no-model"` when empty.
    pub model_id: String,
    /// Active model provider, used for the `(provider)` prefix when more
    /// than one provider is configured.
    pub model_provider: String,
    /// Active model context window in tokens (0 ⇒ unknown).
    pub context_window: u64,
    /// Context utilisation as a percent, or `None` if not yet computable.
    pub context_percent: Option<f64>,
    /// Whether the auto-compact indicator should appear.
    pub auto_compact_enabled: bool,
    /// Whether the active model exposes a reasoning toggle (drives the
    /// `thinking …` segment).
    pub has_reasoning: bool,
    /// Free-form thinking-level label (`"off"`, `"low"`, `"medium"`, …).
    pub thinking_level: String,
    /// Number of providers configured. > 1 enables the `(provider)` prefix.
    pub available_provider_count: usize,
    /// Extension status lines, sorted alphabetically by key by the caller
    /// before being concatenated for display.
    pub extension_statuses: Vec<(String, String)>,
}

/// Footer component.
pub struct FooterComponent {
    view: FooterViewModel,
}

impl FooterComponent {
    pub fn new(view: FooterViewModel) -> Self {
        Self { view }
    }

    /// Replace the view-model.
    pub fn set_view(&mut self, view: FooterViewModel) {
        self.view = view;
    }

    /// Mutable access for partial updates from the driver.
    pub fn view_mut(&mut self) -> &mut FooterViewModel {
        &mut self.view
    }
}

impl Component for FooterComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let v = &self.view;

        // 1. PWD line.
        let mut pwd = v.cwd.clone();
        if let Some(home) = &v.home_dir
            && !home.is_empty()
            && pwd.starts_with(home)
        {
            pwd = format!("~{}", &pwd[home.len()..]);
        }
        if let Some(branch) = &v.git_branch {
            pwd = format!("{pwd} ({branch})");
        }
        if let Some(name) = &v.session_name {
            pwd = format!("{pwd} • {name}");
        }
        let pwd_line = truncate_to_width_with(
            &format!("{DIM}{pwd}{RESET}"),
            width as usize,
            &format!("{DIM}...{RESET}"),
            false,
        );

        // 2. Stats line — left-aligned token/cost/context, right-aligned model.
        let mut stats_parts: Vec<String> = Vec::new();
        if v.usage.input > 0 {
            stats_parts.push(format!("↑{}", format_tokens(v.usage.input)));
        }
        if v.usage.output > 0 {
            stats_parts.push(format!("↓{}", format_tokens(v.usage.output)));
        }
        if v.usage.cache_read > 0 {
            stats_parts.push(format!("R{}", format_tokens(v.usage.cache_read)));
        }
        if v.usage.cache_write > 0 {
            stats_parts.push(format!("W{}", format_tokens(v.usage.cache_write)));
        }
        if v.usage.cost_usd > 0.0 || v.usage.using_subscription {
            stats_parts.push(format!(
                "${:.3}{}",
                v.usage.cost_usd,
                if v.usage.using_subscription {
                    " (sub)"
                } else {
                    ""
                }
            ));
        }

        // Context-percent segment with auto-compact indicator. Color depends
        // on utilisation, not auto-compact state.
        let auto_indicator = if v.auto_compact_enabled {
            " (auto)"
        } else {
            ""
        };
        let context_segment = match v.context_percent {
            Some(pct) => {
                let core = format!(
                    "{:.1}%/{}{auto_indicator}",
                    pct,
                    format_tokens(v.context_window)
                );
                if pct > 90.0 {
                    format!("{ERROR_FG}{core}{RESET}")
                } else if pct > 70.0 {
                    format!("{WARNING_FG}{core}{RESET}")
                } else {
                    core
                }
            }
            None => format!("?/{}{auto_indicator}", format_tokens(v.context_window)),
        };
        stats_parts.push(context_segment);

        let mut stats_left = stats_parts.join(" ");
        let mut stats_left_width = visible_width(&stats_left);
        if stats_left_width > width as usize {
            stats_left = truncate_to_width_with(&stats_left, width as usize, "...", false);
            stats_left_width = visible_width(&stats_left);
        }

        // Right side: model id (+ optional thinking segment + optional
        // provider prefix when there's room).
        let model_name = if v.model_id.is_empty() {
            "no-model".to_string()
        } else {
            v.model_id.clone()
        };
        let right_without_provider = if v.has_reasoning {
            let level = if v.thinking_level.is_empty() || v.thinking_level == "off" {
                "thinking off".to_string()
            } else {
                v.thinking_level.clone()
            };
            format!("{model_name} • {level}")
        } else {
            model_name
        };

        let min_padding = 2;
        let mut right_side = right_without_provider.clone();
        if v.available_provider_count > 1 && !v.model_provider.is_empty() {
            let candidate = format!("({}) {}", v.model_provider, right_without_provider);
            if stats_left_width + min_padding + visible_width(&candidate) <= width as usize {
                right_side = candidate;
            }
        }

        let right_width = visible_width(&right_side);
        let total_needed = stats_left_width + min_padding + right_width;
        let stats_line_plain = if total_needed <= width as usize {
            let pad = " ".repeat(width as usize - stats_left_width - right_width);
            format!("{stats_left}{pad}{right_side}")
        } else {
            let avail_for_right = width as usize - stats_left_width - min_padding;
            if avail_for_right > 0 {
                let truncated = truncate_to_width_with(&right_side, avail_for_right, "", false);
                let truncated_w = visible_width(&truncated);
                let pad = " ".repeat(
                    (width as usize)
                        .saturating_sub(stats_left_width)
                        .saturating_sub(truncated_w),
                );
                format!("{stats_left}{pad}{truncated}")
            } else {
                stats_left.clone()
            }
        };

        // Apply dim wrapping to the parts of the line that don't carry their
        // own color (everything outside the context-percent segment). The
        // colored segment may end with its own RESET, which would clear an
        // outer dim wrapper, so dim left and right halves separately.
        let dim_left = format!("{DIM}{stats_left}{RESET}");
        let remainder = &stats_line_plain[stats_left.len()..];
        let dim_right = format!("{DIM}{remainder}{RESET}");
        let stats_line = format!("{dim_left}{dim_right}");

        let mut lines = vec![pwd_line, stats_line];

        if !v.extension_statuses.is_empty() {
            let mut entries = v.extension_statuses.clone();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let joined = entries
                .iter()
                .map(|(_, text)| sanitize_status_text(text))
                .collect::<Vec<_>>()
                .join(" ");
            let line =
                truncate_to_width_with(&joined, width as usize, &format!("{DIM}...{RESET}"), false);
            lines.push(line);
        }

        lines
    }
}

/// Collapse newlines, tabs, CRs to spaces and squash repeats.
fn sanitize_status_text(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|c| match c {
            '\r' | '\n' | '\t' => ' ',
            _ => c,
        })
        .collect();
    let mut out = String::with_capacity(replaced.len());
    let mut last_space = false;
    for c in replaced.chars() {
        if c == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// Format a token count with k / M suffixes, mirroring pi-mono's
/// `formatTokens`.
fn format_tokens(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    if count < 10_000 {
        return format!("{:.1}k", count as f64 / 1_000.0);
    }
    if count < 1_000_000 {
        return format!("{}k", (count as f64 / 1_000.0).round() as u64);
    }
    if count < 10_000_000 {
        return format!("{:.1}M", count as f64 / 1_000_000.0);
    }
    format!("{}M", (count as f64 / 1_000_000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view() -> FooterViewModel {
        FooterViewModel {
            cwd: "/tmp/work".to_string(),
            home_dir: Some("/Users/me".to_string()),
            git_branch: Some("main".to_string()),
            session_name: Some("review".to_string()),
            usage: TokenUsageSummary {
                input: 1_500,
                output: 2_000,
                cache_read: 0,
                cache_write: 500,
                cost_usd: 0.123,
                using_subscription: false,
            },
            model_id: "gpt-5".to_string(),
            model_provider: "openai".to_string(),
            context_window: 200_000,
            context_percent: Some(45.5),
            auto_compact_enabled: true,
            has_reasoning: true,
            thinking_level: "medium".to_string(),
            available_provider_count: 1,
            extension_statuses: Vec::new(),
        }
    }

    #[test]
    fn renders_pwd_with_branch_and_session() {
        let comp = FooterComponent::new(sample_view());
        let lines = comp.render(120);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("/tmp/work"));
        assert!(lines[0].contains("(main)"));
        assert!(lines[0].contains("review"));
    }

    #[test]
    fn pwd_substitutes_home_directory() {
        let mut v = sample_view();
        v.cwd = "/Users/me/projects/x".to_string();
        v.git_branch = None;
        v.session_name = None;
        let lines = FooterComponent::new(v).render(120);
        assert!(lines[0].contains("~/projects/x"), "got {:?}", lines[0]);
    }

    #[test]
    fn stats_line_shows_tokens_cost_and_model() {
        let comp = FooterComponent::new(sample_view());
        let lines = comp.render(120);
        let stats = &lines[1];
        assert!(stats.contains("↑1.5k"), "input tokens missing: {stats:?}");
        assert!(stats.contains("↓2.0k"), "output tokens missing: {stats:?}");
        assert!(stats.contains("$0.123"), "cost missing: {stats:?}");
        assert!(stats.contains("gpt-5"), "model id missing: {stats:?}");
        assert!(stats.contains("medium"), "thinking missing: {stats:?}");
        assert!(stats.contains("45.5%"), "context % missing: {stats:?}");
        assert!(
            stats.contains("(auto)"),
            "auto indicator missing: {stats:?}"
        );
    }

    #[test]
    fn high_context_percent_uses_error_color() {
        let mut v = sample_view();
        v.context_percent = Some(95.0);
        let stats = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats.contains(ERROR_FG), "{stats:?}");
    }

    #[test]
    fn medium_context_percent_uses_warning_color() {
        let mut v = sample_view();
        v.context_percent = Some(75.0);
        let stats = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats.contains(WARNING_FG), "{stats:?}");
    }

    #[test]
    fn provider_prefix_only_appears_when_multiple_providers() {
        let mut v = sample_view();
        v.available_provider_count = 1;
        let stats_one = FooterComponent::new(v.clone())
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(!stats_one.contains("(openai)"), "{stats_one:?}");

        v.available_provider_count = 3;
        let stats_many = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats_many.contains("(openai)"), "{stats_many:?}");
    }

    #[test]
    fn extension_statuses_appear_sorted_on_third_line() {
        let mut v = sample_view();
        v.extension_statuses = vec![
            ("zeta".to_string(), "z status".to_string()),
            ("alpha".to_string(), "a\tstatus  with\nspaces".to_string()),
        ];
        let lines = FooterComponent::new(v).render(120);
        assert_eq!(lines.len(), 3);
        let third = &lines[2];
        let alpha_pos = third.find("a status").unwrap_or(usize::MAX);
        let zeta_pos = third.find("z status").unwrap_or(0);
        assert!(alpha_pos < zeta_pos, "alpha not first: {third:?}");
        // Tabs/newlines collapsed.
        assert!(!third.contains('\t'));
    }

    #[test]
    fn no_model_falls_back_to_placeholder() {
        let mut v = sample_view();
        v.model_id.clear();
        let stats = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats.contains("no-model"));
    }

    #[test]
    fn thinking_off_renders_label() {
        let mut v = sample_view();
        v.thinking_level = "off".to_string();
        let stats = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats.contains("thinking off"));
    }

    #[test]
    fn unknown_context_renders_question_mark() {
        let mut v = sample_view();
        v.context_percent = None;
        let stats = FooterComponent::new(v)
            .render(120)
            .into_iter()
            .nth(1)
            .unwrap();
        assert!(stats.contains("?/"));
    }

    #[test]
    fn format_tokens_steps() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(50_000), "50k");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        assert_eq!(format_tokens(15_000_000), "15M");
    }
}
