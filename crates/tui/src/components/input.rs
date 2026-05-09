//! Input component — single-line text input with cursor and history.
//
// audit: M3.T5 — parity reviewed against pi-tui/input.ts on 2026-05-07.
// non-goal: TS's `Input` ships with kill-ring (yank/yank-pop), per-input undo
// stack, bracketed-paste buffering, IME `CURSOR_MARKER`, Kitty CSI-u
// printable decoding, and word-aware deletes. The Rust port keeps `Input` as
// a lightweight single-line widget; the heavyweight surface lives on
// `EditorComponent` (which already implements those features in Rust). Adding
// them here would duplicate ~400 lines of editor logic for marginal gain.

use unicode_segmentation::UnicodeSegmentation;

use crate::keys::{Key, KeyName, parse_key};
use crate::tui::{CURSOR_MARKER, Component, Focusable, HandleResult, InputEvent};
use crate::utils;

/// Callback type for input submission.
pub type OnSubmit = Box<dyn Fn(&str) + Send>;
/// Callback type for escape.
pub type OnEscape = Box<dyn Fn() + Send>;

/// Single-line text input with horizontal scrolling.
pub struct InputComponent {
    text: String,
    cursor: usize, // byte index into text
    focused: bool,
    placeholder: String,
    prefix: String,
    scroll_offset: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    on_submit: Option<OnSubmit>,
    on_escape: Option<OnEscape>,
}

impl InputComponent {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            focused: true,
            placeholder: String::new(),
            prefix: String::new(),
            scroll_offset: 0,
            history: Vec::new(),
            history_index: None,
            on_submit: None,
            on_escape: None,
        }
    }

    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    pub fn set_on_submit(&mut self, callback: OnSubmit) {
        self.on_submit = Some(callback);
    }

    pub fn set_on_escape(&mut self, callback: OnEscape) {
        self.on_escape = Some(callback);
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.scroll_offset = 0;
    }

    pub fn add_history(&mut self, entry: impl Into<String>) {
        self.history.push(entry.into());
    }

    /// Character index from byte cursor position.
    fn char_index(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Move cursor left by one character.
    fn move_left(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cursor = prev;
        }
    }

    /// Move cursor right by one character.
    fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.cursor = next;
        }
    }

    /// Delete character before cursor.
    fn delete_back(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete character at cursor.
    fn delete_forward(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.text.drain(self.cursor..next);
        }
    }

    /// Delete from cursor to end of line.
    fn kill_line(&mut self) {
        self.text.truncate(self.cursor);
    }

    /// Delete from start to cursor.
    fn kill_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Insert text at cursor.
    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }
}

impl Default for InputComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for InputComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let prefix_width = utils::visible_width(&self.prefix);
        let available = (width as usize).saturating_sub(prefix_width + 1); // +1 for cursor

        let line = if self.text.is_empty() {
            let placeholder_to_show = if utils::visible_width(&self.placeholder) > available {
                utils::truncate_to_width(&self.placeholder, available)
            } else {
                self.placeholder.clone()
            };
            if self.focused {
                if placeholder_to_show.is_empty() {
                    format!("{}{}\x1b[7m \x1b[0m", self.prefix, CURSOR_MARKER)
                } else {
                    format!(
                        "{}{}\x1b[7m \x1b[0m\x1b[90m{}\x1b[0m",
                        self.prefix, CURSOR_MARKER, placeholder_to_show
                    )
                }
            } else if placeholder_to_show.is_empty() {
                self.prefix.clone()
            } else {
                format!("{}\x1b[90m{}\x1b[0m", self.prefix, placeholder_to_show)
            }
        } else {
            let text_to_show = if utils::visible_width(&self.text) > available {
                utils::truncate_to_width(&self.text, available)
            } else {
                self.text.clone()
            };
            if self.focused {
                let split = self.cursor.min(text_to_show.len());
                let (before, after) = text_to_show.split_at(split);
                if after.is_empty() {
                    format!(
                        "{}{}{}\x1b[7m \x1b[0m",
                        self.prefix, before, CURSOR_MARKER
                    )
                } else {
                    let first = after.graphemes(true).next().unwrap_or(after);
                    let rest = &after[first.len()..];
                    format!(
                        "{}{}{}\x1b[7m{}\x1b[0m{}",
                        self.prefix, before, CURSOR_MARKER, first, rest
                    )
                }
            } else {
                format!("{}{}", self.prefix, text_to_show)
            }
        };

        vec![line]
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        if !self.focused {
            return HandleResult::Ignored;
        }
        match event {
            InputEvent::Key(key) => self.handle_key(key),
            InputEvent::Raw(data) | InputEvent::Paste(data) => self.handle_key(&parse_key(data)),
            _ => HandleResult::Ignored,
        }
    }
}

