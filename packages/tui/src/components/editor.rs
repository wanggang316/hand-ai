//! Editor component — multi-line text editor with cursor, scrolling, and undo.

use crate::keys::{KeyName, parse_key};
use crate::tui::{Component, Focusable, HandleResult};
use crate::utils;

/// Multi-line text editor with scrolling, word wrap, and basic editing.
pub struct EditorComponent {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize, // character index within line
    focused: bool,
    viewport_top: usize,
    viewport_height: usize,
    border: bool,
    undo_stack: Vec<EditorState>,
    redo_stack: Vec<EditorState>,
}

#[derive(Clone)]
struct EditorState {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

impl EditorComponent {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            focused: true,
            viewport_top: 0,
            viewport_height: 10,
            border: true,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn with_viewport_height(mut self, height: usize) -> Self {
        self.viewport_height = height;
        self
    }

    pub fn with_border(mut self, border: bool) -> Self {
        self.border = border;
        self
    }

    /// Get all text as a single string.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Set the editor content.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.viewport_top = 0;
    }

    /// Line count.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Current cursor position (line, col).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    fn save_undo(&mut self) {
        self.undo_stack.push(EditorState {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
        });
        self.redo_stack.clear();
    }

    fn undo(&mut self) {
        if let Some(state) = self.undo_stack.pop() {
            self.redo_stack.push(EditorState {
                lines: self.lines.clone(),
                cursor_line: self.cursor_line,
                cursor_col: self.cursor_col,
            });
            self.lines = state.lines;
            self.cursor_line = state.cursor_line;
            self.cursor_col = state.cursor_col;
        }
    }

    fn redo(&mut self) {
        if let Some(state) = self.redo_stack.pop() {
            self.undo_stack.push(EditorState {
                lines: self.lines.clone(),
                cursor_line: self.cursor_line,
                cursor_col: self.cursor_col,
            });
            self.lines = state.lines;
            self.cursor_line = state.cursor_line;
            self.cursor_col = state.cursor_col;
        }
    }

    #[allow(dead_code)]
    fn current_line(&self) -> &str {
        &self.lines[self.cursor_line]
    }

    fn current_line_len(&self) -> usize {
        self.lines[self.cursor_line].chars().count()
    }

    fn clamp_cursor(&mut self) {
        self.cursor_line = self.cursor_line.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.current_line_len());
    }

    fn ensure_visible(&mut self) {
        if self.cursor_line < self.viewport_top {
            self.viewport_top = self.cursor_line;
        }
        if self.cursor_line >= self.viewport_top + self.viewport_height {
            self.viewport_top = self.cursor_line + 1 - self.viewport_height;
        }
    }

    fn byte_offset_at_col(&self, line_idx: usize, col: usize) -> usize {
        self.lines[line_idx]
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[line_idx].len())
    }

    fn insert_char(&mut self, ch: char) {
        self.save_undo();
        let byte_offset = self.byte_offset_at_col(self.cursor_line, self.cursor_col);
        self.lines[self.cursor_line].insert(byte_offset, ch);
        self.cursor_col += 1;
    }

    fn insert_newline(&mut self) {
        self.save_undo();
        let byte_offset = self.byte_offset_at_col(self.cursor_line, self.cursor_col);
        let rest = self.lines[self.cursor_line][byte_offset..].to_string();
        self.lines[self.cursor_line].truncate(byte_offset);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.ensure_visible();
    }

    fn delete_back(&mut self) {
        if self.cursor_col > 0 {
            self.save_undo();
            self.cursor_col -= 1;
            let byte_offset = self.byte_offset_at_col(self.cursor_line, self.cursor_col);
            let next_offset = self.lines[self.cursor_line][byte_offset..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| byte_offset + i)
                .unwrap_or(self.lines[self.cursor_line].len());
            self.lines[self.cursor_line].drain(byte_offset..next_offset);
        } else if self.cursor_line > 0 {
            // Join with previous line
            self.save_undo();
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&current);
            self.ensure_visible();
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor_col < self.current_line_len() {
            self.save_undo();
            let byte_offset = self.byte_offset_at_col(self.cursor_line, self.cursor_col);
            let next_offset = self.lines[self.cursor_line][byte_offset..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| byte_offset + i)
                .unwrap_or(self.lines[self.cursor_line].len());
            self.lines[self.cursor_line].drain(byte_offset..next_offset);
        } else if self.cursor_line + 1 < self.lines.len() {
            // Join with next line
            self.save_undo();
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
        }
    }
}

