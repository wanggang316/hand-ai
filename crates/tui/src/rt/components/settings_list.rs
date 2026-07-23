//! Settings-list component — a navigable list of editable key/value settings.
//!
//! The rt-native counterpart to the legacy `SettingsListComponent`. It paints
//! styled [`Line`]s into a ratatui [`Buffer`] and consumes structured
//! [`RtKey`]s. Each entry is a key and a typed [`SettingValue`]; Enter/Space
//! edits it in place — a bool flips, an enum cycles, and a string/number opens an
//! inline editor with a visible caret.
//!
//! # Pinned behaviour
//!
//! - **Clamp navigation.** Up/Down move the selection but **stop** at the first
//!   and last entry — they do not wrap. This is the deliberate counterpart to
//!   [`SelectList`](super::SelectList), whose navigation *wraps* (Decision Log:
//!   the two lists disagree on end behaviour on purpose).
//! - **Edit semantics.** Enter/Space on a bool toggles it; on an enum cycles to
//!   the next choice (wrapping the choice list); on a string/number opens the
//!   inline editor pre-filled with the current value. While editing, printable
//!   keys and Backspace mutate the buffer, Enter commits, and Escape discards.
//!   Committing a number that does not parse silently keeps the old value.
//! - **Chrome.** When the list overflows its window a `(n/total)` counter line is
//!   shown; the focused entry's description renders below the list; and a footer
//!   hint `Enter/Space to change · Esc to cancel` renders last. Each is opt-in.
//! - **Empty.** An empty entry list renders a "(no settings)" hint.
//!
//! The caret is reported through [`RtComponent::cursor`] so the host cursor lands
//! at the insertion point of the inline editor while a string/number is open.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Widget;

use super::{display_width, truncate_with_ellipsis};
use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

/// A typed setting value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// A free-text string, edited inline.
    String(String),
    /// A boolean, toggled in place.
    Bool(bool),
    /// A number, edited inline; an unparseable edit is rejected.
    Number(f64),
    /// A choice from a fixed list, cycled in place.
    Enum {
        /// The available choices.
        choices: Vec<String>,
        /// The index of the current choice.
        selected: usize,
    },
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValue::String(s) => write!(f, "{s}"),
            SettingValue::Bool(b) => write!(f, "{b}"),
            SettingValue::Number(n) => write!(f, "{n}"),
            SettingValue::Enum { choices, selected } => {
                write!(f, "{}", choices.get(*selected).map_or("", String::as_str))
            }
        }
    }
}

/// A single setting entry: a key, a typed value, and a help description.
#[derive(Debug, Clone)]
pub struct SettingEntry {
    /// The setting's key/name, shown in the primary column.
    pub key: String,
    /// The typed value.
    pub value: SettingValue,
    /// Help text shown below the list for the focused entry.
    pub description: String,
}

impl SettingEntry {
    /// A new entry with the given key, value, and (possibly empty) description.
    pub fn new(
        key: impl Into<String>,
        value: SettingValue,
        description: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            value,
            description: description.into(),
        }
    }
}

/// A navigable list of editable settings painted into a ratatui buffer.
pub struct SettingsList {
    /// All entries, in order.
    entries: Vec<SettingEntry>,
    /// The selected entry index.
    selected: usize,
    /// Whether the inline editor is open (string/number editing).
    editing: bool,
    /// The inline editor buffer, valid only while `editing`.
    edit_buffer: String,
    /// Rows shown before scrolling kicks in (`None` = show all).
    max_visible: Option<usize>,
    /// Top of the visible window.
    scroll_offset: usize,
    /// Whether to render the footer hint line.
    show_hint: bool,
    /// Whether to render the focused entry's description below the list.
    show_description: bool,
    /// The (x, y) of the caret within the render area while editing; `None`
    /// otherwise. Recorded on render so [`cursor`](RtComponent::cursor) can report
    /// it. Interior mutability via a `Cell` keeps `render` `&self`.
    caret: std::cell::Cell<Option<(u16, u16)>>,
}

