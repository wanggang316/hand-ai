//! Settings list component — displays and edits key-value settings.
//
// audit: M3.T5 — parity reviewed against upstream TUI/settings-list.ts on 2026-05-07.
// non-goal: TS's `SettingsList` ships with optional fuzzy search input
// (`enableSearch`) and submenu support (`SettingItem.submenu`). Both are
// significant subsystems; the Rust port keeps the simpler edit-in-place model
// and leaves search/submenu wiring to host applications.

use crate::theme::Style;
use crate::tui::Component;
use crate::utils;

/// Type of a setting value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// String value.
    String(String),
    /// Boolean toggle.
    Bool(bool),
    /// Numeric value.
    Number(f64),
    /// Enum with a list of choices and current selection index.
    Enum {
        choices: Vec<String>,
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
                write!(f, "{}", choices.get(*selected).unwrap_or(&String::new()))
            }
        }
    }
}

/// A single setting entry.
#[derive(Debug, Clone)]
pub struct SettingEntry {
    /// Setting key/name.
    pub key: String,
    /// Setting value.
    pub value: SettingValue,
    /// Description shown as help text.
    pub description: String,
}

/// Theme for [`SettingsListComponent`], mirroring TS's `SettingsListTheme`.
///
/// Each color slot is an ANSI prefix (e.g. `"\x1b[36m"`); leave it `None` to
/// use the built-in default styling. `cursor` is the literal prefix string
/// printed in front of the selected entry.
#[derive(Debug, Clone)]
pub struct SettingsListTheme {
    pub label: Option<String>,
    pub label_selected: Option<String>,
    pub value: Option<String>,
    pub value_selected: Option<String>,
    pub description: Option<String>,
    pub hint: Option<String>,
    pub cursor: String,
}

impl Default for SettingsListTheme {
    fn default() -> Self {
        Self {
            label: None,
            label_selected: None,
            value: None,
            value_selected: None,
            description: Some("\x1b[90m".to_string()),
            hint: Some("\x1b[90m".to_string()),
            cursor: "▸ ".to_string(),
        }
    }
}

/// Component that displays a navigable list of settings.
#[derive(Debug)]
pub struct SettingsListComponent {
    entries: Vec<SettingEntry>,
    selected: usize,
    editing: bool,
    edit_buffer: String,
    key_style: Style,
    #[allow(dead_code)]
    value_style: Style,
    #[allow(dead_code)]
    selected_style: Style,
    theme: SettingsListTheme,
    /// Maximum entries rendered before scrolling kicks in (None = unlimited).
    max_visible: Option<usize>,
    scroll_offset: usize,
    /// Whether to render a hint footer ("Enter/Space to change · Esc to cancel").
    show_hint: bool,
    /// Whether to render the selected entry's description below the list.
    show_description: bool,
}

impl SettingsListComponent {
    /// Create a new settings list.
    pub fn new(entries: Vec<SettingEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            editing: false,
            edit_buffer: String::new(),
            key_style: Style::bold(),
            value_style: Style::default(),
            selected_style: Style::bold(),
            theme: SettingsListTheme::default(),
            max_visible: None,
            scroll_offset: 0,
            show_hint: false,
            show_description: false,
        }
    }

    /// Builder: replace the theme.
    pub fn with_theme(mut self, theme: SettingsListTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Builder: limit the number of rows rendered before scrolling kicks in.
    pub fn with_max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = Some(max_visible);
        self
    }

    /// Builder: render the selected entry's description under the list.
    pub fn with_description(mut self, show: bool) -> Self {
        self.show_description = show;
        self
    }

    /// Builder: render a footer hint ("Enter/Space to change · Esc to cancel").
    pub fn with_hint(mut self, show: bool) -> Self {
        self.show_hint = show;
        self
    }

    /// Replace the theme at runtime.
    pub fn set_theme(&mut self, theme: SettingsListTheme) {
        self.theme = theme;
    }

    /// Get current entries.
    pub fn entries(&self) -> &[SettingEntry] {
        &self.entries
    }

    /// Get the currently selected index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Get the currently selected entry.
    pub fn selected_entry(&self) -> Option<&SettingEntry> {
        self.entries.get(self.selected)
    }

    /// Move selection up.
    pub fn prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.adjust_scroll();
        }
    }

    /// Move selection down.
    pub fn next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            self.adjust_scroll();
        }
    }

    fn adjust_scroll(&mut self) {
        let Some(max) = self.max_visible else {
            return;
        };
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + max {
            self.scroll_offset = self.selected + 1 - max;
        }
    }

    /// Toggle or start editing the selected entry.
    pub fn toggle_edit(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let entry = &mut self.entries[self.selected];
        match &mut entry.value {
            SettingValue::Bool(b) => *b = !*b,
            SettingValue::Enum { choices, selected } => {
                *selected = (*selected + 1) % choices.len();
            }
            SettingValue::String(_) | SettingValue::Number(_) => {
                if self.editing {
                    self.apply_edit();
                } else {
                    self.edit_buffer = entry.value.to_string();
                    self.editing = true;
                }
            }
        }
    }

    /// Cancel editing.
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// Is currently in edit mode.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    fn apply_edit(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.selected) {
            match &mut entry.value {
                SettingValue::String(s) => {
                    *s = self.edit_buffer.clone();
                }
                SettingValue::Number(n) => {
                    if let Ok(parsed) = self.edit_buffer.parse::<f64>() {
                        *n = parsed;
                    }
                }
                _ => {}
            }
        }
        self.editing = false;
        self.edit_buffer.clear();
    }
}

