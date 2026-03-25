//! Keybindings system — configurable keyboard shortcuts.

use std::collections::HashMap;
use std::path::PathBuf;

/// An action that can be triggered by a key binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    /// Submit the current input.
    Submit,
    /// Insert a newline.
    Newline,
    /// Clear editor or quit if empty.
    ClearOrQuit,
    /// Cancel/abort current operation.
    Cancel,
    /// Navigate history up.
    HistoryUp,
    /// Navigate history down.
    HistoryDown,
    /// Trigger autocomplete.
    Autocomplete,
    /// Clear the screen.
    ClearScreen,
    /// Clear the current line.
    ClearLine,
    /// Kill to end of line.
    KillToEnd,
    /// Move to start of line.
    LineStart,
    /// Move to end of line.
    LineEnd,
    /// Undo last edit.
    Undo,
    /// Redo last undone edit.
    Redo,
}

/// A key combination (simplified representation).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    /// The key name (e.g., "enter", "c", "escape", "up").
    pub key: String,
    /// Whether Ctrl is held.
    pub ctrl: bool,
    /// Whether Shift is held.
    pub shift: bool,
    /// Whether Alt is held.
    pub alt: bool,
}

impl KeyCombo {
    /// Create a simple key with no modifiers.
    pub fn key(name: &str) -> Self {
        Self {
            key: name.to_lowercase(),
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// Create a Ctrl+key combo.
    pub fn ctrl(name: &str) -> Self {
        Self {
            key: name.to_lowercase(),
            ctrl: true,
            shift: false,
            alt: false,
        }
    }

    /// Create a Shift+key combo.
    pub fn shift(name: &str) -> Self {
        Self {
            key: name.to_lowercase(),
            ctrl: false,
            shift: true,
            alt: false,
        }
    }
}

impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.alt {
            parts.push("Alt");
        }
        parts.push(&self.key);
        write!(f, "{}", parts.join("+"))
    }
}

/// A single key binding entry.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    /// The key combination.
    pub combo: KeyCombo,
    /// The action to execute.
    pub action: Action,
    /// Human-readable description.
    pub description: String,
}

/// Manages key bindings with defaults and user overrides.
#[derive(Debug)]
pub struct KeyBindingsConfig {
    bindings: Vec<KeyBinding>,
    lookup: HashMap<KeyCombo, Action>,
}

impl Default for KeyBindingsConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyBindingsConfig {
    /// Create with default bindings.
    pub fn new() -> Self {
        let mut config = Self {
            bindings: Vec::new(),
            lookup: HashMap::new(),
        };
        config.register_defaults();
        config
    }

    /// Load user keybindings from `~/.hand/keybindings.json`, merging with defaults.
    pub fn load_from_file(path: &PathBuf) -> Self {
        let mut config = Self::new();

        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(overrides) = serde_json::from_str::<HashMap<String, String>>(&content)
        {
            for (key_str, action_str) in overrides {
                if let (Some(combo), Some(action)) =
                    (parse_key_combo(&key_str), parse_action(&action_str))
                {
                    config.bind(
                        combo.clone(),
                        action.clone(),
                        &format!("{key_str} -> {action_str}"),
                    );
                }
            }
        }

        config
    }

    /// Bind a key combo to an action.
    pub fn bind(&mut self, combo: KeyCombo, action: Action, description: &str) {
        self.lookup.insert(combo.clone(), action.clone());
        self.bindings.push(KeyBinding {
            combo,
            action,
            description: description.to_string(),
        });
    }

    /// Resolve a key combo to an action.
    pub fn resolve(&self, combo: &KeyCombo) -> Option<&Action> {
        self.lookup.get(combo)
    }

    /// Get all bindings.
    pub fn bindings(&self) -> &[KeyBinding] {
        &self.bindings
    }

    /// Get hotkeys help text.
    pub fn hotkeys_text(&self) -> String {
        let mut lines = vec!["Keyboard Shortcuts:".to_string(), String::new()];
        let max_combo_len = self
            .bindings
            .iter()
            .map(|b| b.combo.to_string().len())
            .max()
            .unwrap_or(0);

        for binding in &self.bindings {
            lines.push(format!(
                "  {:<width$}  {}",
                binding.combo,
                binding.description,
                width = max_combo_len
            ));
        }
        lines.join("\n")
    }

