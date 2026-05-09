//! Horizontal border that adapts to viewport width.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/dynamic-border.ts`.
//!
//! Renders a single line of `─` glyphs spanning the rendered width, with an
//! optional ANSI color prefix. pi-mono parameterises the line color via a
//! closure that reaches into the global theme; the Rust port accepts a static
//! ANSI prefix instead, which keeps the component `Send + 'static` and avoids
//! lifetime gymnastics for callers (each call site's color is a small string
//! literal anyway).
//!
//! Theming caveat: the TS source resolves `theme.fg("border", ...)` by
//! default. Until the coding-agent theme port lands (see parent module docs)
//! the default color is bright black (`\x1b[90m`), matching the dark theme's
//! `border` slot.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::Component;

/// Default ANSI prefix for the border — bright black.
const DEFAULT_FG: &str = "\x1b[90m";
/// ANSI reset.
const RESET: &str = "\x1b[0m";

/// Horizontal-rule component that fills the rendered width with `─` and
/// applies a configurable ANSI color prefix.
pub struct DynamicBorderComponent {
    fg_ansi: String,
}

impl DynamicBorderComponent {
    /// Construct a border with the default bright-black color.
    pub fn new() -> Self {
        Self {
            fg_ansi: DEFAULT_FG.to_string(),
        }
    }

    /// Construct a border with an explicit ANSI prefix (e.g. `"\x1b[36m"` for
    /// cyan). The prefix is applied once per render and reset at the end of
    /// the line.
    pub fn with_color(ansi_prefix: impl Into<String>) -> Self {
        Self {
            fg_ansi: ansi_prefix.into(),
        }
    }

    /// Replace the color prefix at runtime.
    pub fn set_color(&mut self, ansi_prefix: impl Into<String>) {
        self.fg_ansi = ansi_prefix.into();
    }
}

impl Default for DynamicBorderComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for DynamicBorderComponent {
    fn render(&self, width: u16) -> Vec<String> {
        // pi-mono guarantees at least one glyph even when width is 0.
        let cells = width.max(1) as usize;
        let line = if self.fg_ansi.is_empty() {
            "─".repeat(cells)
        } else {
            format!("{}{}{}", self.fg_ansi, "─".repeat(cells), RESET)
        };
        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_single_line_at_requested_width() {
        let border = DynamicBorderComponent::new();
        let lines = border.render(12);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].matches('─').count(), 12);
    }

    #[test]
    fn applies_default_color_prefix_and_reset() {
        let lines = DynamicBorderComponent::new().render(4);
        assert!(lines[0].starts_with(DEFAULT_FG));
        assert!(lines[0].ends_with(RESET));
    }

    #[test]
    fn custom_color_overrides_default() {
        let border = DynamicBorderComponent::with_color("\x1b[36m");
        let line = &border.render(3)[0];
        assert!(line.starts_with("\x1b[36m"));
        assert!(line.ends_with(RESET));
        assert!(!line.contains(DEFAULT_FG));
    }

    #[test]
    fn empty_color_skips_sgr_wrapping() {
        let border = DynamicBorderComponent::with_color("");
        let line = &border.render(2)[0];
        assert_eq!(line, "──");
        assert!(!line.contains(RESET));
    }

    #[test]
    fn zero_width_still_yields_one_glyph() {
        let lines = DynamicBorderComponent::new().render(0);
        assert_eq!(lines[0].matches('─').count(), 1);
    }

    #[test]
    fn set_color_updates_subsequent_renders() {
        let mut border = DynamicBorderComponent::new();
        border.set_color("\x1b[31m");
        let line = &border.render(2)[0];
        assert!(line.contains("\x1b[31m"));
    }
}
