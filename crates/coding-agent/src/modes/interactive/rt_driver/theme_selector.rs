//! The rt-native `/theme` selector — the palette picker built on the
//! [overlay runtime](super::overlay).
//!
//! It is a [`SelectorController`]: it produces rt-native styled [`Line`]s and
//! consumes keys while it is the mounted modal overlay. Its behaviour is the theme
//! picker's:
//!
//! - the list is the persistable [`ThemeSetting`](crate::core::settings::ThemeSetting)
//!   choices (`dark` / `light` / `high-contrast` / `system`) — the values the
//!   settings layer round-trips;
//! - **↑/↓ navigate with wrap**;
//! - the **current** theme is checkmarked and pre-selected (VAL-OVERLAY-035 parity);
//! - **Enter** confirms, **Esc** cancels — each emits exactly one [`ThemeOutcome`]
//!   and raises the [`DoneSignal`](super::overlay::DoneSignal).
//!
//! There is **no live preview** (Decision Log parity): the driver persists the pick
//! to settings and lands the `[theme: <name>] saved; restart to apply` status line,
//! and the next launch renders with that palette (VAL-OVERLAY-014). This component
//! is pure UI + pick logic over its constructor inputs.

use std::sync::atomic::Ordering;

use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::HandleOutcome;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use super::overlay::{DoneSignal, SelectorController};

/// The persistable theme names, in menu order. These are exactly the values the
/// settings layer accepts for the `theme` key, so a pick always round-trips.
pub const THEME_NAMES: &[&str] = &["dark", "light", "high-contrast", "system"];

/// Whether `name` is a valid (persistable) theme name, case-insensitively. Used by
/// the driver to distinguish a direct `/theme <name>` apply from the red
/// "unknown theme" guidance (VAL-OVERLAY-018).
#[must_use]
pub fn is_valid_theme(name: &str) -> bool {
    let needle = name.trim().to_ascii_lowercase();
    THEME_NAMES.iter().any(|t| *t == needle)
}

/// Normalise a theme argument to its canonical persistable name, or `None` when it
/// is not a known theme.
#[must_use]
pub fn canonical_theme(name: &str) -> Option<&'static str> {
    let needle = name.trim().to_ascii_lowercase();
    THEME_NAMES.iter().copied().find(|t| *t == needle)
}

/// The outcome the selector emits on its channel — exactly one per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeOutcome {
    /// The user confirmed this theme name (Enter).
    Selected(String),
    /// The user cancelled (Esc) — the current theme is kept.
    Cancelled,
}

/// The rt-native `/theme` picker component.
pub struct ThemeSelector {
    /// The persistable theme names, in menu order.
    themes: Vec<String>,
    /// The current theme name, checkmarked and pre-selected.
    current: String,
    /// The highlighted row, as an index into `themes`.
    selected: usize,
    /// The outcome channel; exactly one [`ThemeOutcome`] on confirm/cancel.
    tx: mpsc::UnboundedSender<ThemeOutcome>,
    /// Raised on the terminal key (Enter/Esc) so the runtime unmounts this.
    done: DoneSignal,
}

impl ThemeSelector {
    /// Build a selector over the persistable theme names, pre-selecting `current`.
    #[must_use]
    pub fn new(
        current: impl Into<String>,
        tx: mpsc::UnboundedSender<ThemeOutcome>,
        done: DoneSignal,
    ) -> Self {
        let current = current.into();
        let themes: Vec<String> = THEME_NAMES.iter().map(|t| (*t).to_string()).collect();
        let selected = themes.iter().position(|t| t == &current).unwrap_or(0);
        Self {
            themes,
            current,
            selected,
            tx,
            done,
        }
    }

    /// The highlighted theme name (test/introspection aid).
    #[must_use]
    pub fn highlighted(&self) -> Option<&str> {
        self.themes.get(self.selected).map(String::as_str)
    }

    /// The highlighted row index (test aid).
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    fn move_up(&mut self) {
        let len = self.themes.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
    }

    fn move_down(&mut self) {
        let len = self.themes.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
    }

    fn confirm(&self) {
        if let Some(name) = self.themes.get(self.selected) {
            let _ = self.tx.send(ThemeOutcome::Selected(name.clone()));
        }
    }

    fn cancel(&self) {
        let _ = self.tx.send(ThemeOutcome::Cancelled);
    }

