//! Skill-invocation message renderer.
//!
//! Renders a parsed `<skill>` block from an assistant message in one
//! of two shapes:
//!
//! * **Collapsed** (default) — a single line
//!   `[skill] <name> (<key> to expand)` echoing the bracketed-label
//!   style used by [`super::custom_message`].
//! * **Expanded** — the same `[skill]` label followed by a markdown
//!   body built from `**<name>**\n\n<content>`.
//!
//! [`ParsedSkillBlockData`] is a local view-model carrying only the
//! fields the renderer consumes; the agent-session port supplies a
//! richer parsed-block type with span metadata and the original raw
//! text.
//!
//! Theming caveat: the component expects `custom_message_bg`,
//! `custom_message_label`, `custom_message_text`, and `dim` slots.
//! Until the theme system surfaces them we hardcode ANSI defaults
//! that match the dark-theme spirit and reuse the same background as
//! `custom_message` for visual consistency.
//!
//! TODO: theme integration deferred until the theme slot wiring lands.

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, MarkdownComponent, NamedColor, TextComponent};

use super::keybinding_hints::key_text;

/// Background ANSI for the message — same muted purple as
/// [`super::custom_message`] so skill blocks visually align with custom
/// messages.
const DEFAULT_BG_ANSI: &str = "\x1b[48;5;53m";

/// Local view-model carrying the fields the renderer consumes.
///
/// The richer parsed-block type produced by the agent-session port also
/// carries `raw` text and span metadata; those land with that port.
#[derive(Debug, Clone)]
pub struct ParsedSkillBlockData {
    /// Skill name shown in the collapsed line and the expanded header.
    pub name: String,
    /// Skill body (markdown) rendered when expanded.
    pub content: String,
}

impl ParsedSkillBlockData {
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            content: content.into(),
        }
    }
}

/// Component rendering a parsed `<skill>` block.
pub struct SkillInvocationMessageComponent {
    block: ParsedSkillBlockData,
    expanded: bool,
}

impl SkillInvocationMessageComponent {
    /// Construct a collapsed renderer for `block`.
    pub fn new(block: ParsedSkillBlockData) -> Self {
        Self {
            block,
            expanded: false,
        }
    }

    /// Toggle the expansion state. Caller is expected to call
    /// [`Component::invalidate`] / re-render after mutating.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// Whether the renderer is currently expanded.
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn build(&self) -> BoxComponent {
        let mut bx = BoxComponent::new()
            .with_padding(1, 1)
            .with_background(DEFAULT_BG_ANSI);

        if self.expanded {
            // Bold bright-magenta `[skill]` label, mirroring custom_message.
            let label = "\x1b[1m\x1b[95m[skill]\x1b[22m\x1b[0m";
            bx.add_child(Box::new(TextComponent::new(label)));

            // Markdown body: `**<name>**\n\n<content>`.
            let body = format!("**{}**\n\n{}", self.block.name, self.block.content);
            let mut md = MarkdownComponent::new(body);
            md.set_default_style(DefaultTextStyle {
                fg: Some(Color::Named(NamedColor::BrightWhite)),
                bg: None,
                italic: false,
            });
            bx.add_child(Box::new(md));
        } else {
            // Single-line: `[skill] <name> (<key> to expand)` with a dim hint.
            let key = key_text("app.tools.expand");
            let hint = if key.is_empty() {
                String::new()
            } else {
                format!(" \x1b[2m({key} to expand)\x1b[0m")
            };
            let line = format!(
                "\x1b[1m\x1b[95m[skill]\x1b[22m\x1b[0m \x1b[97m{}{}",
                self.block.name, hint
            );
            bx.add_child(Box::new(TextComponent::new(line)));
        }

        bx
    }
}

impl Component for SkillInvocationMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.build().render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_renders_label_and_name() {
        let comp = SkillInvocationMessageComponent::new(ParsedSkillBlockData::new(
            "code-review",
            "ignored when collapsed",
        ));
        let lines = comp.render(60);
        let joined = lines.join("\n");
        assert!(joined.contains("[skill]"), "missing label: {joined:?}");
        assert!(joined.contains("code-review"), "missing name: {joined:?}");
        assert!(
            !joined.contains("ignored when collapsed"),
            "body should be hidden when collapsed: {joined:?}"
        );
    }

    #[test]
    fn expanded_renders_body_with_name_header() {
        let mut comp = SkillInvocationMessageComponent::new(ParsedSkillBlockData::new(
            "explain",
            "Explain the failing test.",
        ));
        comp.set_expanded(true);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("[skill]"));
        assert!(joined.contains("explain"), "name missing: {joined:?}");
        assert!(
            joined.contains("Explain the failing test."),
            "body missing: {joined:?}"
        );
    }

    #[test]
    fn applies_background_ansi() {
        let comp = SkillInvocationMessageComponent::new(ParsedSkillBlockData::new("s", "x"));
        let joined = comp.render(40).join("\n");
        assert!(
            joined.contains(DEFAULT_BG_ANSI),
            "expected background SGR: {joined:?}"
        );
    }

    #[test]
    fn label_is_bold() {
        let comp = SkillInvocationMessageComponent::new(ParsedSkillBlockData::new("s", "x"));
        let joined = comp.render(40).join("\n");
        assert!(joined.contains("\x1b[1m"), "expected bold SGR");
    }
}