impl SettingsList {
    /// A new list over `entries`, with the first entry selected.
    pub fn new(entries: Vec<SettingEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            editing: false,
            edit_buffer: String::new(),
            max_visible: None,
            scroll_offset: 0,
            show_hint: false,
            show_description: false,
            caret: std::cell::Cell::new(None),
        }
    }

    /// Limit the number of rows shown before scrolling kicks in.
    #[must_use]
    pub fn max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = Some(max_visible.max(1));
        self
    }

    /// Show the focused entry's description below the list.
    #[must_use]
    pub fn show_description(mut self, show: bool) -> Self {
        self.show_description = show;
        self
    }

    /// Show the footer hint line.
    #[must_use]
    pub fn show_hint(mut self, show: bool) -> Self {
        self.show_hint = show;
        self
    }

    /// The current entries.
    pub fn entries(&self) -> &[SettingEntry] {
        &self.entries
    }

    /// The selected entry index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The currently selected entry, if any.
    pub fn selected_entry(&self) -> Option<&SettingEntry> {
        self.entries.get(self.selected)
    }

    /// Whether the inline editor is currently open.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Move the selection up one entry, **clamping** at the first (no wrap).
    fn prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.clamp_scroll();
        }
    }

    /// Move the selection down one entry, **clamping** at the last (no wrap).
    fn next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            self.clamp_scroll();
        }
    }

    fn clamp_scroll(&mut self) {
        let Some(max) = self.max_visible else {
            return;
        };
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + max {
            self.scroll_offset = self.selected + 1 - max;
        }
    }

    /// Apply Enter/Space: toggle a bool, cycle an enum, or open/commit the inline
    /// editor for a string/number.
    fn activate(&mut self) {
        let Some(entry) = self.entries.get_mut(self.selected) else {
            return;
        };
        match &mut entry.value {
            SettingValue::Bool(b) => *b = !*b,
            SettingValue::Enum { choices, selected } => {
                if !choices.is_empty() {
                    *selected = (*selected + 1) % choices.len();
                }
            }
            SettingValue::String(_) | SettingValue::Number(_) => {
                if self.editing {
                    self.commit_edit();
                } else {
                    self.edit_buffer = entry.value.to_string();
                    self.editing = true;
                }
            }
        }
    }

    /// Commit the inline editor into the selected entry, then close it.
    ///
    /// A string takes the buffer verbatim; a number takes it only if it parses,
    /// otherwise the old value is silently kept (per the pinned "invalid number
    /// keeps old value" contract).
    fn commit_edit(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.selected) {
            match &mut entry.value {
                SettingValue::String(s) => *s = self.edit_buffer.clone(),
                SettingValue::Number(n) => {
                    if let Ok(parsed) = self.edit_buffer.trim().parse::<f64>() {
                        *n = parsed;
                    }
                }
                _ => {}
            }
        }
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// Discard the inline editor, keeping the old value.
    fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// The visible window `[start, end)` over the entries for the current window
    /// size.
    fn window(&self) -> (usize, usize) {
        let count = self.max_visible.unwrap_or(self.entries.len()).max(1);
        let start = self.scroll_offset.min(self.entries.len().saturating_sub(1));
        let end = (start + count).min(self.entries.len());
        (start, end)
    }
}

