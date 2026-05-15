//! Branch-summary message renderer.
//!
//! When the user navigates back from a branched conversation, the driver
//! inserts a synthetic "branchSummary" message that shows either a short
//! collapsed line or the full markdown summary.
//!
//! Mirrors
//! [`super::compaction_summary_message::CompactionSummaryMessageComponent`]
//! in structure; the differences are the `[branch]` label, the heading,
//! and the absence of a token count.
//!
//! Theming + keybinding caveats: same as
//! [`super::compaction_summary_message`] (see parent module docs).

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, MarkdownComponent, NamedColor, TextComponent};

const DEFAULT_BG_ANSI: &str = "\x1b[48;5;53m";

/// Default key hint shown in the collapsed view.
pub const DEFAULT_EXPAND_HINT: &str = "ctrl+r";

/// Local view-model for a branch-summary message.
#[derive(Debug, Clone)]
pub struct BranchSummaryData {
    /// Markdown summary describing the branch.
    pub summary: String,
}

impl BranchSummaryData {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
        }
    }
}

/// Component that renders a branch-summary message.
pub struct BranchSummaryMessageComponent {
    data: BranchSummaryData,
    expanded: bool,
    expand_hint: String,
}

impl BranchSummaryMessageComponent {
    pub fn new(data: BranchSummaryData) -> Self {
        Self {
            data,
            expanded: false,
            expand_hint: DEFAULT_EXPAND_HINT.to_string(),
        }
    }

    pub fn with_expand_hint(mut self, hint: impl Into<String>) -> Self {
        self.expand_hint = hint.into();
        self
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    fn build(&self) -> BoxComponent {
        let label = "\x1b[1m\x1b[95m[branch]\x1b[0m".to_string();

        let mut inner = BoxComponent::new()
            .with_padding(1, 1)
            .with_background(DEFAULT_BG_ANSI);
        inner.add_child(Box::new(TextComponent::new(label)));
        inner.add_child(Box::new(TextComponent::new("")));

        if self.expanded {
            let body = format!("**Branch Summary**\n\n{}", self.data.summary);
            let mut md = MarkdownComponent::new(body);
            md.set_default_style(DefaultTextStyle {
                fg: Some(Color::Named(NamedColor::BrightWhite)),
                bg: None,
                italic: false,
            });
            inner.add_child(Box::new(md));
        } else {
            let line = format!(
                "\x1b[97mBranch summary (\x1b[90m{}\x1b[97m to expand)\x1b[0m",
                self.expand_hint
            );
            inner.add_child(Box::new(TextComponent::new(line)));
        }
        inner
    }
}

impl Component for BranchSummaryMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.build().render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_shows_expand_hint() {
        let comp = BranchSummaryMessageComponent::new(BranchSummaryData::new("inner stuff"))
            .with_expand_hint("ctrl+r");
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("[branch]"), "missing label: {joined:?}");
        assert!(
            joined.contains("Branch summary"),
            "missing collapsed line: {joined:?}"
        );
        assert!(joined.contains("ctrl+r"), "missing hint: {joined:?}");
        assert!(
            !joined.contains("inner stuff"),
            "must not leak summary when collapsed: {joined:?}"
        );
    }

    #[test]
    fn expanded_shows_summary_body() {
        let mut comp = BranchSummaryMessageComponent::new(BranchSummaryData::new("inner stuff"));
        comp.set_expanded(true);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("[branch]"));
        assert!(
            joined.contains("inner stuff"),
            "missing summary body: {joined:?}"
        );
    }
}