impl Component for SettingsListComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        if self.entries.is_empty() {
            let msg = "  (no settings)";
            return vec![apply_ansi(self.theme.hint.as_deref(), msg)];
        }

        let max_key_len = self.entries.iter().map(|e| e.key.len()).max().unwrap_or(0);

        let visible_count = self.max_visible.unwrap_or(self.entries.len());
        let start = self.scroll_offset.min(self.entries.len().saturating_sub(1));
        let end = (start + visible_count).min(self.entries.len());

        let mut lines: Vec<String> = Vec::with_capacity(end - start + 3);

        for (i, entry) in self.entries.iter().enumerate().take(end).skip(start) {
            let is_selected = i == self.selected;
            let cursor_prefix = if is_selected {
                self.theme.cursor.as_str()
            } else {
                "  "
            };
            let key = format!("{:width$}", entry.key, width = max_key_len);
            let key_styled = if is_selected {
                apply_ansi_or_default(
                    self.theme.label_selected.as_deref(),
                    &self.key_style.apply(&key),
                )
            } else {
                apply_ansi_or_default(self.theme.label.as_deref(), &self.key_style.apply(&key))
            };

            let value_str = if self.editing && is_selected {
                format!("{}▏", self.edit_buffer)
            } else {
                entry.value.to_string()
            };
            let value_styled = if is_selected {
                apply_ansi_or_default(self.theme.value_selected.as_deref(), &value_str)
            } else {
                apply_ansi_or_default(self.theme.value.as_deref(), &value_str)
            };

            let line = format!("{cursor_prefix}{key_styled}: {value_styled}");
            // Truncate to width using visible_width-aware helper.
            let truncated = if utils::visible_width(&line) > width {
                utils::truncate_to_width(&line, width)
            } else {
                line
            };
            lines.push(truncated);
        }

        // Scroll indicator if more rows than fit.
        if self.entries.len() > visible_count {
            let info = format!("  ({}/{})", self.selected + 1, self.entries.len());
            lines.push(apply_ansi(self.theme.hint.as_deref(), &info));
        }

        // Description for the focused entry.
        if self.show_description
            && let Some(entry) = self.entries.get(self.selected)
            && !entry.description.is_empty()
        {
            lines.push(String::new());
            let desc_width = width.saturating_sub(4).max(1);
            for desc_line in utils::wrap_text(&entry.description, desc_width) {
                let prefixed = format!("  {desc_line}");
                lines.push(apply_ansi(self.theme.description.as_deref(), &prefixed));
            }
        }

        // Footer hint.
        if self.show_hint {
            lines.push(String::new());
            let hint = "  Enter/Space to change · Esc to cancel";
            lines.push(apply_ansi(self.theme.hint.as_deref(), hint));
        }

        lines
    }

    fn invalidate(&mut self) {}
}

/// Wrap `text` with `prefix` + reset; if `prefix` is `None`/empty, returns
/// `text` unchanged.
fn apply_ansi(prefix: Option<&str>, text: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}{text}\x1b[0m"),
        _ => text.to_string(),
    }
}

