//! Custom (extension-injected) message renderer.
//!
//! Extensions inject "custom" messages with a `custom_type` tag and a
//! string-or-block content payload. The full surface supports two
//! paths:
//!
//! 1. A caller-provided custom renderer (returns a fully-styled component).
//! 2. A default rendering that puts a `[custom_type]` label above the
//!    markdown body inside a tinted box.
//!
//! This module ships path #2 — the default rendering — which is what
//! every extension falls back to. Custom-renderer injection lands with
//! the extension runtime port.
//!
//! The full `CustomMessage` type belongs with the message-store port;
//! the local [`CustomMessageData`] carries only the fields this
//! renderer actually consumes.
//!
//! Theming caveat: slot lookups (`custom_message_bg`,
//! `custom_message_label`, `custom_message_text`) are hardcoded to
//! dark-theme defaults until the theme system surfaces them.

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, MarkdownComponent, NamedColor, TextComponent};

/// Default background ANSI for a custom message — a muted purple
/// matching the `custom_message_bg` dark slot.
const DEFAULT_BG_ANSI: &str = "\x1b[48;5;53m";

/// Local view-model carrying just the fields the renderer needs.
///
/// The full `CustomMessage<T>` type also tracks `display`, `details`,
/// and a timestamp, none of which influence rendering. They will land
/// with the message-store port.
#[derive(Debug, Clone)]
pub struct CustomMessageData {
    /// Tag shown in the `[bracketed]` label.
    pub custom_type: String,
    /// Body — either pre-flattened text or a list of text blocks already
    /// concatenated by the caller. Image blocks are intentionally not
    /// supported here; the TS path drops them in default rendering.
    pub content: String,
}

impl CustomMessageData {
    pub fn new(custom_type: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            custom_type: custom_type.into(),
            content: content.into(),
        }
    }
}

/// Component that renders a custom (extension-injected) message.
pub struct CustomMessageComponent {
    inner: BoxComponent,
}

impl CustomMessageComponent {
    /// Render `message` with the default styling.
    pub fn new(message: CustomMessageData) -> Self {
        // Label: bold bright-magenta-ish text inside the box.
        let label = format!("\x1b[1m\x1b[95m[{}]\x1b[0m", message.custom_type);

        let mut markdown = MarkdownComponent::new(message.content);
        markdown.set_default_style(DefaultTextStyle {
            fg: Some(Color::Named(NamedColor::BrightWhite)),
            bg: None,
            italic: false,
        });

        let mut inner = BoxComponent::new()
            .with_padding(1, 1)
            .with_background(DEFAULT_BG_ANSI);
        inner.add_child(Box::new(TextComponent::new(label)));
        // Spacer between label and body — single empty line.
        inner.add_child(Box::new(TextComponent::new("")));
        inner.add_child(Box::new(markdown));

        Self { inner }
    }
}

impl Component for CustomMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_label_and_body() {
        let comp = CustomMessageComponent::new(CustomMessageData::new("git", "branch: main"));
        let lines = comp.render(40);
        let joined = lines.join("\n");
        assert!(joined.contains("[git]"), "missing label: {joined:?}");
        assert!(joined.contains("branch: main"), "missing body: {joined:?}");
    }

    #[test]
    fn applies_background_ansi() {
        let comp = CustomMessageComponent::new(CustomMessageData::new("ext", "hi"));
        let joined = comp.render(40).join("\n");
        assert!(
            joined.contains(DEFAULT_BG_ANSI),
            "expected background SGR: {joined:?}"
        );
    }

    #[test]
    fn label_is_bold() {
        let comp = CustomMessageComponent::new(CustomMessageData::new("ext", "x"));
        let joined = comp.render(40).join("\n");
        // Bold SGR wraps the label.
        assert!(joined.contains("\x1b[1m"), "expected bold label SGR");
    }
}
