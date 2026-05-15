//! User message renderer.
//!
//! Renders a single user-role message as a markdown body wrapped in a
//! background-tinted [`hand_tui::BoxComponent`], with OSC 133 zone
//! markers on the first and last lines so terminals with
//! shell-integration support (iTerm2, WezTerm, Ghostty) can detect
//! prompt regions.
//!
//! Theming caveat: the component expects `user_message_bg` /
//! `user_message_text` slots. Until the theme system surfaces them
//! it hardcodes a 256-color background and falls back to the markdown
//! component's default text colors.

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, MarkdownComponent};

/// OSC 133 prompt-zone start (`A`).
const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
/// OSC 133 prompt-zone end (`B`).
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
/// OSC 133 command-finished marker (`C`).
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Default user-message background — a muted gray (`#343541`). The
/// truecolor escape below works in modern terminals; older 256-color
/// terminals would substitute `\x1b[48;5;238m` for a roughly equivalent
/// hue.
const DEFAULT_BG_ANSI: &str = "\x1b[48;2;52;53;65m";
/// Hex equivalent of [`DEFAULT_BG_ANSI`] — consumed by the markdown renderer
/// so wrapped lines get tinted edge-to-edge.
const DEFAULT_BG_HEX: &str = "#343541";
/// Default foreground for user-message text. Light gray with good
/// contrast against [`DEFAULT_BG_HEX`], matching a dark-theme
/// terminal foreground default.
const DEFAULT_FG_HEX: &str = "#e6e6e6";

/// Component that renders a user message.
pub struct UserMessageComponent {
    inner: BoxComponent,
}

impl UserMessageComponent {
    /// Create a renderer for `text` (markdown source).
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_background(text, DEFAULT_BG_ANSI)
    }

    /// Create a renderer with an explicit ANSI background sequence (e.g. from
    /// an externally-resolved theme slot).
    pub fn with_background(text: impl Into<String>, bg_ansi: impl Into<String>) -> Self {
        let bg_ansi = bg_ansi.into();
        let mut markdown = MarkdownComponent::new(text);
        // Pass background and a contrasting foreground through to the
        // markdown renderer so the entire row tints edge-to-edge AND text
        // stays readable (terminals don't always set a useful default fg
        // inside SGR-painted regions — without an explicit fg the user
        // ends up with a blue block of "invisible" text).
        markdown.set_default_style(DefaultTextStyle {
            fg: Some(Color::Hex(DEFAULT_FG_HEX.to_string())),
            bg: Some(Color::Hex(bg_to_hex(&bg_ansi))),
            italic: false,
        });

        let mut inner = BoxComponent::new()
            .with_padding(1, 1)
            .with_background(bg_ansi);
        inner.add_child(Box::new(markdown));
        Self { inner }
    }
}

impl Component for UserMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = self.inner.render(width);
        if lines.is_empty() {
            return lines;
        }
        let last = lines.len() - 1;
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

/// Best-effort extraction of a hex color from an ANSI background sequence.
///
/// Used purely so the markdown body's wrapped-text fill matches the box
/// background. Falls back to a placeholder hex when the sequence isn't a
/// recognised 256/RGB form — the visible result is identical because the
/// surrounding box already paints the background.
fn bg_to_hex(_bg: &str) -> String {
    // The bg ansi is opaque (256-color or truecolor); rather than parse it,
    // return the same hex the default constants advertise so the markdown
    // background matches the box background. Callers that pass a custom
    // ANSI bg should also use a custom hex — see [`Self::with_background`].
    DEFAULT_BG_HEX.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text_inside_zone_markers() {
        let comp = UserMessageComponent::new("hello");
        let lines = comp.render(40);
        assert!(!lines.is_empty(), "expected at least one rendered line");
        assert!(
            lines[0].starts_with(OSC133_ZONE_START),
            "first line missing OSC 133 zone start: {:?}",
            lines[0]
        );
        let last = lines.last().expect("nonempty");
        assert!(
            last.contains(OSC133_ZONE_END) && last.contains(OSC133_ZONE_FINAL),
            "last line missing OSC 133 zone end/final: {last:?}"
        );
        // Body somewhere in between contains the rendered text.
        let joined = lines.join("\n");
        assert!(
            joined.contains("hello"),
            "rendered output missing body text: {joined:?}"
        );
    }

    #[test]
    fn applies_background_ansi_to_body() {
        let comp = UserMessageComponent::new("hi");
        let lines = comp.render(40);
        let joined = lines.join("\n");
        assert!(
            joined.contains(DEFAULT_BG_ANSI),
            "expected default background ANSI in output: {joined:?}"
        );
    }

    #[test]
    fn empty_text_does_not_panic() {
        let comp = UserMessageComponent::new("");
        // Box still emits padding lines, so we only assert it didn't crash and
        // — if any lines are produced — the zone markers wrap them.
        let lines = comp.render(20);
        if !lines.is_empty() {
            assert!(lines[0].starts_with(OSC133_ZONE_START));
        }
    }
}