impl InputComponent {
    fn handle_key(&mut self, key: &Key) -> HandleResult {
        if key.is_release {
            return HandleResult::Ignored;
        }

        match (&key.name, &key.modifiers) {
            (KeyName::Enter, _) => {
                if let Some(cb) = &self.on_submit {
                    cb(&self.text);
                }
                HandleResult::Handled
            }
            (KeyName::Escape, _) => {
                if let Some(cb) = &self.on_escape {
                    cb();
                }
                HandleResult::Handled
            }
            (KeyName::Backspace, _) => {
                self.delete_back();
                HandleResult::Handled
            }
            (KeyName::Delete, _) => {
                self.delete_forward();
                HandleResult::Handled
            }
            (KeyName::Left, m) if m.ctrl => {
                // Move to previous word
                while self.cursor > 0 {
                    self.move_left();
                    if self.cursor == 0
                        || self.text[..self.cursor]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_whitespace())
                    {
                        break;
                    }
                }
                HandleResult::Handled
            }
            (KeyName::Right, m) if m.ctrl => {
                // Move to next word
                while self.cursor < self.text.len() {
                    self.move_right();
                    if self.cursor >= self.text.len()
                        || self.text[self.cursor..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_whitespace())
                    {
                        break;
                    }
                }
                HandleResult::Handled
            }
            (KeyName::Left, _) => {
                self.move_left();
                HandleResult::Handled
            }
            (KeyName::Right, _) => {
                self.move_right();
                HandleResult::Handled
            }
            (KeyName::Home, _) => {
                self.cursor = 0;
                HandleResult::Handled
            }
            (KeyName::End, _) => {
                self.cursor = self.text.len();
                HandleResult::Handled
            }
            (KeyName::Char('k'), m) if m.ctrl => {
                self.kill_line();
                HandleResult::Handled
            }
            (KeyName::Char('u'), m) if m.ctrl => {
                self.kill_to_start();
                HandleResult::Handled
            }
            (KeyName::Up, _) => {
                // History navigation
                if !self.history.is_empty() {
                    let idx = match self.history_index {
                        Some(i) if i > 0 => i - 1,
                        None => self.history.len() - 1,
                        _ => return HandleResult::Handled,
                    };
                    self.history_index = Some(idx);
                    self.text = self.history[idx].clone();
                    self.cursor = self.text.len();
                }
                HandleResult::Handled
            }
            (KeyName::Down, _) => {
                if let Some(idx) = self.history_index {
                    if idx + 1 < self.history.len() {
                        self.history_index = Some(idx + 1);
                        self.text = self.history[idx + 1].clone();
                    } else {
                        self.history_index = None;
                        self.text.clear();
                    }
                    self.cursor = self.text.len();
                }
                HandleResult::Handled
            }
            (KeyName::Char(ch), m) if !m.ctrl && !m.alt => {
                self.insert(&ch.to_string());
                HandleResult::Handled
            }
            _ => HandleResult::Ignored,
        }
    }
}

