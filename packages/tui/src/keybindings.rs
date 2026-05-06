//! Global keybinding registry and resolver.
//!
//! Maps semantic actions (e.g. `tui.editor.cursorUp`) to user-facing key ids
//! (e.g. `up`, `ctrl+b`). The default table mirrors `TUI_KEYBINDINGS` from the
//! TS source. Users may override defaults via [`KeybindingsConfig`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, RwLock};

use crate::keys::{KeyId, matches_key};

/// All built-in semantic keybindings.
///
/// String ids match the TS keys verbatim (e.g. `"tui.editor.cursorUp"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keybinding {
    EditorCursorUp,
    EditorCursorDown,
    EditorCursorLeft,
    EditorCursorRight,
    EditorCursorWordLeft,
    EditorCursorWordRight,
    EditorCursorLineStart,
    EditorCursorLineEnd,
    EditorJumpForward,
    EditorJumpBackward,
    EditorPageUp,
    EditorPageDown,
    EditorDeleteCharBackward,
    EditorDeleteCharForward,
    EditorDeleteWordBackward,
    EditorDeleteWordForward,
    EditorDeleteToLineStart,
    EditorDeleteToLineEnd,
    EditorYank,
    EditorYankPop,
    EditorUndo,
    InputNewLine,
    InputSubmit,
    InputTab,
    InputCopy,
    SelectUp,
    SelectDown,
    SelectPageUp,
    SelectPageDown,
    SelectConfirm,
    SelectCancel,
}