    fn body_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let muted = Style::default().fg(Color::DarkGray);
        let accent = Style::default().fg(Color::Cyan);
        let success = Style::default().fg(Color::Green);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::styled("Theme".to_string(), muted));
        lines.push(Line::from(String::new()));

        for (i, name) in self.themes.iter().enumerate() {
            let is_selected = i == self.selected;
            let is_current = name == &self.current;

            let mut spans: Vec<Span<'static>> = Vec::new();
            if is_selected {
                spans.push(Span::styled("→ ".to_string(), accent));
                spans.push(Span::styled(
                    name.clone(),
                    accent.add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("  ".to_string()));
                spans.push(Span::raw(name.clone()));
            }
            if is_current {
                spans.push(Span::styled(" ✓ (current)".to_string(), success));
            }
            lines.push(Line::from(spans));
        }

        lines.push(Line::from(String::new()));
        lines.push(Line::styled(
            "  saved on Enter; restart to apply".to_string(),
            muted,
        ));
        lines
    }
}

impl SelectorController for ThemeSelector {
    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body_lines(width)
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        match key.key_id.as_deref() {
            Some("up") | Some("ctrl+k") => {
                self.move_up();
                HandleOutcome::Consumed
            }
            Some("down") | Some("ctrl+j") => {
                self.move_down();
                HandleOutcome::Consumed
            }
            Some("enter") => {
                self.confirm();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            Some("escape") => {
                self.cancel();
                self.done.store(true, Ordering::SeqCst);
                HandleOutcome::Consumed
            }
            // Modal: own every key so none reaches the editor (VAL-OVERLAY-005).
            _ => HandleOutcome::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key_id(id: &str) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        }
    }

    fn selector(
        current: &str,
    ) -> (
        ThemeSelector,
        mpsc::UnboundedReceiver<ThemeOutcome>,
        DoneSignal,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let done = super::super::overlay::new_done_signal();
        (ThemeSelector::new(current, tx, done.clone()), rx, done)
    }

    fn body_text(sel: &ThemeSelector) -> String {
        sel.body_lines(80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<ThemeOutcome>) -> Option<ThemeOutcome> {
        rx.try_recv().ok()
    }

    #[test]
    fn renders_all_persistable_themes_with_current_marker() {
        let (sel, _rx, _done) = selector("light");
        let body = body_text(&sel);
        for name in ["dark", "light", "high-contrast", "system"] {
            assert!(body.contains(name), "missing theme {name}: {body}");
        }
        assert!(body.contains("(current)"), "current marker missing: {body}");
        // No-live-preview parity: the dialog states the restart-to-apply contract.
        assert!(
            body.contains("restart to apply"),
            "restart hint missing: {body}"
        );
    }

    #[test]
    fn current_theme_is_preselected_and_bare_enter_keeps_it() {
        let (mut sel, mut rx, done) = selector("light");
        assert_eq!(sel.highlighted(), Some("light"), "current is preselected");
        sel.handle_key(&key_id("enter"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(ThemeOutcome::Selected("light".into())));
    }

    #[test]
    fn down_then_enter_selects_the_next_theme() {
        let (mut sel, mut rx, _done) = selector("dark"); // idx 0
        sel.handle_key(&key_id("down")); // -> light
        sel.handle_key(&key_id("enter"));
        assert_eq!(drain(&mut rx), Some(ThemeOutcome::Selected("light".into())));
    }

    #[test]
    fn navigation_wraps() {
        let (mut sel, _rx, _done) = selector("dark");
        sel.handle_key(&key_id("up")); // wrap to last
        assert_eq!(sel.highlighted(), Some("system"));
    }

    #[test]
    fn escape_emits_cancelled() {
        let (mut sel, mut rx, done) = selector("dark");
        sel.handle_key(&key_id("escape"));
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(drain(&mut rx), Some(ThemeOutcome::Cancelled));
    }

    #[test]
    fn unknown_current_theme_does_not_panic_and_selects_first() {
        let (mut sel, mut rx, _done) = selector("nonexistent");
        assert_eq!(sel.selected_index(), 0);
        sel.handle_key(&key_id("enter"));
        assert_eq!(drain(&mut rx), Some(ThemeOutcome::Selected("dark".into())));
    }

    #[test]
    fn theme_validation_helpers() {
        assert!(is_valid_theme("dark"));
        assert!(is_valid_theme("HIGH-CONTRAST"));
        assert!(!is_valid_theme("nosuch"));
        assert_eq!(canonical_theme("LIGHT"), Some("light"));
        assert_eq!(canonical_theme("nope"), None);
    }
}