    fn register_defaults(&mut self) {
        let defaults = vec![
            (KeyCombo::key("enter"), Action::Submit, "Submit message"),
            (KeyCombo::shift("enter"), Action::Newline, "Insert newline"),
            (
                KeyCombo::ctrl("c"),
                Action::ClearOrQuit,
                "Clear editor / quit if empty",
            ),
            (
                KeyCombo::key("escape"),
                Action::Cancel,
                "Cancel current operation",
            ),
            (
                KeyCombo::key("up"),
                Action::HistoryUp,
                "Previous in history",
            ),
            (
                KeyCombo::key("down"),
                Action::HistoryDown,
                "Next in history",
            ),
            (
                KeyCombo::key("tab"),
                Action::Autocomplete,
                "Trigger autocomplete",
            ),
            (KeyCombo::ctrl("l"), Action::ClearScreen, "Clear screen"),
            (KeyCombo::ctrl("u"), Action::ClearLine, "Clear line"),
            (
                KeyCombo::ctrl("k"),
                Action::KillToEnd,
                "Kill to end of line",
            ),
            (
                KeyCombo::ctrl("a"),
                Action::LineStart,
                "Move to start of line",
            ),
            (KeyCombo::ctrl("e"), Action::LineEnd, "Move to end of line"),
            (KeyCombo::ctrl("z"), Action::Undo, "Undo"),
            (KeyCombo::ctrl("y"), Action::Redo, "Redo"),
        ];

        for (combo, action, desc) in defaults {
            self.bind(combo, action, desc);
        }
    }
}

/// Parse a key combo string like "Ctrl+C" or "Shift+Enter".
pub fn parse_key_combo(s: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut key = String::new();

    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            k => key = k.to_string(),
        }
    }

    if key.is_empty() {
        return None;
    }

    Some(KeyCombo {
        key,
        ctrl,
        shift,
        alt,
    })
}

/// Parse an action string.
pub fn parse_action(s: &str) -> Option<Action> {
    match s.to_lowercase().as_str() {
        "submit" => Some(Action::Submit),
        "newline" => Some(Action::Newline),
        "clear_or_quit" | "clearorquit" => Some(Action::ClearOrQuit),
        "cancel" => Some(Action::Cancel),
        "history_up" | "historyup" => Some(Action::HistoryUp),
        "history_down" | "historydown" => Some(Action::HistoryDown),
        "autocomplete" => Some(Action::Autocomplete),
        "clear_screen" | "clearscreen" => Some(Action::ClearScreen),
        "clear_line" | "clearline" => Some(Action::ClearLine),
        "kill_to_end" | "killtoend" => Some(Action::KillToEnd),
        "line_start" | "linestart" => Some(Action::LineStart),
        "line_end" | "lineend" => Some(Action::LineEnd),
        "undo" => Some(Action::Undo),
        "redo" => Some(Action::Redo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_registered() {
        let config = KeyBindingsConfig::new();
        assert!(!config.bindings().is_empty());
        assert!(config.bindings().len() >= 14);
    }

    #[test]
    fn resolve_ctrl_c() {
        let config = KeyBindingsConfig::new();
        let action = config.resolve(&KeyCombo::ctrl("c"));
        assert_eq!(action, Some(&Action::ClearOrQuit));
    }

    #[test]
    fn resolve_enter() {
        let config = KeyBindingsConfig::new();
        assert_eq!(
            config.resolve(&KeyCombo::key("enter")),
            Some(&Action::Submit)
        );
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let config = KeyBindingsConfig::new();
        assert!(config.resolve(&KeyCombo::key("f12")).is_none());
    }

    #[test]
    fn parse_key_combo_simple() {
        let combo = parse_key_combo("enter").unwrap();
        assert_eq!(combo.key, "enter");
        assert!(!combo.ctrl);
    }

    #[test]
    fn parse_key_combo_with_ctrl() {
        let combo = parse_key_combo("Ctrl+C").unwrap();
        assert_eq!(combo.key, "c");
        assert!(combo.ctrl);
    }

    #[test]
    fn parse_key_combo_with_shift() {
        let combo = parse_key_combo("Shift+Enter").unwrap();
        assert_eq!(combo.key, "enter");
        assert!(combo.shift);
    }

    #[test]
    fn parse_action_valid() {
        assert_eq!(parse_action("submit"), Some(Action::Submit));
        assert_eq!(parse_action("Cancel"), Some(Action::Cancel));
        assert_eq!(parse_action("undo"), Some(Action::Undo));
    }

    #[test]
    fn parse_action_invalid() {
        assert!(parse_action("nonexistent").is_none());
    }

    #[test]
    fn custom_binding() {
        let mut config = KeyBindingsConfig::new();
        config.bind(KeyCombo::ctrl("s"), Action::Submit, "Save/Submit");
        assert_eq!(config.resolve(&KeyCombo::ctrl("s")), Some(&Action::Submit));
    }

    #[test]
    fn hotkeys_text_not_empty() {
        let config = KeyBindingsConfig::new();
        let text = config.hotkeys_text();
        assert!(text.contains("Ctrl+c"));
        assert!(text.contains("enter"));
    }

    #[test]
    fn key_combo_display() {
        assert_eq!(KeyCombo::ctrl("c").to_string(), "Ctrl+c");
        assert_eq!(KeyCombo::key("enter").to_string(), "enter");
        assert_eq!(KeyCombo::shift("enter").to_string(), "Shift+enter");
    }
}