impl Keybinding {
    /// Stable string id, matching the TS keys.
    pub fn id(self) -> &'static str {
        match self {
            Self::EditorCursorUp => "tui.editor.cursorUp",
            Self::EditorCursorDown => "tui.editor.cursorDown",
            Self::EditorCursorLeft => "tui.editor.cursorLeft",
            Self::EditorCursorRight => "tui.editor.cursorRight",
            Self::EditorCursorWordLeft => "tui.editor.cursorWordLeft",
            Self::EditorCursorWordRight => "tui.editor.cursorWordRight",
            Self::EditorCursorLineStart => "tui.editor.cursorLineStart",
            Self::EditorCursorLineEnd => "tui.editor.cursorLineEnd",
            Self::EditorJumpForward => "tui.editor.jumpForward",
            Self::EditorJumpBackward => "tui.editor.jumpBackward",
            Self::EditorPageUp => "tui.editor.pageUp",
            Self::EditorPageDown => "tui.editor.pageDown",
            Self::EditorDeleteCharBackward => "tui.editor.deleteCharBackward",
            Self::EditorDeleteCharForward => "tui.editor.deleteCharForward",
            Self::EditorDeleteWordBackward => "tui.editor.deleteWordBackward",
            Self::EditorDeleteWordForward => "tui.editor.deleteWordForward",
            Self::EditorDeleteToLineStart => "tui.editor.deleteToLineStart",
            Self::EditorDeleteToLineEnd => "tui.editor.deleteToLineEnd",
            Self::EditorYank => "tui.editor.yank",
            Self::EditorYankPop => "tui.editor.yankPop",
            Self::EditorUndo => "tui.editor.undo",
            Self::InputNewLine => "tui.input.newLine",
            Self::InputSubmit => "tui.input.submit",
            Self::InputTab => "tui.input.tab",
            Self::InputCopy => "tui.input.copy",
            Self::SelectUp => "tui.select.up",
            Self::SelectDown => "tui.select.down",
            Self::SelectPageUp => "tui.select.pageUp",
            Self::SelectPageDown => "tui.select.pageDown",
            Self::SelectConfirm => "tui.select.confirm",
            Self::SelectCancel => "tui.select.cancel",
        }
    }

    /// Inverse of [`Keybinding::id`]; returns `None` for unknown ids.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "tui.editor.cursorUp" => Self::EditorCursorUp,
            "tui.editor.cursorDown" => Self::EditorCursorDown,
            "tui.editor.cursorLeft" => Self::EditorCursorLeft,
            "tui.editor.cursorRight" => Self::EditorCursorRight,
            "tui.editor.cursorWordLeft" => Self::EditorCursorWordLeft,
            "tui.editor.cursorWordRight" => Self::EditorCursorWordRight,
            "tui.editor.cursorLineStart" => Self::EditorCursorLineStart,
            "tui.editor.cursorLineEnd" => Self::EditorCursorLineEnd,
            "tui.editor.jumpForward" => Self::EditorJumpForward,
            "tui.editor.jumpBackward" => Self::EditorJumpBackward,
            "tui.editor.pageUp" => Self::EditorPageUp,
            "tui.editor.pageDown" => Self::EditorPageDown,
            "tui.editor.deleteCharBackward" => Self::EditorDeleteCharBackward,
            "tui.editor.deleteCharForward" => Self::EditorDeleteCharForward,
            "tui.editor.deleteWordBackward" => Self::EditorDeleteWordBackward,
            "tui.editor.deleteWordForward" => Self::EditorDeleteWordForward,
            "tui.editor.deleteToLineStart" => Self::EditorDeleteToLineStart,
            "tui.editor.deleteToLineEnd" => Self::EditorDeleteToLineEnd,
            "tui.editor.yank" => Self::EditorYank,
            "tui.editor.yankPop" => Self::EditorYankPop,
            "tui.editor.undo" => Self::EditorUndo,
            "tui.input.newLine" => Self::InputNewLine,
            "tui.input.submit" => Self::InputSubmit,
            "tui.input.tab" => Self::InputTab,
            "tui.input.copy" => Self::InputCopy,
            "tui.select.up" => Self::SelectUp,
            "tui.select.down" => Self::SelectDown,
            "tui.select.pageUp" => Self::SelectPageUp,
            "tui.select.pageDown" => Self::SelectPageDown,
            "tui.select.confirm" => Self::SelectConfirm,
            "tui.select.cancel" => Self::SelectCancel,
            _ => return None,
        })
    }

    /// Iterator over every variant.
    fn all() -> impl Iterator<Item = Self> {
        [
            Self::EditorCursorUp,
            Self::EditorCursorDown,
            Self::EditorCursorLeft,
            Self::EditorCursorRight,
            Self::EditorCursorWordLeft,
            Self::EditorCursorWordRight,
            Self::EditorCursorLineStart,
            Self::EditorCursorLineEnd,
            Self::EditorJumpForward,
            Self::EditorJumpBackward,
            Self::EditorPageUp,
            Self::EditorPageDown,
            Self::EditorDeleteCharBackward,
            Self::EditorDeleteCharForward,
            Self::EditorDeleteWordBackward,
            Self::EditorDeleteWordForward,
            Self::EditorDeleteToLineStart,
            Self::EditorDeleteToLineEnd,
            Self::EditorYank,
            Self::EditorYankPop,
            Self::EditorUndo,
            Self::InputNewLine,
            Self::InputSubmit,
            Self::InputTab,
            Self::InputCopy,
            Self::SelectUp,
            Self::SelectDown,
            Self::SelectPageUp,
            Self::SelectPageDown,
            Self::SelectConfirm,
            Self::SelectCancel,
        ]
        .into_iter()
    }
}

/// Default key mapping plus optional human-readable description.
#[derive(Debug, Clone)]
pub struct KeybindingDefinition {
    pub default_keys: Vec<KeyId>,
    pub description: Option<String>,
}

fn def(keys: &[&str], desc: &str) -> KeybindingDefinition {
    KeybindingDefinition {
        default_keys: keys.iter().map(|s| (*s).to_string()).collect(),
        description: Some(desc.to_string()),
    }
}

