//! Selector for forking sessions from a chosen user message.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/user-message-selector.ts`.
//!
//! Renders a header + a scrolling list of past user messages (10 visible
//! at a time, two lines per message). Up / down wrap around. Confirm
//! emits [`UserMessageSelectorEvent::Select`] with the entry id; cancel
//! emits [`UserMessageSelectorEvent::Cancel`].
//!
//! pi-mono auto-cancels via `setTimeout(..., 100)` when the message list
//! is empty. The Rust port surfaces the same outcome by emitting `Cancel`
//! eagerly during construction; the driver can short-circuit the dialog
//! the same way it would on user input.

use std::sync::mpsc::Sender;

use hand_tui::keybindings::Keybinding;
use hand_tui::tui::{Component, HandleResult, InputEvent};
use hand_tui::utils::{truncate_to_width, visible_width};
use hand_tui::{KeybindingsManager, get_keybindings};

use super::dynamic_border::DynamicBorderComponent;
use crate::modes::interactive::theme::{Theme, ThemeColor};

/// One row in the selector.
#[derive(Debug, Clone)]
pub struct UserMessageItem {
    /// Stable id used by the driver to identify the chosen branching point.
    pub id: String,
    /// User-message text. Multi-line strings are normalised to a single
    /// line at render time.
    pub text: String,
    /// Optional human-readable timestamp; reserved for future formatting
    /// (matches the TS field, currently unused at render time).
    pub timestamp: Option<String>,
}

/// Events surfaced by [`UserMessageSelectorComponent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserMessageSelectorEvent {
    /// User confirmed the message identified by `entry_id`.
    Select { entry_id: String },
    /// User cancelled, or the list was empty.
    Cancel,
}

/// Maximum visible rows in the scrolling list (matches pi-mono's
/// `maxVisible = 10`).
const MAX_VISIBLE: usize = 10;

/// Selector dialog implementing fork-from-message UX.
pub struct UserMessageSelectorComponent {
    messages: Vec<UserMessageItem>,
    selected_index: usize,
    border: DynamicBorderComponent,
    theme: Option<Theme>,
    events: Sender<UserMessageSelectorEvent>,
}

impl UserMessageSelectorComponent {
    /// Build a selector. If `initial_selected_id` matches a message, that
    /// row starts highlighted; otherwise the most recent message is.
    /// Empty `messages` lists eagerly emit a `Cancel` event.
    pub fn new(
        messages: Vec<UserMessageItem>,
        initial_selected_id: Option<&str>,
        events: Sender<UserMessageSelectorEvent>,
        theme: Option<Theme>,
    ) -> Self {
        let selected_index = initial_selected_id
            .and_then(|id| messages.iter().position(|m| m.id == id))
            .unwrap_or_else(|| messages.len().saturating_sub(1));

        if messages.is_empty() {
            let _ = events.send(UserMessageSelectorEvent::Cancel);
        }

        Self {
            messages,
            selected_index,
            border: DynamicBorderComponent::new(),
            theme,
            events,
        }
    }

    /// Currently-highlighted entry id, if any.
    pub fn selected_id(&self) -> Option<&str> {
        self.messages
            .get(self.selected_index)
            .map(|m| m.id.as_str())
    }

    fn fg(&self, color: ThemeColor, text: &str) -> String {
        match &self.theme {
            Some(theme) => theme.fg(color, text).unwrap_or_else(|_| text.to_string()),
            None => text.to_string(),
        }
    }

    fn bold(&self, text: &str) -> String {
        match &self.theme {
            Some(theme) => theme.bold(text),
            None => format!("\x1b[1m{}\x1b[22m", text),
        }
    }

    fn render_list(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        if self.messages.is_empty() {
            lines.push(self.fg(ThemeColor::Muted, "  No user messages found"));
            return lines;
        }

        let half = MAX_VISIBLE / 2;
        let max_start = self.messages.len().saturating_sub(MAX_VISIBLE);
        let start = self.selected_index.saturating_sub(half).min(max_start);
        let end = (start + MAX_VISIBLE).min(self.messages.len());
        let max_msg_width = (width as usize).saturating_sub(2);

        for i in start..end {
            let m = &self.messages[i];
            let normalised: String = m.text.replace('\n', " ").trim().to_string();
            let truncated = truncate_to_width(&normalised, max_msg_width);
            let is_selected = i == self.selected_index;
            let cursor = if is_selected {
                self.fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };
            let body = if is_selected {
                self.bold(&truncated)
            } else {
                truncated
            };
            lines.push(format!("{}{}", cursor, body));
            let metadata = format!("  Message {} of {}", i + 1, self.messages.len());
            lines.push(self.fg(ThemeColor::Muted, &metadata));
            lines.push(String::new());
        }

        if start > 0 || end < self.messages.len() {
            let scroll = format!("  ({}/{})", self.selected_index + 1, self.messages.len());
            lines.push(self.fg(ThemeColor::Muted, &scroll));
        }

        lines
    }