impl Default for EditorComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for EditorComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut output = Vec::new();

        let end = (self.viewport_top + self.viewport_height).min(self.lines.len());
        let display_width = if self.border {
            (width as usize).saturating_sub(4) // "│ " + " │"
        } else {
            width as usize
        };

        // Top border
        if self.border {
            let border_line = format!("┌{}┐", "─".repeat(width as usize - 2));
            output.push(border_line);
        }

        for i in self.viewport_top..end {
            let line = &self.lines[i];
            let truncated = if utils::visible_width(line) > display_width {
                utils::truncate_to_width(line, display_width)
            } else {
                utils::pad_to_width(line, display_width)
            };

            if self.border {
                output.push(format!("│ {} │", truncated));
            } else {
                output.push(truncated);
            }
        }

        // Pad empty viewport rows
        for _ in end..(self.viewport_top + self.viewport_height) {
            let empty = " ".repeat(display_width);
            if self.border {
                output.push(format!("│ {} │", empty));
            } else {
                output.push(empty);
            }
        }

        // Bottom border with line info
        if self.border {
            let info = format!(" {}:{} ", self.cursor_line + 1, self.cursor_col + 1);
            let remaining = (width as usize).saturating_sub(2 + info.len());
            let border_line = format!(
                "└{}{info}{}┘",
                "─".repeat(remaining / 2),
                "─".repeat(remaining - remaining / 2)
            );
            output.push(border_line);
        }

        output
    }

    fn handle_input(&mut self, data: &str) -> HandleResult {
        if !self.focused {
            return HandleResult::Ignored;
        }

        let key = parse_key(data);
        if key.is_release {
            return HandleResult::Ignored;
        }

        match (&key.name, &key.modifiers) {
            (KeyName::Up, _) => {
                if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.clamp_cursor();
                    self.ensure_visible();
                }
                HandleResult::Handled
            }
            (KeyName::Down, _) => {
                if self.cursor_line + 1 < self.lines.len() {
                    self.cursor_line += 1;
                    self.clamp_cursor();
                    self.ensure_visible();
                }
                HandleResult::Handled
            }
            (KeyName::Left, _) => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.current_line_len();
                    self.ensure_visible();
                }
                HandleResult::Handled
            }
            (KeyName::Right, _) => {
                if self.cursor_col < self.current_line_len() {
                    self.cursor_col += 1;
                } else if self.cursor_line + 1 < self.lines.len() {
                    self.cursor_line += 1;
                    self.cursor_col = 0;
                    self.ensure_visible();
                }
                HandleResult::Handled
            }
            (KeyName::Home, _) => {
                self.cursor_col = 0;
                HandleResult::Handled
            }
            (KeyName::End, _) => {
                self.cursor_col = self.current_line_len();
                HandleResult::Handled
            }
            (KeyName::PageUp, _) => {
                self.cursor_line = self.cursor_line.saturating_sub(self.viewport_height);
                self.clamp_cursor();
                self.ensure_visible();
                HandleResult::Handled
            }
            (KeyName::PageDown, _) => {
                self.cursor_line = (self.cursor_line + self.viewport_height)
                    .min(self.lines.len().saturating_sub(1));
                self.clamp_cursor();
                self.ensure_visible();
                HandleResult::Handled
            }
            (KeyName::Enter, _) => {
                self.insert_newline();
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
            (KeyName::Char('z'), m) if m.ctrl => {
                self.undo();
                HandleResult::Handled
            }
            (KeyName::Char('y'), m) if m.ctrl => {
                self.redo();
                HandleResult::Handled
            }
            (KeyName::Char('k'), m) if m.ctrl => {
                // Kill line
                self.save_undo();
                let byte_offset = self.byte_offset_at_col(self.cursor_line, self.cursor_col);
                self.lines[self.cursor_line].truncate(byte_offset);
                HandleResult::Handled
            }
            (KeyName::Char(ch), m) if !m.ctrl && !m.alt => {
                self.insert_char(*ch);
                HandleResult::Handled
            }
            _ => HandleResult::Ignored,
        }
    }

    fn invalidate(&mut self) {}
}