/// Default keybinding table. Mirrors `TUI_KEYBINDINGS` in the TS source verbatim.
pub static TUI_KEYBINDINGS: LazyLock<HashMap<&'static str, KeybindingDefinition>> =
    LazyLock::new(|| {
        let entries: [(&str, KeybindingDefinition); 31] = [
            ("tui.editor.cursorUp", def(&["up"], "Move cursor up")),
            ("tui.editor.cursorDown", def(&["down"], "Move cursor down")),
            (
                "tui.editor.cursorLeft",
                def(&["left", "ctrl+b"], "Move cursor left"),
            ),
            (
                "tui.editor.cursorRight",
                def(&["right", "ctrl+f"], "Move cursor right"),
            ),
            (
                "tui.editor.cursorWordLeft",
                def(&["alt+left", "ctrl+left", "alt+b"], "Move cursor word left"),
            ),
            (
                "tui.editor.cursorWordRight",
                def(
                    &["alt+right", "ctrl+right", "alt+f"],
                    "Move cursor word right",
                ),
            ),
            (
                "tui.editor.cursorLineStart",
                def(&["home", "ctrl+a"], "Move to line start"),
            ),
            (
                "tui.editor.cursorLineEnd",
                def(&["end", "ctrl+e"], "Move to line end"),
            ),
            (
                "tui.editor.jumpForward",
                def(&["ctrl+]"], "Jump forward to character"),
            ),
            (
                "tui.editor.jumpBackward",
                def(&["ctrl+alt+]"], "Jump backward to character"),
            ),
            ("tui.editor.pageUp", def(&["pageUp"], "Page up")),
            ("tui.editor.pageDown", def(&["pageDown"], "Page down")),
            (
                "tui.editor.deleteCharBackward",
                def(&["backspace"], "Delete character backward"),
            ),
            (
                "tui.editor.deleteCharForward",
                def(&["delete", "ctrl+d"], "Delete character forward"),
            ),
            (
                "tui.editor.deleteWordBackward",
                def(&["ctrl+w", "alt+backspace"], "Delete word backward"),
            ),
            (
                "tui.editor.deleteWordForward",
                def(&["alt+d", "alt+delete"], "Delete word forward"),
            ),
            (
                "tui.editor.deleteToLineStart",
                def(&["ctrl+u"], "Delete to line start"),
            ),
            (
                "tui.editor.deleteToLineEnd",
                def(&["ctrl+k"], "Delete to line end"),
            ),
            ("tui.editor.yank", def(&["ctrl+y"], "Yank")),
            ("tui.editor.yankPop", def(&["alt+y"], "Yank pop")),
            ("tui.editor.undo", def(&["ctrl+-"], "Undo")),
            ("tui.input.newLine", def(&["shift+enter"], "Insert newline")),
            ("tui.input.submit", def(&["enter"], "Submit input")),
            ("tui.input.tab", def(&["tab"], "Tab / autocomplete")),
            ("tui.input.copy", def(&["ctrl+c"], "Copy selection")),
            ("tui.select.up", def(&["up"], "Move selection up")),
            ("tui.select.down", def(&["down"], "Move selection down")),
            ("tui.select.pageUp", def(&["pageUp"], "Selection page up")),
            (
                "tui.select.pageDown",
                def(&["pageDown"], "Selection page down"),
            ),
            ("tui.select.confirm", def(&["enter"], "Confirm selection")),
            (
                "tui.select.cancel",
                def(&["escape", "ctrl+c"], "Cancel selection"),
            ),
        ];
        entries.into_iter().collect()
    });

/// Reported when a single key id is bound to multiple [`Keybinding`]s
/// **via user overrides**. Defaults are not flagged (mirroring TS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    pub key_id: KeyId,
    pub bindings: Vec<Keybinding>,
}

/// User override map.
///
/// - id absent → use default
/// - `Some(keys)` → override (empty Vec = explicitly empty / disabled)
/// - `None` → disabled (treated like empty Vec for matching)
pub type KeybindingsConfig = HashMap<String, Option<Vec<KeyId>>>;

fn dedup_keys(keys: &[KeyId]) -> Vec<KeyId> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        if seen.insert(k.clone()) {
            out.push(k.clone());
        }
    }
    out
}

