//! Compaction-summary message renderer.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts`.
//!
//! When the conversation is compacted, the driver inserts a synthetic
//! "compactionSummary" message that displays either a short collapsed line
//! ("Compacted from N tokens (… to expand)") or the full markdown summary
//! when the user expands it.
//!
//! The full `CompactionSummaryMessage` Rust type belongs with the
//! message-store port (queued); the local [`CompactionSummaryData`] mirrors
//! only what this renderer needs.
//!
//! Theming caveat: shares pi-mono's `customMessageBg`/`customMessageLabel`
//! slots — hardcoded here, see parent module docs.
//!
//! Keybinding caveat: pi-mono renders the expand hint via
//! `keyText("app.tools.expand")`. Our Rust [`hand_tui`] keybinding registry
//! only carries the `tui.*` set; the coding-agent–specific `app.*` table
//! ports with the driver. Until then the hint string is configurable via
//! [`CompactionSummaryMessageComponent::with_expand_hint`].

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, MarkdownComponent, NamedColor, TextComponent};

const DEFAULT_BG_ANSI: &str = "\x1b[48;5;53m";

/// Default key hint shown in the collapsed view.
pub const DEFAULT_EXPAND_HINT: &str = "ctrl+r";

/// Local view-model for a compaction-summary message.
#[derive(Debug, Clone)]
pub struct CompactionSummaryData {
    /// Markdown summary produced by the compaction step.
    pub summary: String,
    /// Token count before compaction; rendered with thousands separators.
    pub tokens_before: u64,
}

impl CompactionSummaryData {
    pub fn new(summary: impl Into<String>, tokens_before: u64) -> Self {
        Self {
            summary: summary.into(),
            tokens_before,
        }
    }
}

/// Component that renders a compaction-summary message.
pub struct CompactionSummaryMessageComponent {
    data: CompactionSummaryData,
    expanded: bool,
    expand_hint: String,
}

impl CompactionSummaryMessageComponent {
    pub fn new(data: CompactionSummaryData) -> Self {
        Self {
            data,
            expanded: false,
            expand_hint: DEFAULT_EXPAND_HINT.to_string(),
        }
    }

    /// Override the expand-key hint shown in the collapsed view.
    pub fn with_expand_hint(mut self, hint: impl Into<String>) -> Self {
        self.expand_hint = hint.into();
        self
    }

    /// Toggle between collapsed and expanded views.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    fn build(&self) -> BoxComponent {
        let token_str = format_thousands(self.data.tokens_before);
        let label = "\x1b[1m\x1b[95m[compaction]\x1b[0m".to_string();

        let mut inner = BoxComponent::new()
            .with_padding(1, 1)
            .with_background(DEFAULT_BG_ANSI);
        inner.add_child(Box::new(TextComponent::new(label)));
        inner.add_child(Box::new(TextComponent::new("")));

        if self.expanded {
            let body = format!(
                "**Compacted from {token_str} tokens**\n\n{}",
                self.data.summary
            );
            let mut md = MarkdownComponent::new(body);
            md.set_default_style(DefaultTextStyle {
                fg: Some(Color::Named(NamedColor::BrightWhite)),
                bg: None,
                italic: false,
            });
            inner.add_child(Box::new(md));
        } else {
            // "Compacted from N tokens (<key> to expand)"
            let line = format!(
                "\x1b[97mCompacted from {token_str} tokens (\x1b[90m{}\x1b[97m to expand)\x1b[0m",
                self.expand_hint
            );
            inner.add_child(Box::new(TextComponent::new(line)));
        }
        inner
    }
}

impl Component for CompactionSummaryMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.build().render(width)
    }
}

/// Render a u64 with comma thousands separators (`12345` → `"12,345"`).
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_shows_expand_hint() {
        let comp = CompactionSummaryMessageComponent::new(CompactionSummaryData::new(
            "lots of stuff",
            12_345,
        ))
        .with_expand_hint("ctrl+r");
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("[compaction]"), "missing label: {joined:?}");
        assert!(
            joined.contains("Compacted from 12,345 tokens"),
            "missing token count line: {joined:?}"
        );
        assert!(joined.contains("ctrl+r"), "missing expand hint: {joined:?}");
        assert!(
            !joined.contains("lots of stuff"),
            "must not leak summary when collapsed: {joined:?}"
        );
    }

    #[test]
    fn expanded_shows_summary_body() {
        let mut comp = CompactionSummaryMessageComponent::new(CompactionSummaryData::new(
            "summary text",
            1000,
        ));
        comp.set_expanded(true);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("[compaction]"));
        assert!(
            joined.contains("Compacted from 1,000 tokens"),
            "missing header: {joined:?}"
        );
        assert!(
            joined.contains("summary text"),
            "missing summary body: {joined:?}"
        );
    }

    #[test]
    fn formats_tokens_with_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }
}