impl Focusable for EditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }
        let row = (self.cursor_line - self.viewport_top) as u16;
        let col = self.cursor_col as u16;
        let offset = if self.border { 2 } else { 0 }; // "│ " prefix
        Some((col + offset, row + if self.border { 1 } else { 0 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_editor_new() {
        let editor = EditorComponent::new();
        assert_eq!(editor.text(), "");
        assert_eq!(editor.line_count(), 1);
        assert_eq!(editor.cursor(), (0, 0));
    }

    #[test]
    fn test_editor_set_text() {
        let mut editor = EditorComponent::new();
        editor.set_text("line1\nline2\nline3");
        assert_eq!(editor.line_count(), 3);
        assert_eq!(editor.text(), "line1\nline2\nline3");
    }

    #[test]
    fn test_editor_insert_char() {
        let mut editor = EditorComponent::new();
        editor.handle_input("h");
        editor.handle_input("i");
        assert_eq!(editor.text(), "hi");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn test_editor_newline() {
        let mut editor = EditorComponent::new();
        editor.handle_input("a");
        editor.handle_input("\r"); // Enter
        editor.handle_input("b");
        assert_eq!(editor.line_count(), 2);
        assert_eq!(editor.text(), "a\nb");
    }

    #[test]
    fn test_editor_backspace() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello");
        editor.cursor_col = 5;
        editor.handle_input("\x7f"); // Backspace
        assert_eq!(editor.text(), "hell");
    }

    #[test]
    fn test_editor_backspace_join_lines() {
        let mut editor = EditorComponent::new();
        editor.set_text("line1\nline2");
        editor.cursor_line = 1;
        editor.cursor_col = 0;
        editor.handle_input("\x7f");
        assert_eq!(editor.text(), "line1line2");
        assert_eq!(editor.line_count(), 1);
    }

    #[test]
    fn test_editor_cursor_movement() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello\nworld");
        editor.cursor_line = 0;
        editor.cursor_col = 2;

        editor.handle_input("\x1b[B"); // Down
        assert_eq!(editor.cursor(), (1, 2));

        editor.handle_input("\x1b[A"); // Up
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn test_editor_undo_redo() {
        let mut editor = EditorComponent::new();
        editor.handle_input("a");
        editor.handle_input("b");
        assert_eq!(editor.text(), "ab");

        // Undo the "b" insertion
        editor.undo();
        assert_eq!(editor.text(), "a");

        // Redo
        editor.redo();
        assert_eq!(editor.text(), "ab");
    }

    #[test]
    fn test_editor_render_with_border() {
        let mut editor = EditorComponent::new()
            .with_viewport_height(3)
            .with_border(true);
        editor.set_text("line1\nline2\nline3");
        let lines = editor.render(40);
        assert!(lines[0].starts_with('┌'));
        assert!(lines[1].starts_with('│'));
        assert!(lines.last().unwrap().starts_with('└'));
    }

    #[test]
    fn test_editor_render_without_border() {
        let mut editor = EditorComponent::new()
            .with_viewport_height(3)
            .with_border(false);
        editor.set_text("hello");
        let lines = editor.render(40);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_editor_viewport_scrolling() {
        let mut editor = EditorComponent::new().with_viewport_height(3);
        editor.set_text("1\n2\n3\n4\n5");
        editor.cursor_line = 4;
        editor.cursor_col = 0;
        editor.ensure_visible();
        assert!(editor.viewport_top > 0);
    }

    #[test]
    fn test_editor_page_up_down() {
        let mut editor = EditorComponent::new().with_viewport_height(3);
        editor.set_text("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        editor.cursor_line = 5;
        editor.cursor_col = 0;

        editor.handle_input("\x1b[5~"); // PageUp
        assert!(editor.cursor_line < 5);

        editor.cursor_line = 0;
        editor.handle_input("\x1b[6~"); // PageDown
        assert!(editor.cursor_line > 0);
    }

    #[test]
    fn test_editor_delete_forward() {
        let mut editor = EditorComponent::new();
        editor.set_text("hello");
        editor.cursor_col = 0;
        editor.handle_input("\x1b[3~"); // Delete
        assert_eq!(editor.text(), "ello");
    }

    #[test]
    fn test_editor_focus() {
        let mut editor = EditorComponent::new();
        assert!(editor.focused());
        editor.set_focused(false);
        assert!(!editor.focused());
        assert_eq!(editor.handle_input("a"), HandleResult::Ignored);
    }
}
