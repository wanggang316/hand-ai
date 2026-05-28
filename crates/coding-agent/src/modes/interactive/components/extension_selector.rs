//! List-of-options selector dialog used by extensions.
//!
//! Renders a framed dialog with a title, the option list (selected row
//! marked with `→`), and a hint line. Drives selection with `tui.select.up`
//! / `tui.select.down` / `tui.select.confirm` / `tui.select.cancel`, plus
//! the vim-style `j` / `k` shortcuts.
//!
//! Events ([`ExtensionSelectorEvent::Select`] / `Cancel`) flow through
//! an [`std::sync::mpsc::Sender`] supplied at construction. Theme
//! colouring of the
//! title and selected row is optional; pass `None` for plain output and
//! let the driver wrap it.

use std::sync::mpsc::Sender;
use std::time::Duration;

use hand_tui::keybindings::Keybinding;
use hand_tui::keys::matches_key;
use hand_tui::tui::{Component, HandleResult, InputEvent};
use hand_tui::utils::visible_width;
use hand_tui::{KeybindingsManager, get_keybindings};

use super::countdown_timer::CountdownTimer;
use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::{key_hint_for, raw_key_hint};
use crate::modes::interactive::theme::{Theme, ThemeColor};

/// Events surfaced by [`ExtensionSelectorComponent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionSelectorEvent {
    /// User confirmed `option`.
    Select(String),
    /// User cancelled or the countdown timer expired.
    Cancel,
}

/// Selector dialog with arrow-key navigation.
pub struct ExtensionSelectorComponent {
    base_title: String,
    title_line: String,
    options: Vec<String>,
    selected_index: usize,
    hint: String,
    border: DynamicBorderComponent,
    countdown: Option<CountdownTimer>,
    events: Sender<ExtensionSelectorEvent>,
    theme: Option<Theme>,
}

impl ExtensionSelectorComponent {
    /// Build a selector with `title` and the supplied `options`. An empty
    /// option list is allowed; confirm becomes a no-op until options are
    /// pushed via [`Self::set_options`].
    pub fn new(
        title: impl Into<String>,
        options: Vec<String>,
        timeout: Option<Duration>,
        events: Sender<ExtensionSelectorEvent>,
        theme: Option<Theme>,
    ) -> Self {
        let title = title.into();
        let hint = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate"),
            key_hint_for("tui.select.confirm", "select"),
            key_hint_for("tui.select.cancel", "cancel"),
        );

        let countdown = timeout.and_then(|d| {
            if d.as_millis() == 0 {
                return None;
            }
            let expire_tx = events.clone();
            Some(CountdownTimer::new(
                d,
                |_| {},
                move || {
                    let _ = expire_tx.send(ExtensionSelectorEvent::Cancel);
                },
            ))
        });

        let mut me = Self {
            base_title: title,
            title_line: String::new(),
            options,
            selected_index: 0,
            hint,
            border: DynamicBorderComponent::new(),
            countdown,
            events,
            theme,
        };
        me.refresh_title();
        me
    }

    /// Replace the option list. Resets the selection to row 0.
    pub fn set_options(&mut self, options: Vec<String>) {
        self.options = options;
        self.selected_index = 0;
    }

    /// Drive the optional countdown by one second.
    pub fn tick_countdown(&mut self) {
        if let Some(cd) = self.countdown.as_mut() {
            cd.tick();
            self.refresh_title();
        }
    }

    /// Drop the timer without firing its expire callback.
    pub fn dispose(&mut self) {
        if let Some(cd) = self.countdown.as_mut() {
            cd.dispose();
        }
    }

    /// Currently-highlighted option, if any.
    pub fn selected(&self) -> Option<&str> {
        self.options.get(self.selected_index).map(String::as_str)
    }

    fn refresh_title(&mut self) {
        let body = match self
            .countdown
            .as_ref()
            .map(CountdownTimer::remaining_seconds)
        {
            Some(s) if s >= 0 => format!("{} ({}s)", self.base_title, s),
            _ => self.base_title.clone(),
        };
        let bold = format!("\x1b[1m{}\x1b[22m", body);
        let coloured = match &self.theme {
            Some(theme) => theme.fg(ThemeColor::Accent, &bold).unwrap_or(bold),
            None => bold,
        };
        self.title_line = coloured;
    }

    fn render_row(&self, idx: usize) -> String {
        let label = &self.options[idx];
        let selected = idx == self.selected_index;
        match (&self.theme, selected) {
            (Some(theme), true) => {
                let arrow = theme
                    .fg(ThemeColor::Accent, "→ ")
                    .unwrap_or_else(|_| "→ ".into());
                let body = theme
                    .fg(ThemeColor::Accent, label)
                    .unwrap_or_else(|_| label.clone());
                format!("{}{}", arrow, body)
            }
            (Some(theme), false) => {
                let body = theme
                    .fg(ThemeColor::Text, label)
                    .unwrap_or_else(|_| label.clone());
                format!("  {}", body)
            }
            (None, true) => format!("→ {}", label),
            (None, false) => format!("  {}", label),
        }
    }

    /// Accept any `InputEvent` variant the Tui pipeline can emit.
    /// Esc-prefixed sequences and single control bytes arrive as
    /// `InputEvent::Key` in production, not `Raw` — see the #56 fix.
    fn raw_key(event: &InputEvent) -> Option<String> {
        match event {
            InputEvent::Raw(s) | InputEvent::Paste(s) => Some(s.clone()),
            InputEvent::Key(key) => hand_tui::key_to_canonical_bytes(key),
            _ => None,
        }
    }

    fn navigate(&mut self, kb: &KeybindingsManager, data: &str) -> bool {
        if kb.matches(data, Keybinding::SelectUp) || data == "k" {
            if self.selected_index > 0 {
                self.selected_index -= 1;
            }
            return true;
        }
        if kb.matches(data, Keybinding::SelectDown) || data == "j" {
            if !self.options.is_empty() && self.selected_index + 1 < self.options.len() {
                self.selected_index += 1;
            }
            return true;
        }
        false
    }
}