impl Focusable for InputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        if self.focused {
            let prefix_width = utils::visible_width(&self.prefix);
            let cursor_col = prefix_width + self.char_index();
            Some((cursor_col as u16, 0))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_basic_render() {
        let input = InputComponent::new();
        let lines = input.render(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_input_with_text() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        assert_eq!(input.text(), "hello");
        let lines = input.render(80);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_input_type_characters() {
        let mut input = InputComponent::new();
        input.handle_input(&InputEvent::Raw("h".into()));
        input.handle_input(&InputEvent::Raw("i".into()));
        assert_eq!(input.text(), "hi");
    }

    #[test]
    fn test_input_backspace() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        input.handle_input(&InputEvent::Raw("\x7f".into())); // Backspace
        assert_eq!(input.text(), "hell");
    }

    #[test]
    fn test_input_delete() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        input.cursor = 0;
        input.handle_input(&InputEvent::Raw("\x1b[3~".into())); // Delete
        assert_eq!(input.text(), "ello");
    }

    #[test]
    fn test_input_cursor_movement() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        assert_eq!(input.cursor, 5);

        input.handle_input(&InputEvent::Raw("\x1b[D".into())); // Left
        assert_eq!(input.cursor, 4);

        input.handle_input(&InputEvent::Raw("\x1b[C".into())); // Right
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn test_input_home_end() {
        let mut input = InputComponent::new();
        input.set_text("hello");

        input.handle_input(&InputEvent::Raw("\x1b[H".into())); // Home
        assert_eq!(input.cursor, 0);

        input.handle_input(&InputEvent::Raw("\x1b[F".into())); // End
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn test_input_clear() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        input.clear();
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_input_history() {
        let mut input = InputComponent::new();
        input.add_history("cmd1");
        input.add_history("cmd2");

        input.handle_input(&InputEvent::Raw("\x1b[A".into())); // Up
        assert_eq!(input.text(), "cmd2");

        input.handle_input(&InputEvent::Raw("\x1b[A".into())); // Up
        assert_eq!(input.text(), "cmd1");

        input.handle_input(&InputEvent::Raw("\x1b[B".into())); // Down
        assert_eq!(input.text(), "cmd2");
    }

    #[test]
    fn test_input_placeholder() {
        let input = InputComponent::new().with_placeholder("Type here...");
        let lines = input.render(80);
        assert!(lines[0].contains("Type here..."));
    }

    #[test]
    fn test_input_prefix() {
        let mut input = InputComponent::new().with_prefix("> ");
        input.set_text("hello");
        let lines = input.render(80);
        assert!(lines[0].starts_with("> "));
    }

    #[test]
    fn test_input_focus() {
        let mut input = InputComponent::new();
        assert!(input.focused());
        input.set_focused(false);
        assert!(!input.focused());
        assert_eq!(
            input.handle_input(&InputEvent::Raw("a".into())),
            HandleResult::Ignored
        );
    }

    #[test]
    fn test_input_emits_cursor_marker_when_focused() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        let lines = input.render(80);
        assert!(
            lines[0].contains(CURSOR_MARKER),
            "focused input should emit CURSOR_MARKER"
        );
    }

    #[test]
    fn test_input_omits_cursor_marker_when_unfocused() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        input.set_focused(false);
        let lines = input.render(80);
        assert!(
            !lines[0].contains(CURSOR_MARKER),
            "unfocused input should not emit CURSOR_MARKER"
        );
    }

    #[test]
    fn test_input_marker_position_tracks_cursor() {
        let mut input = InputComponent::new();
        input.set_text("hello");
        input.cursor = 2; // between "he" and "llo"
        let lines = input.render(80);
        let marker_idx = lines[0].find(CURSOR_MARKER).expect("marker present");
        let prefix = &lines[0][..marker_idx];
        let stripped = utils::strip_ansi(prefix);
        assert_eq!(stripped, "he", "marker should sit after the first 2 chars");
    }

    #[test]
    fn test_input_cursor_position() {
        let mut input = InputComponent::new().with_prefix("> ");
        input.set_text("hi");
        let pos = input.cursor_position();
        assert!(pos.is_some());
        let (col, row) = pos.unwrap();
        assert_eq!(col, 4); // "> " (2) + "hi" (2)
        assert_eq!(row, 0);
    }
}