impl RtComponent for SettingsList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.caret.set(None);
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;

        if self.entries.is_empty() {
            let line = Line::from(Span::styled(
                "  (no settings)",
                Style::default().add_modifier(Modifier::DIM),
            ));
            Text::from(vec![line]).render(area, buf);
            return;
        }

        let key_col = self
            .entries
            .iter()
            .map(|e| display_width(&e.key))
            .max()
            .unwrap_or(0);
        let count = self.max_visible.unwrap_or(self.entries.len()).max(1);
        let (start, end) = self.window();

        let mut lines: Vec<Line<'static>> = Vec::new();
        // `y` tracks the row each line will paint on, so the caret's row can be
        // recorded for the entry being edited.
        for i in start..end {
            let entry = &self.entries[i];
            let is_selected = i == self.selected;
            let cursor = if is_selected { "▸ " } else { "  " };
            let key = pad_key(&entry.key, key_col);

            let value_str = if self.editing && is_selected {
                // A visible caret block marks the insertion point.
                format!("{}▏", self.edit_buffer)
            } else {
                entry.value.to_string()
            };

            let prefix = format!("{cursor}{key}: ");
            let mut line_style = Style::default();
            if is_selected {
                line_style = line_style.add_modifier(Modifier::BOLD);
            }
            let full = format!("{prefix}{value_str}");
            let shown = truncate_with_ellipsis(&full, width);
            lines.push(Line::from(Span::raw(shown)).style(line_style));

            // Record the caret column: end of the prefix + editor buffer, clamped
            // to the area, so the host cursor lands on the `▏` marker.
            if self.editing && is_selected {
                let caret_x = display_width(&prefix) + display_width(&self.edit_buffer);
                let row = (i - start) as u16;
                let x = (caret_x as u16).min(area.width.saturating_sub(1));
                self.caret.set(Some((x, row)));
            }
        }

        // Window counter, shown only when the list overflows the window.
        if self.entries.len() > count {
            let info = format!("  ({}/{})", self.selected + 1, self.entries.len());
            lines.push(Line::from(Span::styled(
                info,
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        // Focused entry's description, below the list.
        if self.show_description
            && let Some(entry) = self.entries.get(self.selected)
            && !entry.description.is_empty()
        {
            lines.push(Line::raw(""));
            let budget = width.saturating_sub(4).max(1);
            let desc = truncate_with_ellipsis(&entry.description, budget);
            lines.push(Line::from(Span::styled(
                format!("  {desc}"),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        // Footer hint.
        if self.show_hint {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "  Enter/Space to change · Esc to cancel",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        Text::from(lines).render(area, buf);
    }

    fn handle_key(&mut self, key: &RtKey) -> HandleOutcome {
        let Some(id) = key.key_id.as_deref() else {
            return HandleOutcome::Ignored;
        };

        // While editing, keys feed the inline editor.
        if self.editing {
            return self.handle_editing_key(key, id);
        }

        match id {
            "up" | "ctrl+k" => {
                self.prev();
                HandleOutcome::Consumed
            }
            "down" | "ctrl+j" => {
                self.next();
                HandleOutcome::Consumed
            }
            "enter" | "space" => {
                self.activate();
                HandleOutcome::Consumed
            }
            _ => HandleOutcome::Ignored,
        }
    }

    fn cursor(&self) -> Option<Position> {
        self.caret.get().map(|(x, y)| Position::new(x, y))
    }
}

impl SettingsList {
    /// Route a key while the inline editor is open.
    fn handle_editing_key(&mut self, key: &RtKey, id: &str) -> HandleOutcome {
        match id {
            "enter" => {
                self.commit_edit();
                HandleOutcome::Consumed
            }
            "escape" => {
                self.cancel_edit();
                HandleOutcome::Consumed
            }
            "backspace" => {
                self.edit_buffer.pop();
                HandleOutcome::Consumed
            }
            "space" => {
                self.edit_buffer.push(' ');
                HandleOutcome::Consumed
            }
            _ => {
                // A bare printable character (no modifiers) types into the buffer.
                if let Some(c) = printable_char(key) {
                    self.edit_buffer.push(c);
                    HandleOutcome::Consumed
                } else {
                    HandleOutcome::Ignored
                }
            }
        }
    }
}

/// The single printable character a key represents, if it is a bare (unmodified)
/// character key. Chorded keys (ctrl/alt/super) and named keys return `None`.
fn printable_char(key: &RtKey) -> Option<char> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mods = key.raw.modifiers;
    if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
        return None;
    }
    match key.raw.code {
        KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

/// Pad `key` on the right with spaces to `width` display columns.
fn pad_key(key: &str, width: usize) -> String {
    let w = display_width(key);
    if w >= width {
        key.to_string()
    } else {
        format!("{key}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn named(id: &str, code: KeyCode) -> RtKey {
        RtKey {
            key_id: Some(id.to_string()),
            raw: KeyEvent::new(code, KeyModifiers::NONE),
        }
    }

    fn chr(c: char) -> RtKey {
        RtKey {
            key_id: Some(c.to_string()),
            raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        }
    }

    fn entries() -> Vec<SettingEntry> {
        vec![
            SettingEntry::new(
                "theme",
                SettingValue::Enum {
                    choices: vec!["dark".into(), "light".into()],
                    selected: 0,
                },
                "Color theme",
            ),
            SettingEntry::new("auto_save", SettingValue::Bool(true), "Auto save"),
            SettingEntry::new("max_tokens", SettingValue::Number(4096.0), "Max tokens"),
            SettingEntry::new("model", SettingValue::String("gpt".into()), "Default model"),
        ]
    }

    #[test]
    fn navigation_clamps_no_wrap() {
        let mut list = SettingsList::new(entries());
        // Up at the first entry stays put (clamp, not wrap).
        list.handle_key(&named("up", KeyCode::Up));
        assert_eq!(list.selected_index(), 0);
        for _ in 0..10 {
            list.handle_key(&named("down", KeyCode::Down));
        }
        // Down past the last entry stays on the last (clamp, not wrap).
        assert_eq!(list.selected_index(), 3);
    }

    #[test]
    fn toggle_bool_and_cycle_enum() {
        let mut list = SettingsList::new(entries());
        list.handle_key(&named("enter", KeyCode::Enter)); // enum: dark -> light
        assert_eq!(list.entries()[0].value.to_string(), "light");
        list.handle_key(&named("down", KeyCode::Down));
        list.handle_key(&named("space", KeyCode::Char(' '))); // bool: true -> false
        assert_eq!(list.entries()[1].value, SettingValue::Bool(false));
    }

    #[test]
    fn invalid_number_keeps_old_value() {
        let mut list = SettingsList::new(entries());
        for _ in 0..2 {
            list.handle_key(&named("down", KeyCode::Down));
        }
        list.handle_key(&named("enter", KeyCode::Enter)); // open editor
        assert!(list.is_editing());
        // Replace with a non-numeric buffer.
        for _ in 0..4 {
            list.handle_key(&named("backspace", KeyCode::Backspace));
        }
        list.handle_key(&chr('n'));
        list.handle_key(&chr('a'));
        list.handle_key(&named("enter", KeyCode::Enter)); // commit
        assert_eq!(list.entries()[2].value, SettingValue::Number(4096.0));
    }

    #[test]
    fn escape_discards_edit() {
        let mut list = SettingsList::new(entries());
        for _ in 0..3 {
            list.handle_key(&named("down", KeyCode::Down));
        }
        list.handle_key(&named("enter", KeyCode::Enter)); // open editor on string
        list.handle_key(&chr('x'));
        list.handle_key(&named("escape", KeyCode::Esc));
        assert!(!list.is_editing());
        assert_eq!(list.entries()[3].value, SettingValue::String("gpt".into()));
    }
}
