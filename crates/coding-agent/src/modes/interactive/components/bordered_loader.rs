//! Bordered loader component.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/bordered-loader.ts`.
//!
//! Wraps a [`hand_tui::CancellableLoaderComponent`] (or a plain
//! [`hand_tui::LoaderComponent`] when cancellable is off) between two
//! horizontal border lines, emitting the same visual frame the pi-mono
//! extensions display while waiting on long-running operations.
//!
//! pi-mono's TS source pulls helpers from two sibling components
//! (`DynamicBorder`, `keybinding-hints`) that are queued for later batches.
//! To avoid introducing new public surface that the eventual port would
//! collide with, this file inlines minimal private equivalents:
//!
//! * [`border_line`] mirrors `DynamicBorder::render`.
//! * [`format_cancel_hint`] mirrors `keyHint`'s default formatting.
//!
//! Theming caveat: the TS component reads `border`, `accent`, `muted`, `dim`
//! slots from the coding-agent theme. Until the theme port lands, the
//! defaults below are hardcoded (bright black for borders, cyan for accent,
//! bright black for muted).

use hand_tui::{CancellableLoaderComponent, Component, LoaderComponent};

/// ANSI prefix for the border line — bright-black, matching pi-mono's
/// default `border` slot.
const BORDER_FG: &str = "\x1b[90m";
/// Reset escape.
const RESET: &str = "\x1b[0m";

/// Underlying loader: either cancellable or plain.
enum Inner {
    Cancellable(CancellableLoaderComponent),
    Plain(LoaderComponent),
}

/// Loader wrapped in horizontal borders, suitable for extension UI.
pub struct BorderedLoaderComponent {
    inner: Inner,
    cancellable: bool,
    /// Hint text shown beneath the loader when `cancellable` is true.
    /// Empty string suppresses the hint line entirely.
    cancel_hint: String,
}

impl BorderedLoaderComponent {
    /// Construct a cancellable bordered loader.
    pub fn new_cancellable(message: impl Into<String>) -> Self {
        let message = message.into();
        let mut loader = CancellableLoaderComponent::new(message);
        // pi-mono's CancellableLoader doesn't render its own cancel hint —
        // BorderedLoader supplies it as a separate line. Suppress the inner
        // hint so we don't double up.
        loader.set_cancel_hint("");
        Self {
            inner: Inner::Cancellable(loader),
            cancellable: true,
            cancel_hint: format_cancel_hint("esc", "cancel"),
        }
    }

    /// Construct a non-cancellable bordered loader.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            inner: Inner::Plain(LoaderComponent::new(message)),
            cancellable: false,
            cancel_hint: String::new(),
        }
    }

    /// Override the cancel-hint line. Pass an empty string to suppress.
    pub fn set_cancel_hint(&mut self, hint: impl Into<String>) {
        self.cancel_hint = hint.into();
    }

    /// Update the loader message.
    pub fn set_message(&mut self, message: impl Into<String>) {
        match &mut self.inner {
            Inner::Cancellable(c) => c.set_message(message),
            Inner::Plain(p) => p.set_message(message),
        }
    }

    /// Advance the spinner animation.
    pub fn tick(&mut self) {
        match &mut self.inner {
            Inner::Cancellable(c) => c.tick(),
            Inner::Plain(p) => p.tick(),
        }
    }

    /// Whether the underlying cancellable loader observed a cancel keypress.
    /// Always returns `false` for the non-cancellable variant.
    pub fn is_cancelled(&self) -> bool {
        match &self.inner {
            Inner::Cancellable(c) => c.is_cancelled(),
            Inner::Plain(_) => false,
        }
    }

    /// Whether this loader is the cancellable variant.
    pub fn is_cancellable(&self) -> bool {
        self.cancellable
    }
}

impl Component for BorderedLoaderComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(border_line(width));
        match &self.inner {
            Inner::Cancellable(c) => lines.extend(c.render(width)),
            Inner::Plain(p) => lines.extend(p.render(width)),
        }
        if self.cancellable && !self.cancel_hint.is_empty() {
            lines.push(String::new());
            lines.push(self.cancel_hint.clone());
        }
        lines.push(String::new());
        lines.push(border_line(width));
        lines
    }

    fn handle_input(&mut self, event: &hand_tui::InputEvent) -> hand_tui::HandleResult {
        match &mut self.inner {
            Inner::Cancellable(c) => c.handle_input(event),
            Inner::Plain(_) => hand_tui::HandleResult::Ignored,
        }
    }
}

/// Build a single horizontal border line spanning `width` columns.
fn border_line(width: u16) -> String {
    let len = width.max(1) as usize;
    format!("{BORDER_FG}{}{RESET}", "─".repeat(len))
}

/// Format a key hint as `"<key> <description>"` with dim styling on the key.
fn format_cancel_hint(key: &str, description: &str) -> String {
    // \x1b[2m = dim, \x1b[90m = bright black for the description.
    format!("\x1b[2m{key}\x1b[0m \x1b[90m{description}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellable_renders_borders_and_cancel_hint() {
        let comp = BorderedLoaderComponent::new_cancellable("Working");
        let lines = comp.render(20);
        assert!(lines.len() >= 4, "expected ≥4 lines, got {lines:?}");
        // First and last lines are borders.
        assert!(
            lines[0].contains("─"),
            "first line not a border: {:?}",
            lines[0]
        );
        let last = lines.last().unwrap();
        assert!(last.contains("─"), "last line not a border: {last:?}");
        // Body contains the message and cancel hint.
        let joined = lines.join("\n");
        assert!(joined.contains("Working"), "missing message: {joined:?}");
        assert!(joined.contains("cancel"), "missing cancel hint: {joined:?}");
    }

    #[test]
    fn plain_renders_borders_without_cancel_hint() {
        let comp = BorderedLoaderComponent::new("Loading");
        let lines = comp.render(20);
        let joined = lines.join("\n");
        assert!(joined.contains("Loading"));
        assert!(
            !joined.contains("cancel"),
            "non-cancellable variant should not render a cancel hint: {joined:?}"
        );
        assert!(lines[0].contains("─"));
        assert!(lines.last().unwrap().contains("─"));
    }

    #[test]
    fn border_line_uses_full_width() {
        let line = border_line(10);
        // 10 dashes × ~3 bytes for `─` plus the SGR wrapper.
        assert!(line.contains(BORDER_FG));
        assert!(line.contains(RESET));
        // Visible length matches the requested width.
        assert_eq!(line.matches('─').count(), 10);
    }

    #[test]
    fn tick_advances_inner_loader() {
        let mut comp = BorderedLoaderComponent::new_cancellable("x");
        let before = comp.render(20).join("\n");
        comp.tick();
        let after = comp.render(20).join("\n");
        // Spinner glyph differs frame-to-frame; outputs should diverge.
        assert_ne!(before, after);
    }

    #[test]
    fn is_cancellable_reflects_constructor() {
        assert!(BorderedLoaderComponent::new_cancellable("x").is_cancellable());
        assert!(!BorderedLoaderComponent::new("x").is_cancellable());
    }
}
