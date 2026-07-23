//! The rt footer: a plain-data [`FooterViewModel`] and the pure renderer that
//! turns a snapshot of it into the two- or three-line bottom summary.
//!
//! # Why a view-model, and why rt-native
//!
//! The legacy footer (`components::footer`) rendered ANSI-coded `Vec<String>`
//! through the legacy `Component` trait. The rt draw path commits ratatui
//! [`Line`]s, so this is the rt rewrite: the same view-model *shape* and field
//! semantics the legacy driver populated (`build_footer_view` logic), but the
//! renderer emits styled [`Line`]s with ratatui [`Style`]/[`Color`] instead of
//! raw escapes. The view-model stays a plain, `Send` data record so the draw
//! closure can hold it behind an `Arc<Mutex<…>>` and the turn / session paths can
//! rebuild it from session state without the renderer touching `AgentSession`.
//!
//! The renderer is a free function ([`render_footer_lines`]) over a borrowed
//! view-model, so it is pure and unit-tested without a terminal.

use std::path::Path;

use hand_tui::utils::visible_width;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::core::agent_session::AgentSession;

/// Aggregated token / cost statistics shown on the footer's stats line.
///
/// The running accumulator the turn path bumps on each `MessageEnd`
/// ([`accumulate_usage`]) and the renderer reads to draw the `↑/↓/R/W/$`
/// segments. Ported verbatim from the legacy footer so the segment semantics are
/// unchanged.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TokenUsageSummary {
    /// Cumulative input (prompt) tokens across the session.
    pub input: u64,
    /// Cumulative output (completion) tokens.
    pub output: u64,
    /// Cumulative tokens read from cache.
    pub cache_read: u64,
    /// Cumulative tokens written to cache.
    pub cache_write: u64,
    /// Cumulative cost in USD.
    pub cost_usd: f64,
    /// True when the active model is billed via an OAuth subscription (renders the
    /// `(sub)` indicator).
    pub using_subscription: bool,
}

/// Plain-data view-model rebuilt from session state and consumed by
/// [`render_footer_lines`].
///
/// Mirrors the legacy `FooterViewModel` field-for-field so [`build_footer_view`]
/// is a straight port of the legacy `build_footer_view`.
#[derive(Debug, Default, Clone)]
pub struct FooterViewModel {
    /// Working directory; the renderer applies the `~` substitution when
    /// [`Self::home_dir`] is supplied.
    pub cwd: String,
    /// Optional home directory for the `~` substitution. `None` disables it.
    pub home_dir: Option<String>,
    /// Optional git branch shown after the cwd as `(branch)`.
    pub git_branch: Option<String>,
    /// Optional human session label shown after a `•` separator.
    pub session_name: Option<String>,
    /// Token / cost stats. All-zero values render no token segments (only the
    /// context segment, which is always present).
    pub usage: TokenUsageSummary,
    /// Active model id; falls back to `no-model` when empty.
    pub model_id: String,
    /// Active model provider, used for the `(provider)` prefix when more than one
    /// provider is configured.
    pub model_provider: String,
    /// Active model context window in tokens (`0` ⇒ unknown).
    pub context_window: u64,
    /// Context utilisation as a percent, or `None` if not yet computable.
    pub context_percent: Option<f64>,
    /// Whether the auto-compact indicator should appear.
    pub auto_compact_enabled: bool,
    /// Whether the active model exposes a reasoning toggle (drives the `thinking …`
    /// segment).
    pub has_reasoning: bool,
    /// Free-form thinking-level label (`off`, `low`, `medium`, …).
    pub thinking_level: String,
    /// Number of providers with credentials. `> 1` enables the `(provider)` prefix.
    pub available_provider_count: usize,
}

/// Build the footer view-model from current session state and the running usage
/// accumulator.
///
/// A direct port of the legacy driver's `build_footer_view`: the context percent
/// is estimated from the message history against the model's context window, and
/// the thinking-level label comes from the session's stream options. The `usage`
/// accumulator is passed in (rather than re-walked) so a per-frame rebuild stays
/// cheap.
#[must_use]
pub fn build_footer_view(
    session: &AgentSession,
    cwd: &Path,
    usage: TokenUsageSummary,
) -> FooterViewModel {
    let context_window = session.model().context_window;
    let context_percent = if context_window > 0 {
        let tokens = crate::core::compaction::estimate_context_tokens(session.messages()) as f64;
        Some(tokens / context_window as f64 * 100.0)
    } else {
        None
    };
    FooterViewModel {
        cwd: cwd.display().to_string(),
        home_dir: dirs::home_dir().map(|p| p.display().to_string()),
        git_branch: detect_git_branch(cwd),
        session_name: session.label().map(|s| s.to_string()),
        usage,
        model_id: session.model().id.clone(),
        model_provider: session.model().provider.as_str().to_string(),
        context_window,
        context_percent,
        auto_compact_enabled: session.auto_compaction_enabled(),
        has_reasoning: session.model().reasoning,
        thinking_level: session
            .stream_options()
            .reasoning
            .map(|l| thinking_level_label(Some(l)).to_string())
            .unwrap_or_default(),
        available_provider_count: count_providers_with_credentials(),
    }
}