#[inline]
fn apply_ansi_or_default(prefix: Option<&str>, text: &str) -> String {
    apply_ansi(prefix, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entries() -> Vec<SettingEntry> {
        vec![
            SettingEntry {
                key: "theme".to_string(),
                value: SettingValue::Enum {
                    choices: vec!["dark".to_string(), "light".to_string()],
                    selected: 0,
                },
                description: "Color theme".to_string(),
            },
            SettingEntry {
                key: "auto_save".to_string(),
                value: SettingValue::Bool(true),
                description: "Auto save".to_string(),
            },
            SettingEntry {
                key: "max_tokens".to_string(),
                value: SettingValue::Number(4096.0),
                description: "Max tokens".to_string(),
            },
            SettingEntry {
                key: "model".to_string(),
                value: SettingValue::String("gpt-4o".to_string()),
                description: "Default model".to_string(),
            },
        ]
    }

    #[test]
    fn render_shows_all_entries() {
        let comp = SettingsListComponent::new(test_entries());
        let lines = comp.render(80);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn navigation() {
        let mut comp = SettingsListComponent::new(test_entries());
        assert_eq!(comp.selected_index(), 0);

        comp.next();
        assert_eq!(comp.selected_index(), 1);

        comp.next();
        comp.next();
        assert_eq!(comp.selected_index(), 3);

        // Can't go past end
        comp.next();
        assert_eq!(comp.selected_index(), 3);

        comp.prev();
        assert_eq!(comp.selected_index(), 2);
    }

    #[test]
    fn toggle_bool() {
        let mut comp = SettingsListComponent::new(test_entries());
        comp.next(); // select auto_save (Bool)
        comp.toggle_edit();
        if let SettingValue::Bool(v) = &comp.entries()[1].value {
            assert!(!v);
        } else {
            panic!("Expected Bool");
        }
    }

    #[test]
    fn toggle_enum_cycles() {
        let mut comp = SettingsListComponent::new(test_entries());
        // theme is at index 0, it's an Enum
        comp.toggle_edit();
        if let SettingValue::Enum { selected, .. } = &comp.entries()[0].value {
            assert_eq!(*selected, 1);
        }
        comp.toggle_edit();
        if let SettingValue::Enum { selected, .. } = &comp.entries()[0].value {
            assert_eq!(*selected, 0); // cycles back
        }
    }

    #[test]
    fn empty_settings() {
        let comp = SettingsListComponent::new(vec![]);
        let lines = comp.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no settings"));
    }

    #[test]
    fn setting_value_display() {
        assert_eq!(SettingValue::String("hello".into()).to_string(), "hello");
        assert_eq!(SettingValue::Bool(true).to_string(), "true");
        assert_eq!(SettingValue::Number(42.0).to_string(), "42");
        assert_eq!(
            SettingValue::Enum {
                choices: vec!["a".into(), "b".into()],
                selected: 1
            }
            .to_string(),
            "b"
        );
    }

    #[test]
    fn theme_paints_selected_label_and_value() {
        let theme = SettingsListTheme {
            label_selected: Some("\x1b[36m".into()),
            value_selected: Some("\x1b[33m".into()),
            cursor: "» ".into(),
            ..SettingsListTheme::default()
        };
        let comp = SettingsListComponent::new(test_entries()).with_theme(theme);
        let lines = comp.render(80);
        assert!(lines[0].starts_with("» "));
        assert!(lines[0].contains("\x1b[36m"));
        assert!(lines[0].contains("\x1b[33m"));
        // Non-selected rows do NOT carry the selected ANSI prefixes.
        assert!(!lines[1].contains("\x1b[36m"));
    }

    #[test]
    fn description_renders_when_enabled() {
        let comp = SettingsListComponent::new(test_entries()).with_description(true);
        let lines = comp.render(80);
        // Description for "theme" is "Color theme".
        assert!(lines.iter().any(|l| l.contains("Color theme")));
    }

    #[test]
    fn hint_footer_renders_when_enabled() {
        let comp = SettingsListComponent::new(test_entries()).with_hint(true);
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("Enter/Space to change")));
    }

    #[test]
    fn max_visible_scrolls_with_indicator() {
        let mut comp = SettingsListComponent::new(test_entries()).with_max_visible(2);
        let lines = comp.render(80);
        // 2 visible entries + 1 scroll indicator line.
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("(1/4)"));

        comp.next();
        comp.next();
        comp.next(); // selects entry index 3 (last)
        let lines = comp.render(80);
        // Window shifted: now showing entries 2..=3
        assert!(lines.iter().any(|l| l.contains("max_tokens")));
        assert!(lines.iter().any(|l| l.contains("(4/4)")));
    }
}