    fn raw_key(event: &InputEvent) -> Option<&str> {
        match event {
            InputEvent::Raw(s) | InputEvent::Paste(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn navigate(&mut self, kb: &KeybindingsManager, data: &str) -> bool {
        if self.messages.is_empty() {
            return false;
        }
        if kb.matches(data, Keybinding::SelectUp) {
            self.selected_index = if self.selected_index == 0 {
                self.messages.len() - 1
            } else {
                self.selected_index - 1
            };
            return true;
        }
        if kb.matches(data, Keybinding::SelectDown) {
            self.selected_index = if self.selected_index + 1 >= self.messages.len() {
                0
            } else {
                self.selected_index + 1
            };
            return true;
        }
        false
    }
}

impl Component for UserMessageSelectorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = vec![
            String::new(),
            pad(&self.bold("Fork from Message"), width),
            pad(
                &self.fg(
                    ThemeColor::Muted,
                    "Select a user message to copy the active path up to that point into a new session",
                ),
                width,
            ),
            String::new(),
        ];
        out.extend(self.border.render(width));
        out.push(String::new());
        for line in self.render_list(width) {
            out.push(pad(&line, width));
        }
        out.push(String::new());
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        let Some(data) = Self::raw_key(event) else {
            return HandleResult::Ignored;
        };
        let kb = get_keybindings();
        if self.navigate(&kb, data) {
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectConfirm) {
            if let Some(id) = self.selected_id().map(str::to_string) {
                let _ = self
                    .events
                    .send(UserMessageSelectorEvent::Select { entry_id: id });
            }
            return HandleResult::Handled;
        }
        if kb.matches(data, Keybinding::SelectCancel) {
            let _ = self.events.send(UserMessageSelectorEvent::Cancel);
            return HandleResult::Handled;
        }
        HandleResult::Ignored
    }
}

fn pad(line: &str, width: u16) -> String {
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

    fn item(id: &str, text: &str) -> UserMessageItem {
        UserMessageItem {
            id: id.to_string(),
            text: text.to_string(),
            timestamp: None,
        }
    }

    fn make_event(data: &str) -> InputEvent {
        InputEvent::Raw(data.to_string())
    }

    #[test]
    fn empty_messages_emit_cancel_eagerly() {
        let (tx, rx) = mpsc::channel();
        let _comp = UserMessageSelectorComponent::new(Vec::new(), None, tx, None);
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, UserMessageSelectorEvent::Cancel);
    }

    #[test]
    fn defaults_to_most_recent_message() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "first"), item("b", "second"), item("c", "third")];
        let comp = UserMessageSelectorComponent::new(messages, None, tx, None);
        assert_eq!(comp.selected_id(), Some("c"));
    }

    #[test]
    fn initial_id_is_respected() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "1"), item("b", "2"), item("c", "3")];
        let comp = UserMessageSelectorComponent::new(messages, Some("b"), tx, None);
        assert_eq!(comp.selected_id(), Some("b"));
    }

    #[test]
    fn down_wraps_to_top() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "1"), item("b", "2")];
        let mut comp = UserMessageSelectorComponent::new(messages, Some("b"), tx, None);
        comp.handle_input(&make_event("\x1b[B"));
        assert_eq!(comp.selected_id(), Some("a"));
    }

    #[test]
    fn up_wraps_to_bottom() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "1"), item("b", "2")];
        let mut comp = UserMessageSelectorComponent::new(messages, Some("a"), tx, None);
        comp.handle_input(&make_event("\x1b[A"));
        assert_eq!(comp.selected_id(), Some("b"));
    }

    #[test]
    fn enter_selects_current() {
        let (tx, rx) = mpsc::channel();
        let messages = vec![item("a", "1"), item("b", "2")];
        let mut comp = UserMessageSelectorComponent::new(messages, Some("a"), tx, None);
        comp.handle_input(&make_event("\r"));
        let evt = rx.recv().unwrap();
        assert_eq!(
            evt,
            UserMessageSelectorEvent::Select {
                entry_id: "a".into()
            }
        );
    }

    #[test]
    fn escape_cancels() {
        let (tx, rx) = mpsc::channel();
        let messages = vec![item("a", "1")];
        let mut comp = UserMessageSelectorComponent::new(messages, None, tx, None);
        comp.handle_input(&make_event("\x1b"));
        let evt = rx.recv().unwrap();
        assert_eq!(evt, UserMessageSelectorEvent::Cancel);
    }

    #[test]
    fn renders_header_and_messages() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "hello"), item("b", "world")];
        let comp = UserMessageSelectorComponent::new(messages, None, tx, None);
        let lines = comp.render(40);
        assert!(lines.iter().any(|l| l.contains("Fork from Message")));
        assert!(lines.iter().any(|l| l.contains("hello")));
        assert!(lines.iter().any(|l| l.contains("world")));
        // Selected indicator should appear (most-recent default).
        assert!(lines.iter().any(|l| l.contains("›")));
    }

    #[test]
    fn empty_list_renders_placeholder() {
        let (tx, _rx) = mpsc::channel();
        let comp = UserMessageSelectorComponent::new(Vec::new(), None, tx, None);
        let lines = comp.render(40);
        assert!(lines.iter().any(|l| l.contains("No user messages found")));
    }

    #[test]
    fn renders_scroll_indicator_when_overflowing() {
        let (tx, _rx) = mpsc::channel();
        let messages: Vec<UserMessageItem> = (0..15)
            .map(|i| item(&format!("id{}", i), &format!("msg {}", i)))
            .collect();
        let comp = UserMessageSelectorComponent::new(messages, None, tx, None);
        let lines = comp.render(40);
        // Should show the position indicator like "(15/15)".
        assert!(lines.iter().any(|l| l.contains("/15")));
    }

    #[test]
    fn newlines_in_message_collapsed_to_single_line() {
        let (tx, _rx) = mpsc::channel();
        let messages = vec![item("a", "line one\nline two")];
        let comp = UserMessageSelectorComponent::new(messages, None, tx, None);
        let lines = comp.render(40);
        // The combined text "line one line two" should appear on one row.
        assert!(lines.iter().any(|l| l.contains("line one line two")));
    }
}
