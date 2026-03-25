//! Settings list component — displays and edits key-value settings.

use crate::theme::Style;
use crate::tui::Component;

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
        }
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
        }
    }

    /// Move selection down.
    pub fn next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
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
            return vec!["  (no settings)".to_string()];
        }

        let max_key_len = self.entries.iter().map(|e| e.key.len()).max().unwrap_or(0);

        self.entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let prefix = if i == self.selected { "▸ " } else { "  " };
                let key = format!("{:width$}", entry.key, width = max_key_len);
                let value = if self.editing && i == self.selected {
                    format!("{}▏", self.edit_buffer)
                } else {
                    entry.value.to_string()
                };

                let line = format!("{prefix}{}: {value}", self.key_style.apply(&key));
                if line.len() > width {
                    line[..width].to_string()
                } else {
                    line
                }
            })
            .collect()
    }

    fn handle_input(&mut self, _data: &str) -> crate::tui::HandleResult {
        crate::tui::HandleResult::Ignored
    }

    fn invalidate(&mut self) {}
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
}