/// Accumulate token usage from an assistant message's [`model::Usage`] into the
/// running total. Called on each `MessageEnd` so the footer's spend segment
/// increases monotonically across a session (VAL-CHAT-005).
pub fn accumulate_usage(running: &mut TokenUsageSummary, usage: &model::Usage) {
    running.input += usage.input;
    running.output += usage.output;
    running.cache_read += usage.cache_read;
    running.cache_write += usage.cache_write;
    running.cost_usd += usage.cost.total;
}

/// Count the providers that have an API key in the environment; `> 1` widens the
/// footer to prefix the model with its provider. Ported from the legacy driver.
fn count_providers_with_credentials() -> usize {
    model::get_providers()
        .into_iter()
        .filter(|p| model::get_env_api_key(p).is_some())
        .count()
}

/// Detect the current git branch by reading `.git/HEAD` in `cwd` or any ancestor.
///
/// Returns `None` outside a git repo or when HEAD cannot be read. A symbolic
/// `ref: refs/heads/<name>` yields `<name>`; a detached HEAD (a raw SHA) yields
/// its first seven characters. Ported from the legacy driver so a session action
/// like `!git checkout -b tmp` surfaces on the next footer rebuild (VAL-CHAT-035).
fn detect_git_branch(cwd: &Path) -> Option<String> {
    let mut dir = cwd;
    loop {
        let head = dir.join(".git").join("HEAD");
        if head.exists() {
            let text = std::fs::read_to_string(&head).ok()?;
            let line = text.trim();
            if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
                return Some(rest.to_string());
            }
            return Some(line.chars().take(7).collect());
        }
        dir = dir.parent()?;
    }
}

/// The label for a thinking level, matching the thinking-selector's `level_label`
/// (`off` / `minimal` / `low` / …). Kept local so the footer does not reach into
/// a sibling component's private helper; shared with the slash-command
/// session-info renderer so both surfaces agree on the label text.
pub(crate) fn thinking_level_label(level: Option<model::ThinkingLevel>) -> &'static str {
    match level {
        None => "off",
        Some(model::ThinkingLevel::Minimal) => "minimal",
        Some(model::ThinkingLevel::Low) => "low",
        Some(model::ThinkingLevel::Medium) => "medium",
        Some(model::ThinkingLevel::High) => "high",
        Some(model::ThinkingLevel::Xhigh) => "xhigh",
        Some(model::ThinkingLevel::Max) => "max",
    }
}

/// Format a token count with `k` / `M` suffixes for compact display. Ported from
/// the legacy footer.
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