/// Resolves raw key strings to semantic [`Keybinding`]s based on the default
/// table merged with optional user overrides.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    /// Resolved keys per binding (after applying user overrides).
    keys_by_id: HashMap<Keybinding, Vec<KeyId>>,
    /// Conflicts from user-supplied overrides only.
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    /// Build a manager using the default table and no user overrides.
    pub fn new() -> Self {
        Self::with_config(&KeybindingsConfig::new())
    }

    /// Build a manager using the default table merged with `config`.
    pub fn with_config(config: &KeybindingsConfig) -> Self {
        let mut keys_by_id = HashMap::with_capacity(31);

        // Resolve every binding: user override (Some/None) wins over default.
        for binding in Keybinding::all() {
            let id = binding.id();
            let resolved = match config.get(id) {
                Some(Some(keys)) => dedup_keys(keys),
                Some(None) => Vec::new(),
                None => {
                    let def = TUI_KEYBINDINGS
                        .get(id)
                        .expect("every Keybinding has a default entry");
                    dedup_keys(&def.default_keys)
                }
            };
            keys_by_id.insert(binding, resolved);
        }

        // Conflicts: only consider keys that appear in user overrides.
        // Mirrors TS which iterates `userBindings`, not the resolved table.
        let mut user_claims: HashMap<KeyId, Vec<Keybinding>> = HashMap::new();
        for (id, value) in config {
            let Some(binding) = Keybinding::from_id(id) else {
                continue;
            };
            let keys = match value {
                Some(keys) => dedup_keys(keys),
                None => Vec::new(),
            };
            for key in keys {
                user_claims.entry(key).or_default().push(binding);
            }
        }

        // Sorted output for stable reporting.
        let mut sorted: BTreeMap<KeyId, Vec<Keybinding>> = BTreeMap::new();
        for (key, bindings) in user_claims {
            if bindings.len() > 1 {
                sorted.insert(key, bindings);
            }
        }
        let conflicts = sorted
            .into_iter()
            .map(|(key_id, bindings)| KeybindingConflict { key_id, bindings })
            .collect();

        Self {
            keys_by_id,
            conflicts,
        }
    }

    /// Override the keys bound to `binding`. Empty vec disables matching.
    pub fn set(&mut self, binding: Keybinding, keys: Vec<KeyId>) {
        self.keys_by_id.insert(binding, dedup_keys(&keys));
    }

    /// Disable `binding` (no keys will match it).
    pub fn unset(&mut self, binding: Keybinding) {
        self.keys_by_id.insert(binding, Vec::new());
    }

    /// Resolved keys for `binding`. Empty slice means disabled.
    pub fn get(&self, binding: Keybinding) -> &[KeyId] {
        self.keys_by_id
            .get(&binding)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// True iff any key bound to `binding` matches the raw input `key_data`.
    pub fn matches(&self, key_data: &str, binding: Keybinding) -> bool {
        self.get(binding)
            .iter()
            .any(|key| matches_key(key_data, key))
    }

    /// User-override conflicts, sorted by key id.
    pub fn conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    /// Iterate all bindings with their resolved keys.
    pub fn all(&self) -> impl Iterator<Item = (Keybinding, &[KeyId])> {
        Keybinding::all().map(|b| (b, self.get(b)))
    }
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_MANAGER: LazyLock<RwLock<KeybindingsManager>> =
    LazyLock::new(|| RwLock::new(KeybindingsManager::new()));

/// Replace the process-wide [`KeybindingsManager`] with one built from `config`.
pub fn set_keybindings(config: KeybindingsConfig) {
    let manager = KeybindingsManager::with_config(&config);
    *GLOBAL_MANAGER.write().expect("keybindings lock poisoned") = manager;
}

/// Snapshot the process-wide [`KeybindingsManager`]. Callers do not hold a lock.
pub fn get_keybindings() -> KeybindingsManager {
    GLOBAL_MANAGER
        .read()
        .expect("keybindings lock poisoned")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(entries: &[(&str, Option<Vec<&str>>)]) -> KeybindingsConfig {
        entries
            .iter()
            .map(|(id, keys)| {
                let value = keys
                    .as_ref()
                    .map(|v| v.iter().map(|s| (*s).to_string()).collect());
                ((*id).to_string(), value)
            })
            .collect()
    }

    #[test]
    fn id_round_trip_for_every_variant() {
        for binding in Keybinding::all() {
            let id = binding.id();
            assert_eq!(Keybinding::from_id(id), Some(binding));
        }
    }

    #[test]
    fn from_id_rejects_unknown() {
        assert_eq!(Keybinding::from_id("nope"), None);
        assert_eq!(Keybinding::from_id(""), None);
        assert_eq!(Keybinding::from_id("tui.editor.unknown"), None);
    }

    #[test]
    fn defaults_table_has_every_binding() {
        for binding in Keybinding::all() {
            assert!(
                TUI_KEYBINDINGS.contains_key(binding.id()),
                "missing default for {}",
                binding.id()
            );
        }
        assert_eq!(TUI_KEYBINDINGS.len(), 31);
    }

    #[test]
    fn default_cursor_up_matches_ansi_up_arrow() {
        let m = KeybindingsManager::new();
        assert!(m.matches("\x1b[A", Keybinding::EditorCursorUp));
    }

    #[test]
    fn default_yank_matches_ctrl_y_byte() {
        let m = KeybindingsManager::new();
        assert!(m.matches("\x19", Keybinding::EditorYank));
    }

    #[test]
    fn default_cursor_left_matches_either_default_key() {
        let m = KeybindingsManager::new();
        assert!(m.matches("\x1b[D", Keybinding::EditorCursorLeft));
        assert!(m.matches("\x02", Keybinding::EditorCursorLeft)); // ctrl+b
    }

    #[test]
    fn default_submit_matches_enter() {
        let m = KeybindingsManager::new();
        assert!(m.matches("\r", Keybinding::InputSubmit));
    }

    #[test]
    fn default_select_cancel_matches_escape_or_ctrl_c() {
        let m = KeybindingsManager::new();
        assert!(m.matches("\x1b", Keybinding::SelectCancel));
        assert!(m.matches("\x03", Keybinding::SelectCancel));
    }

    #[test]
    fn empty_config_yields_all_defaults() {
        let m = KeybindingsManager::with_config(&KeybindingsConfig::new());
        assert_eq!(
            m.get(Keybinding::EditorCursorLeft),
            &["left".to_string(), "ctrl+b".to_string()]
        );
        assert_eq!(m.get(Keybinding::EditorYank), &["ctrl+y".to_string()]);
    }

    #[test]
    fn override_with_single_key() {
        let cfg = make_config(&[("tui.input.submit", Some(vec!["ctrl+enter"]))]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(m.get(Keybinding::InputSubmit), &["ctrl+enter".to_string()]);
        // Other bindings untouched.
        assert_eq!(
            m.get(Keybinding::SelectConfirm),
            &["enter".to_string()]
        );
    }

    #[test]
    fn override_with_multiple_keys() {
        let cfg = make_config(&[(
            "tui.input.submit",
            Some(vec!["enter", "ctrl+enter"]),
        )]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(
            m.get(Keybinding::InputSubmit),
            &["enter".to_string(), "ctrl+enter".to_string()]
        );
    }

    #[test]
    fn override_does_not_evict_defaults_on_other_bindings() {
        // Mirrors TS test: rebinding select.up should not touch editor.cursorUp.
        let cfg = make_config(&[("tui.select.up", Some(vec!["up", "ctrl+p"]))]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(
            m.get(Keybinding::SelectUp),
            &["up".to_string(), "ctrl+p".to_string()]
        );
        assert_eq!(m.get(Keybinding::EditorCursorUp), &["up".to_string()]);
    }

    #[test]
    fn override_dedupes_keys() {
        let cfg = make_config(&[(
            "tui.input.submit",
            Some(vec!["enter", "enter", "ctrl+enter", "enter"]),
        )]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(
            m.get(Keybinding::InputSubmit),
            &["enter".to_string(), "ctrl+enter".to_string()]
        );
    }

    #[test]
    fn disable_via_none_value() {
        let cfg = make_config(&[("tui.input.submit", None)]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(m.get(Keybinding::InputSubmit), &[] as &[KeyId]);
        assert!(!m.matches("\r", Keybinding::InputSubmit));
    }

    #[test]
    fn disable_via_empty_vec() {
        let cfg = make_config(&[("tui.input.submit", Some(vec![]))]);
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(m.get(Keybinding::InputSubmit), &[] as &[KeyId]);
        assert!(!m.matches("\r", Keybinding::InputSubmit));
    }

    #[test]
    fn unknown_config_id_is_ignored() {
        let mut cfg = make_config(&[("tui.input.submit", Some(vec!["ctrl+enter"]))]);
        cfg.insert(
            "tui.does.not.exist".to_string(),
            Some(vec!["ctrl+x".to_string()]),
        );
        let m = KeybindingsManager::with_config(&cfg);
        assert_eq!(m.get(Keybinding::InputSubmit), &["ctrl+enter".to_string()]);
        assert!(m.conflicts().is_empty());
    }

    #[test]
    fn user_conflicts_reported() {
        // Mirrors TS: same key bound by two user-set bindings.
        let cfg = make_config(&[
            ("tui.input.submit", Some(vec!["ctrl+x"])),
            ("tui.select.confirm", Some(vec!["ctrl+x"])),
        ]);
        let m = KeybindingsManager::with_config(&cfg);
        let conflicts = m.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key_id, "ctrl+x");
        let mut bindings = conflicts[0].bindings.clone();
        bindings.sort_by_key(|b| b.id());
        assert_eq!(
            bindings,
            vec![Keybinding::InputSubmit, Keybinding::SelectConfirm]
        );
    }

    #[test]
    fn defaults_alone_produce_no_conflicts() {
        // TS only reports user-claim conflicts; defaults legitimately overlap.
        let m = KeybindingsManager::new();
        assert!(m.conflicts().is_empty());
    }

    #[test]
    fn conflicts_sorted_by_key_id() {
        let cfg = make_config(&[
            ("tui.input.submit", Some(vec!["ctrl+x", "ctrl+a"])),
            ("tui.select.confirm", Some(vec!["ctrl+x"])),
            ("tui.input.copy", Some(vec!["ctrl+a"])),
        ]);
        let m = KeybindingsManager::with_config(&cfg);
        let conflicts = m.conflicts();
        assert_eq!(conflicts.len(), 2);
        assert_eq!(conflicts[0].key_id, "ctrl+a");
        assert_eq!(conflicts[1].key_id, "ctrl+x");
    }

    #[test]
    fn set_replaces_keys() {
        let mut m = KeybindingsManager::new();
        m.set(Keybinding::InputSubmit, vec!["ctrl+enter".to_string()]);
        assert_eq!(m.get(Keybinding::InputSubmit), &["ctrl+enter".to_string()]);
    }

    #[test]
    fn unset_disables_binding() {
        let mut m = KeybindingsManager::new();
        m.unset(Keybinding::InputSubmit);
        assert_eq!(m.get(Keybinding::InputSubmit), &[] as &[KeyId]);
        assert!(!m.matches("\r", Keybinding::InputSubmit));
    }

    #[test]
    fn empty_keys_do_not_match() {
        let mut m = KeybindingsManager::new();
        m.set(Keybinding::EditorCursorUp, vec![]);
        assert!(!m.matches("\x1b[A", Keybinding::EditorCursorUp));
    }

    #[test]
    fn all_iterates_every_binding() {
        let m = KeybindingsManager::new();
        let count = m.all().count();
        assert_eq!(count, 31);
    }

    #[test]
    fn global_get_after_set_reflects_update() {
        let cfg = make_config(&[("tui.input.submit", Some(vec!["ctrl+enter"]))]);
        set_keybindings(cfg);
        let m = get_keybindings();
        assert_eq!(m.get(Keybinding::InputSubmit), &["ctrl+enter".to_string()]);

        // Reset to defaults so other tests are unaffected.
        set_keybindings(KeybindingsConfig::new());
        let m = get_keybindings();
        assert_eq!(m.get(Keybinding::InputSubmit), &["enter".to_string()]);
    }

    #[test]
    fn description_present_for_every_default() {
        for binding in Keybinding::all() {
            let def = TUI_KEYBINDINGS.get(binding.id()).unwrap();
            assert!(
                def.description.as_deref().map(str::is_empty) == Some(false),
                "missing description for {}",
                binding.id()
            );
        }
    }

    #[test]
    fn multi_default_keys_match_either() {
        let m = KeybindingsManager::new();
        // "delete" and "ctrl+d" both bound to deleteCharForward.
        assert!(m.matches("\x04", Keybinding::EditorDeleteCharForward)); // ctrl+d
    }
}
