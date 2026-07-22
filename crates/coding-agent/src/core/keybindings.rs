//! Keybindings configuration.
//!
//! User-customizable bindings layered on top of defaults. Resolution order:
//! project (`<cwd>/.hand/keybindings.yaml`) > global
//! (`~/.hand/keybindings.yaml`) > defaults.
//!
//! Each action has at most one chord; multiple chords for one action would
//! require palette UX work — defer to a later phase.
//!
//! The chord string format is intentionally simple:
//!
//! - `"ctrl+s"` -> `Char('s')` + `Ctrl`
//! - `"alt+enter"` -> `Enter` + `Alt`
//! - `"escape"` -> `Escape`
//! - `"f5"` -> `F(5)`
//! - `"ctrl+shift+up"` -> `ArrowUp` + `Ctrl + Shift`
//!
//! All lowercase. Modifiers in any order before the key. Single `+`
//! separator. The `KeyChord` representation is for keybindings
//! persistence/parsing only; it does not interop with runtime input event
//! types yet (T5 will define those and translate if needed).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Every action that the interactive REPL can fire via a keybinding.
///
/// Names match the TS reference where possible. Kept tight for v1; new
/// actions can be added later.
// TODO: extend Action enum as new keybindable surfaces appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Submit the current prompt to the agent.
    Submit,
    /// Cancel an in-flight operation (or clear input when idle).
    Cancel,
    /// Insert a literal newline in the input buffer.
    NewLine,
    /// Move backward through input history.
    HistoryPrev,
    /// Move forward through input history.
    HistoryNext,
    /// Delete the previous word.
    DeleteWordBack,
    /// Kill text from cursor to end of line.
    KillToEnd,
    /// Kill text from cursor to start of line.
    KillToStart,
    /// Quit the REPL.
    Quit,
    /// Toggle the model's reasoning/thinking display.
    ToggleThinking,
    /// Open the slash-command palette.
    OpenSlashPalette,
    /// Copy the last assistant message to the clipboard (same routine
    /// as `/copy`).
    CopyLastMessage,
}

impl Action {
    /// All variants, in declaration order. Useful for iterating defaults.
    pub const ALL: &'static [Action] = &[
        Action::Submit,
        Action::Cancel,
        Action::NewLine,
        Action::HistoryPrev,
        Action::HistoryNext,
        Action::DeleteWordBack,
        Action::KillToEnd,
        Action::KillToStart,
        Action::Quit,
        Action::ToggleThinking,
        Action::OpenSlashPalette,
        Action::CopyLastMessage,
    ];

    /// Kebab-case name as used in YAML config keys.
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Submit => "submit",
            Action::Cancel => "cancel",
            Action::NewLine => "new-line",
            Action::HistoryPrev => "history-prev",
            Action::HistoryNext => "history-next",
            Action::DeleteWordBack => "delete-word-back",
            Action::KillToEnd => "kill-to-end",
            Action::KillToStart => "kill-to-start",
            Action::Quit => "quit",
            Action::ToggleThinking => "toggle-thinking",
            Action::OpenSlashPalette => "open-slash-palette",
            Action::CopyLastMessage => "copy-last-message",
        }
    }

    /// Parse a kebab-case action name.
    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.as_str() == name)
    }
}

/// A single keystroke (key + modifiers).
///
/// Chord sequences (e.g., `Ctrl+X Ctrl+S`) are deferred — v1 supports only
/// single chords.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KeyChord {
    pub key: Key,
    #[serde(default)]
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// Construct a chord with no modifiers.
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            modifiers: KeyModifiers::default(),
        }
    }

    /// Construct a chord with the given modifiers.
    pub fn with_mods(key: Key, modifiers: KeyModifiers) -> Self {
        Self { key, modifiers }
    }
}

/// A keyboard key. Extend as new surfaces require it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Key {
    /// A printable character.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KeyModifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub cmd: bool,
}

impl KeyModifiers {
    /// `Ctrl` only.
    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
        cmd: false,
    };
    /// `Alt` only.
    pub const ALT: Self = Self {
        ctrl: false,
        alt: true,
        shift: false,
        cmd: false,
    };
    /// `Shift` only.
    pub const SHIFT: Self = Self {
        ctrl: false,
        alt: false,
        shift: true,
        cmd: false,
    };
}

#[derive(Debug, Error)]
pub enum KeyBindingsError {
    #[error("I/O error reading {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML parse error in {path}: {source}", path = .path.display())]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid chord {raw:?} for action {action:?}: {reason}")]
    InvalidChord {
        raw: String,
        action: String,
        reason: String,
    },
}