impl Component for ExtensionSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.push(String::new());
        out.push(pad_line(&self.title_line, width));
        out.push(String::new());
        for i in 0..self.options.len() {
            out.push(pad_line(&self.render_row(i), width));
        }
        out.push(String::new());
        out.push(pad_line(&self.hint, width));
        out.push(String::new());
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let Some(data) = Self::raw_key(event) else {
            return HandleResult::Ignored;
        };

        let kb = get_keybindings();

        if self.navigate(&kb, &data) {
            return HandleResult::Handled;
        }

        if kb.matches(&data, Keybinding::SelectConfirm)
            || matches_key(&data, "enter")
            || data == "\n"
        {
            if let Some(option) = self.selected().map(str::to_string) {
                let _ = self.events.send(ExtensionSelectorEvent::Select(option));
            }
            return HandleResult::Handled;
        }

        if kb.matches(&data, Keybinding::SelectCancel) {
            let _ = self.events.send(ExtensionSelectorEvent::Cancel);
            return HandleResult::Handled;
        }

        HandleResult::Ignored
    }
}

fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{}{}", line, " ".repeat(target - current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn make_event(data: &str) -> InputEvent {
        InputEvent::Raw(data.to_string())
    }

    #[test]
    fn renders_title_options_and_hint() {
        let (tx, _rx) = mpsc::channel();
        let comp = ExtensionSelectorComponent::new(
            "Pick:",
            vec!["Alpha".into(), "Beta".into()],
            None,
            tx,
            None,
        );
        let lines = comp.render(40);
        assert!(lines.iter().any(|l| l.contains("Pick:")));
        assert!(lines.iter().any(|l| l.contains("Alpha")));
        assert!(lines.iter().any(|l| l.contains("Beta")));
        assert!(lines.iter().any(|l| l.contains("navigate")));
        assert!(lines.iter().any(|l| l.contains("select")));
        assert!(lines.iter().any(|l| l.contains("cancel")));
    }

    #[test]
    fn first_row_is_selected_initially() {
        let (tx, _rx) = mpsc::channel();
        let comp =
            ExtensionSelectorComponent::new("t", vec!["one".into(), "two".into()], None, tx, None);
        assert_eq!(comp.selected(), Some("one"));
        let lines = comp.render(20);
        // Selected row carries an arrow prefix.
        assert!(lines.iter().any(|l| l.contains("→ one")));
        assert!(lines.iter().any(|l| l.contains("  two")));
    }

    #[test]
    fn down_arrow_advances_selection() {
        let (tx, _rx) = mpsc::channel();
        let mut comp =
            ExtensionSelectorComponent::new("t", vec!["one".into(), "two".into()], None, tx, None);
        // ANSI down arrow.
        comp.handle_input(&make_event("\x1b[B"));
        assert_eq!(comp.selected(), Some("two"));
    }

    #[test]
    fn up_arrow_clamps_at_zero() {
        let (tx, _rx) = mpsc::channel();
        let mut comp =
            ExtensionSelectorComponent::new("t", vec!["one".into(), "two".into()], None, tx, None);
        comp.handle_input(&make_event("\x1b[A"));
        assert_eq!(comp.selected(), Some("one"));
    }

    #[test]
    fn vim_keys_navigate() {
        let (tx, _rx) = mpsc::channel();
        let mut comp = ExtensionSelectorComponent::new(
            "t",
            vec!["one".into(), "two".into(), "three".into()],
            None,
            tx,
            None,
        );
        comp.handle_input(&make_event("j"));
        comp.handle_input(&make_event("j"));
        assert_eq!(comp.selected(), Some("three"));
        comp.handle_input(&make_event("k"));
        assert_eq!(comp.selected(), Some("two"));
    }

    #[test]
    fn enter_emits_select() {
        let (tx, rx) = mpsc::channel();
        let mut comp =
            ExtensionSelectorComponent::new("t", vec!["one".into(), "two".into()], None, tx, None);
        comp.handle_input(&make_event("\r"));
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionSelectorEvent::Select("one".to_string()));
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, rx) = mpsc::channel();
        let mut comp = ExtensionSelectorComponent::new("t", vec!["one".into()], None, tx, None);
        comp.handle_input(&make_event("\x1b"));
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionSelectorEvent::Cancel);
    }

    #[test]
    fn empty_options_confirm_is_noop() {
        let (tx, rx) = mpsc::channel();
        let mut comp = ExtensionSelectorComponent::new("t", Vec::new(), None, tx, None);
        comp.handle_input(&make_event("\r"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn countdown_expiry_cancels() {
        let (tx, rx) = mpsc::channel();
        let mut comp = ExtensionSelectorComponent::new(
            "t",
            vec!["one".into()],
            Some(Duration::from_secs(1)),
            tx,
            None,
        );
        comp.tick_countdown();
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionSelectorEvent::Cancel);
    }

    #[test]
    fn set_options_resets_selection() {
        let (tx, _rx) = mpsc::channel();
        let mut comp =
            ExtensionSelectorComponent::new("t", vec!["one".into(), "two".into()], None, tx, None);
        comp.handle_input(&make_event("j"));
        comp.set_options(vec!["x".into(), "y".into()]);
        assert_eq!(comp.selected(), Some("x"));
    }
}