/// The dim style applied to the footer's non-coloured text.
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Truncate a plain string to at most `width` display columns, appending `…`
/// when it is clipped, on a character (column) boundary so a multibyte glyph is
/// never byte-sliced.
///
/// Unlike `hand_tui::utils::truncate_to_width_with` (which brackets the ellipsis
/// with `\x1b[0m` resets for the legacy ANSI-string renderer), this returns clean
/// plain text safe to place in a ratatui [`Span`].
fn truncate_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if width_of(text) <= width {
        return text.to_string();
    }
    // Reserve one column for the ellipsis.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = width_of(&ch.to_string());
        if used + cw > budget {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Display width of a string in terminal columns.
fn width_of(text: &str) -> usize {
    visible_width(text)
}

/// Render the footer view-model into its two lines (cwd line + stats line).
///
/// - **Line 1** — the `~`-abbreviated cwd, then `(branch)` and `• session` when
///   present, dim, clipped to the width.
/// - **Line 2** — left-aligned token/cost/context segments and a right-aligned
///   model id (with the optional `• thinking` suffix and `(provider)` prefix),
///   padded to fill the width. The context-percent segment is coloured by
///   utilisation (yellow > 70 %, red > 90 %); everything else is dim.
///
/// Pure: takes a borrowed view-model and a width, returns owned [`Line`]s, so it
/// is unit-tested without a terminal.
#[must_use]
pub fn render_footer_lines(view: &FooterViewModel, width: u16) -> Vec<Line<'static>> {
    let w = width as usize;

    // --- Line 1: cwd (~-abbreviated) + branch + session, dim ---------------
    let mut pwd = view.cwd.clone();
    if let Some(home) = &view.home_dir
        && !home.is_empty()
        && pwd.starts_with(home)
    {
        pwd = format!("~{}", &pwd[home.len()..]);
    }
    if let Some(branch) = &view.git_branch {
        pwd = format!("{pwd} ({branch})");
    }
    if let Some(name) = &view.session_name {
        pwd = format!("{pwd} • {name}");
    }
    let pwd_line = Line::from(Span::styled(truncate_to_width(&pwd, w), dim()));

    // --- Line 2: stats (left) + model (right) ------------------------------
    let mut stats_parts: Vec<String> = Vec::new();
    if view.usage.input > 0 {
        stats_parts.push(format!("↑{}", format_tokens(view.usage.input)));
    }
    if view.usage.output > 0 {
        stats_parts.push(format!("↓{}", format_tokens(view.usage.output)));
    }
    if view.usage.cache_read > 0 {
        stats_parts.push(format!("R{}", format_tokens(view.usage.cache_read)));
    }
    if view.usage.cache_write > 0 {
        stats_parts.push(format!("W{}", format_tokens(view.usage.cache_write)));
    }
    if view.usage.cost_usd > 0.0 || view.usage.using_subscription {
        stats_parts.push(format!(
            "${:.3}{}",
            view.usage.cost_usd,
            if view.usage.using_subscription {
                " (sub)"
            } else {
                ""
            }
        ));
    }

    // The context segment (always present), plus its colour by utilisation.
    let auto_indicator = if view.auto_compact_enabled {
        " (auto)"
    } else {
        ""
    };
    let (context_text, context_color) = match view.context_percent {
        Some(pct) => {
            let core = format!(
                "{:.1}%/{}{auto_indicator}",
                pct,
                format_tokens(view.context_window)
            );
            let color = if pct > 90.0 {
                Some(Color::Red)
            } else if pct > 70.0 {
                Some(Color::Yellow)
            } else {
                None
            };
            (core, color)
        }
        None => (
            format!("?/{}{auto_indicator}", format_tokens(view.context_window)),
            None,
        ),
    };

    // Left half: the token/cost parts joined, then the context segment. The
    // token parts are one dim run; the context segment carries its own colour.
    let stats_prefix = stats_parts.join(" ");
    let mut left_plain = stats_prefix.clone();
    if !left_plain.is_empty() {
        left_plain.push(' ');
    }
    left_plain.push_str(&context_text);
    let left_width = width_of(&left_plain);

    // Right half: model id (+ optional thinking suffix, + optional provider
    // prefix when there's room).
    let model_name = if view.model_id.is_empty() {
        "no-model".to_string()
    } else {
        view.model_id.clone()
    };
    let right_without_provider = if view.has_reasoning {
        let level = if view.thinking_level.is_empty() || view.thinking_level == "off" {
            "thinking off".to_string()
        } else {
            view.thinking_level.clone()
        };
        format!("{model_name} • {level}")
    } else {
        model_name
    };
    let min_padding = 2usize;
    let mut right_side = right_without_provider.clone();
    if view.available_provider_count > 1 && !view.model_provider.is_empty() {
        let candidate = format!("({}) {}", view.model_provider, right_without_provider);
        if left_width + min_padding + width_of(&candidate) <= w {
            right_side = candidate;
        }
    }
    let right_width = width_of(&right_side);

    // Assemble the spans: dim token prefix, coloured (or dim) context, padding,
    // dim model on the right. When the line overflows the width the right side is
    // dropped so the left half is never clipped mid-segment.
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !stats_prefix.is_empty() {
        spans.push(Span::styled(format!("{stats_prefix} "), dim()));
    }
    let context_style = match context_color {
        Some(color) => Style::default().fg(color),
        None => dim(),
    };
    spans.push(Span::styled(context_text, context_style));

    if left_width + min_padding + right_width <= w {
        let pad = w.saturating_sub(left_width).saturating_sub(right_width);
        spans.push(Span::styled(" ".repeat(pad), dim()));
        spans.push(Span::styled(right_side, dim()));
    }
    let stats_line = Line::from(spans);

    vec![pwd_line, stats_line]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn sample() -> FooterViewModel {
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
        }
    }

    #[test]
    fn renders_two_lines() {
        let lines = render_footer_lines(&sample(), 120);
        assert_eq!(lines.len(), 2, "footer is two lines");
    }

    #[test]
    fn cwd_line_shows_branch_and_session() {
        let lines = render_footer_lines(&sample(), 120);
        let pwd = text_of(&lines[0]);
        assert!(pwd.contains("/tmp/work"), "cwd missing: {pwd:?}");
        assert!(pwd.contains("(main)"), "branch missing: {pwd:?}");
        assert!(pwd.contains("review"), "session missing: {pwd:?}");
    }

    #[test]
    fn cwd_line_substitutes_home_directory() {
        let mut v = sample();
        v.cwd = "/Users/me/projects/x".to_string();
        v.git_branch = None;
        v.session_name = None;
        let pwd = text_of(&render_footer_lines(&v, 120)[0]);
        assert!(pwd.contains("~/projects/x"), "got {pwd:?}");
    }

    #[test]
    fn stats_line_shows_tokens_cost_context_and_model() {
        let stats = text_of(&render_footer_lines(&sample(), 120)[1]);
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
    fn high_context_percent_uses_red() {
        let mut v = sample();
        v.context_percent = Some(95.0);
        let line = &render_footer_lines(&v, 120)[1];
        let colored = line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::Red) && s.content.contains("95.0%"));
        assert!(colored, "context segment not red: {:?}", line.spans);
    }

    #[test]
    fn medium_context_percent_uses_yellow() {
        let mut v = sample();
        v.context_percent = Some(75.0);
        let line = &render_footer_lines(&v, 120)[1];
        let colored = line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::Yellow) && s.content.contains("75.0%"));
        assert!(colored, "context segment not yellow: {:?}", line.spans);
    }

    #[test]
    fn provider_prefix_only_when_multiple_providers() {
        let mut v = sample();
        v.available_provider_count = 1;
        let one = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(
            !one.contains("(openai)"),
            "single provider prefixed: {one:?}"
        );

        v.available_provider_count = 3;
        let many = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(
            many.contains("(openai)"),
            "multi provider not prefixed: {many:?}"
        );
    }

    #[test]
    fn no_model_falls_back_to_placeholder() {
        let mut v = sample();
        v.model_id.clear();
        let stats = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(stats.contains("no-model"), "got {stats:?}");
    }

    #[test]
    fn thinking_off_renders_label() {
        let mut v = sample();
        v.thinking_level = "off".to_string();
        let stats = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(stats.contains("thinking off"), "got {stats:?}");
    }

    #[test]
    fn unknown_context_renders_question_mark() {
        let mut v = sample();
        v.context_percent = None;
        let stats = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(stats.contains("?/"), "got {stats:?}");
    }

    #[test]
    fn zero_usage_shows_only_context_segment() {
        let mut v = sample();
        v.usage = TokenUsageSummary::default();
        let stats = text_of(&render_footer_lines(&v, 120)[1]);
        assert!(!stats.contains('↑'), "zero input shows no arrow: {stats:?}");
        assert!(!stats.contains('$'), "zero cost shows no dollar: {stats:?}");
        assert!(stats.contains("45.5%"), "context still present: {stats:?}");
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

    #[test]
    fn accumulate_usage_is_monotonic_across_turns() {
        use model::types::{Usage, UsageCost};
        let mut acc = TokenUsageSummary::default();
        let turn = Usage {
            input: 100,
            output: 200,
            cache_read: 10,
            cache_write: 20,
            total_tokens: 330,
            cost: UsageCost {
                total: 0.5,
                ..Default::default()
            },
        };
        accumulate_usage(&mut acc, &turn);
        assert_eq!(acc.input, 100);
        assert_eq!(acc.output, 200);
        assert!((acc.cost_usd - 0.5).abs() < 1e-9);
        // A second turn only adds — the totals never decrease.
        accumulate_usage(&mut acc, &turn);
        assert_eq!(acc.input, 200);
        assert_eq!(acc.output, 400);
        assert_eq!(acc.cache_read, 20);
        assert_eq!(acc.cache_write, 40);
        assert!((acc.cost_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn thinking_level_label_maps_variants() {
        assert_eq!(thinking_level_label(None), "off");
        assert_eq!(thinking_level_label(Some(model::ThinkingLevel::Low)), "low");
        assert_eq!(
            thinking_level_label(Some(model::ThinkingLevel::Medium)),
            "medium"
        );
        assert_eq!(thinking_level_label(Some(model::ThinkingLevel::Max)), "max");
    }

    #[test]
    fn narrow_width_clips_without_panic() {
        // A width narrower than the content must not byte-slice a multibyte cwd.
        let mut v = sample();
        v.cwd = "/项目/工作目录/深层/路径".to_string();
        v.home_dir = None;
        for width in 1u16..=20 {
            let _ = render_footer_lines(&v, width);
        }
    }
}