/// Wire format for the keybindings YAML file.
///
/// The file is a flat map from action name (kebab-case) to chord string
/// (e.g., `"ctrl+s"`, `"alt+enter"`, `"f5"`, `"escape"`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct KeyBindingsFile {
    pub bindings: HashMap<String, String>,
}

/// User-facing keybindings table.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    /// action -> chord, after merging defaults + user overrides.
    by_action: HashMap<Action, KeyChord>,
    /// chord -> action (for reverse lookup; populated by `Self::build`).
    by_chord: HashMap<KeyChord, Action>,
    /// Conflicts encountered at load time; reported via diagnostics.
    conflicts: Vec<(KeyChord, Vec<Action>)>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl KeyBindings {
    /// Default keybindings. Hardcoded; one chord per `Action`.
    pub fn defaults() -> Self {
        let mut by_action: HashMap<Action, KeyChord> = HashMap::new();

        by_action.insert(Action::Submit, KeyChord::plain(Key::Enter));
        by_action.insert(Action::Cancel, KeyChord::plain(Key::Escape));
        by_action.insert(
            Action::NewLine,
            KeyChord::with_mods(Key::Enter, KeyModifiers::SHIFT),
        );
        by_action.insert(Action::HistoryPrev, KeyChord::plain(Key::ArrowUp));
        by_action.insert(Action::HistoryNext, KeyChord::plain(Key::ArrowDown));
        by_action.insert(
            Action::DeleteWordBack,
            KeyChord::with_mods(Key::Char('w'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::KillToEnd,
            KeyChord::with_mods(Key::Char('k'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::KillToStart,
            KeyChord::with_mods(Key::Char('u'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::Quit,
            KeyChord::with_mods(Key::Char('c'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::ToggleThinking,
            KeyChord::with_mods(Key::Char('t'), KeyModifiers::CTRL),
        );
        by_action.insert(Action::OpenSlashPalette, KeyChord::plain(Key::Char('/')));
        by_action.insert(
            Action::CopyLastMessage,
            KeyChord::with_mods(Key::Char('x'), KeyModifiers::CTRL),
        );

        Self::build(by_action)
    }

    /// Load and merge global + project YAML files. Either missing is OK.
    ///
    /// Unknown action names and malformed chord strings are logged via
    /// `tracing::warn!` and skipped; the rest of the file is still applied.
    pub fn load(
        global_path: Option<&Path>,
        project_path: Option<&Path>,
    ) -> Result<Self, KeyBindingsError> {
        let mut by_action = Self::defaults().by_action;

        if let Some(p) = global_path {
            apply_file(&mut by_action, p, "global")?;
        }
        if let Some(p) = project_path {
            apply_file(&mut by_action, p, "project")?;
        }

        Ok(Self::build(by_action))
    }

    /// Resolve an action to its bound chord, if any.
    pub fn resolve(&self, action: Action) -> Option<&KeyChord> {
        self.by_action.get(&action)
    }

    /// Reverse-resolve a chord to its action, if bound.
    ///
    /// When two actions are bound to the same chord (a conflict), the chord
    /// is removed from the reverse map — neither action wins. Use
    /// [`Self::conflicts`] to surface this to the user.
    pub fn resolve_chord(&self, chord: &KeyChord) -> Option<Action> {
        self.by_chord.get(chord).copied()
    }

    /// Conflicts (chord bound to multiple actions). Reported once per load.
    pub fn conflicts(&self) -> &[(KeyChord, Vec<Action>)] {
        &self.conflicts
    }

    /// Internal: build the reverse index and conflict list from an
    /// action -> chord map.
    fn build(by_action: HashMap<Action, KeyChord>) -> Self {
        // First, group actions by chord to detect collisions.
        let mut grouped: HashMap<KeyChord, Vec<Action>> = HashMap::new();
        for (action, chord) in &by_action {
            grouped.entry(chord.clone()).or_default().push(*action);
        }

        let mut by_chord = HashMap::new();
        let mut conflicts = Vec::new();
        for (chord, mut actions) in grouped {
            if actions.len() == 1 {
                by_chord.insert(chord, actions[0]);
            } else {
                // Stable order so diagnostics are reproducible.
                actions.sort_by_key(|a| a.as_str());
                tracing::warn!(
                    chord = ?chord,
                    actions = ?actions,
                    "keybinding conflict: chord bound to multiple actions"
                );
                conflicts.push((chord, actions));
            }
        }

        Self {
            by_action,
            by_chord,
            conflicts,
        }
    }
}

fn apply_file(
    by_action: &mut HashMap<Action, KeyChord>,
    path: &Path,
    layer: &str,
) -> Result<(), KeyBindingsError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(KeyBindingsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    if raw.trim().is_empty() {
        return Ok(());
    }

    let file: KeyBindingsFile =
        serde_yaml::from_str(&raw).map_err(|source| KeyBindingsError::Yaml {
            path: path.to_path_buf(),
            source,
        })?;

    for (action_name, chord_str) in file.bindings {
        let Some(action) = Action::from_name(&action_name) else {
            tracing::warn!(
                layer,
                path = %path.display(),
                action = %action_name,
                "unknown action in keybindings file; skipping",
            );
            continue;
        };
        match parse_chord(&chord_str) {
            Ok(chord) => {
                by_action.insert(action, chord);
            }
            Err(e) => {
                tracing::warn!(
                    layer,
                    path = %path.display(),
                    action = %action_name,
                    chord = %chord_str,
                    error = %e,
                    "invalid chord in keybindings file; skipping",
                );
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChordParseError {
    #[error("empty chord")]
    Empty,
    #[error("unknown modifier {modifier:?}")]
    UnknownModifier { modifier: String },
    #[error("unknown key {key:?}")]
    UnknownKey { key: String },
}

/// Parse a chord string like `"ctrl+shift+s"` or `"alt+enter"` into a
/// [`KeyChord`].
///
/// Lowercase only. Modifiers may appear in any order, separated by `+`.
/// The final segment is the key.
pub fn parse_chord(raw: &str) -> Result<KeyChord, ChordParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ChordParseError::Empty);
    }

    let segments: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        // Catches "ctrl+", "+s", and "ctrl++s".
        return Err(ChordParseError::UnknownKey { key: String::new() });
    }

    let (key_seg, mod_segs) = segments
        .split_last()
        .expect("non-empty after empty-segment check");

    let mut modifiers = KeyModifiers::default();
    for m in mod_segs {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => modifiers.ctrl = true,
            "alt" | "option" | "opt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "cmd" | "meta" | "super" | "win" => modifiers.cmd = true,
            other => {
                return Err(ChordParseError::UnknownModifier {
                    modifier: other.to_string(),
                });
            }
        }
    }

    let key = parse_key(key_seg)?;
    Ok(KeyChord { key, modifiers })
}

fn parse_key(raw: &str) -> Result<Key, ChordParseError> {
    let lower = raw.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "escape" | "esc" => Key::Escape,
        "up" | "arrow-up" | "arrowup" => Key::ArrowUp,
        "down" | "arrow-down" | "arrowdown" => Key::ArrowDown,
        "left" | "arrow-left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrow-right" | "arrowright" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "page-up" | "pgup" => Key::PageUp,
        "pagedown" | "page-down" | "pgdn" => Key::PageDown,
        s if s.starts_with('f') && s.len() >= 2 => {
            // f1..f24
            let n: u8 = s[1..].parse().map_err(|_| ChordParseError::UnknownKey {
                key: raw.to_string(),
            })?;
            if !(1..=24).contains(&n) {
                return Err(ChordParseError::UnknownKey {
                    key: raw.to_string(),
                });
            }
            Key::F(n)
        }
        s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap()),
        _ => {
            return Err(ChordParseError::UnknownKey {
                key: raw.to_string(),
            });
        }
    };
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_yaml(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_are_fully_populated() {
        let kb = KeyBindings::defaults();
        for action in Action::ALL {
            assert!(
                kb.resolve(*action).is_some(),
                "missing default binding for {:?}",
                action
            );
        }
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::plain(Key::Enter))
        );
        assert_eq!(
            kb.resolve(Action::Quit),
            Some(&KeyChord::with_mods(Key::Char('c'), KeyModifiers::CTRL))
        );
        assert_eq!(
            kb.resolve(Action::CopyLastMessage),
            Some(&KeyChord::with_mods(Key::Char('x'), KeyModifiers::CTRL))
        );
        assert!(kb.conflicts().is_empty());
    }

    #[test]
    fn copy_last_message_is_remappable() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "copy-last-message: alt+c\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert_eq!(
            kb.resolve(Action::CopyLastMessage),
            Some(&KeyChord::with_mods(Key::Char('c'), KeyModifiers::ALT))
        );
        // The vacated default no longer reverse-resolves to the action.
        assert_eq!(
            kb.resolve_chord(&KeyChord::with_mods(Key::Char('x'), KeyModifiers::CTRL)),
            None
        );
    }

    #[test]
    fn empty_user_file_preserves_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "keybindings.yaml", "");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        let defaults = KeyBindings::defaults();
        for action in Action::ALL {
            assert_eq!(kb.resolve(*action), defaults.resolve(*action));
        }
    }

    #[test]
    fn missing_file_is_ok() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.yaml");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::plain(Key::Enter))
        );
    }

    #[test]
    fn user_override_replaces_default() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "global.yaml", "submit: ctrl+s\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::with_mods(Key::Char('s'), KeyModifiers::CTRL))
        );
        // Default for an unrelated action is preserved.
        assert_eq!(
            kb.resolve(Action::Cancel),
            Some(&KeyChord::plain(Key::Escape))
        );
    }

    #[test]
    fn project_shadows_global() {
        let dir = TempDir::new().unwrap();
        let global = write_yaml(&dir, "global.yaml", "submit: ctrl+s\n");
        let project = write_yaml(&dir, "project.yaml", "submit: alt+enter\n");
        let kb = KeyBindings::load(Some(&global), Some(&project)).unwrap();
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::with_mods(Key::Enter, KeyModifiers::ALT))
        );
    }

    #[test]
    fn unknown_action_name_is_skipped() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "bogus: ctrl+x\nsubmit: ctrl+s\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        // The valid binding is applied.
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::with_mods(Key::Char('s'), KeyModifiers::CTRL))
        );
    }

    #[test]
    fn invalid_chord_is_skipped() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "submit: \"ctrl+\"\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        // Default preserved because the override was malformed.
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::plain(Key::Enter))
        );
    }

    #[test]
    fn conflict_detected_when_two_actions_share_chord() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "submit: ctrl+x\ncancel: ctrl+x\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        let conflicts = kb.conflicts();
        assert_eq!(
            conflicts.len(),
            1,
            "expected one conflict, got {:?}",
            conflicts
        );
        let (chord, actions) = &conflicts[0];
        assert_eq!(
            chord,
            &KeyChord::with_mods(Key::Char('x'), KeyModifiers::CTRL)
        );
        assert!(actions.contains(&Action::Submit));
        assert!(actions.contains(&Action::Cancel));
        // Conflicting chord does not reverse-resolve.
        assert!(kb.resolve_chord(chord).is_none());
    }

    #[test]
    fn parse_chord_happy_paths() {
        assert_eq!(
            parse_chord("ctrl+s").unwrap(),
            KeyChord::with_mods(Key::Char('s'), KeyModifiers::CTRL)
        );
        assert_eq!(
            parse_chord("alt+enter").unwrap(),
            KeyChord::with_mods(Key::Enter, KeyModifiers::ALT)
        );
        assert_eq!(parse_chord("escape").unwrap(), KeyChord::plain(Key::Escape));
        assert_eq!(parse_chord("f5").unwrap(), KeyChord::plain(Key::F(5)));
        assert_eq!(
            parse_chord("ctrl+shift+up").unwrap(),
            KeyChord::with_mods(
                Key::ArrowUp,
                KeyModifiers {
                    ctrl: true,
                    shift: true,
                    ..KeyModifiers::default()
                }
            )
        );
    }

    #[test]
    fn parse_chord_errors() {
        assert_eq!(parse_chord(""), Err(ChordParseError::Empty));
        assert_eq!(parse_chord("   "), Err(ChordParseError::Empty));
        assert!(matches!(
            parse_chord("hyper+s"),
            Err(ChordParseError::UnknownModifier { .. })
        ));
        assert!(matches!(
            parse_chord("ctrl+nopekey"),
            Err(ChordParseError::UnknownKey { .. })
        ));
        // Trailing '+' (empty key segment).
        assert!(matches!(
            parse_chord("ctrl+"),
            Err(ChordParseError::UnknownKey { .. })
        ));
    }

    #[test]
    fn resolve_chord_reverse_lookup() {
        let kb = KeyBindings::defaults();
        assert_eq!(
            kb.resolve_chord(&KeyChord::plain(Key::Enter)),
            Some(Action::Submit)
        );
        assert_eq!(
            kb.resolve_chord(&KeyChord::plain(Key::Escape)),
            Some(Action::Cancel)
        );
        assert_eq!(
            kb.resolve_chord(&KeyChord::with_mods(Key::Char('c'), KeyModifiers::CTRL)),
            Some(Action::Quit)
        );
        // Unbound chord returns None.
        assert_eq!(kb.resolve_chord(&KeyChord::plain(Key::F(12))), None);
    }

    #[test]
    fn action_name_roundtrip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.as_str()), Some(*a));
        }
        assert_eq!(Action::from_name("not-an-action"), None);
    }
}
