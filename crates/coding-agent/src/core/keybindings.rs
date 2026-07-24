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
    /// Open the buffer in the external editor (`$VISUAL` / `$EDITOR`).
    OpenExternalEditor,
    /// Paste the system clipboard into the input buffer.
    PasteClipboard,
    /// Expand / collapse the most-recent collapsible summary.
    ToggleLastSummary,
    /// Move the selection up in a registry-backed selector (tree / resume /
    /// settings / fork). Hardcoded-dispatch selectors keep their built-in keys.
    SelectUp,
    /// Move the selection down in a registry-backed selector.
    SelectDown,
    /// Confirm the highlighted row in a registry-backed selector.
    SelectConfirm,
    /// Cancel / dismiss a registry-backed selector.
    SelectCancel,
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
        Action::OpenExternalEditor,
        Action::PasteClipboard,
        Action::ToggleLastSummary,
        Action::SelectUp,
        Action::SelectDown,
        Action::SelectConfirm,
        Action::SelectCancel,
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
            Action::OpenExternalEditor => "open-external-editor",
            Action::PasteClipboard => "paste-clipboard",
            Action::ToggleLastSummary => "toggle-last-summary",
            Action::SelectUp => "select-up",
            Action::SelectDown => "select-down",
            Action::SelectConfirm => "select-confirm",
            Action::SelectCancel => "select-cancel",
        }
    }

    /// Parse a kebab-case action name.
    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.as_str() == name)
    }

    /// The canonical `key_id` string of this action's built-in default chord.
    ///
    /// Derived from [`KeyBindings::defaults`] — the single source of truth for the
    /// default chords — so a fallback can never drift from the real default. Call
    /// sites that resolve a live binding and need a fallback for an unbound action
    /// (e.g. [`resolved_key_id`](crate::modes::interactive::rt_driver::keys::resolved_key_id))
    /// should use this instead of a hand-written literal, so adding a new `Action`
    /// (with its default in `defaults()`) cannot silently disagree with the
    /// fallback string.
    ///
    /// # Panics
    ///
    /// Never in practice: every `Action` variant has a default chord in
    /// `defaults()`. If a future variant is added without one, this panics loudly
    /// at first use rather than shipping a wrong fallback — the coupling the method
    /// exists to enforce.
    #[must_use]
    pub fn default_key_id(self) -> String {
        KeyBindings::defaults()
            .by_action
            .get(&self)
            .map(KeyChord::to_key_id)
            .unwrap_or_else(|| {
                panic!("Action::{self:?} has no default chord in KeyBindings::defaults()")
            })
    }

    /// Which input surface the action fires on.
    ///
    /// Input-line and selector-overlay actions live in **disjoint modes**: a
    /// key only ever means one thing at a time (the mounted selector captures
    /// every key before it can reach the input line, VAL-OVERLAY-005). So the
    /// same chord bound to a `Select*` action and an input-line action is not a
    /// real conflict — `Enter` is `Submit` at the prompt and `SelectConfirm`
    /// inside a selector. Conflict detection and the reverse chord index are
    /// therefore scoped: only same-scope collisions are reported (VAL-COMPAT-003).
    pub fn scope(&self) -> Scope {
        match self {
            Action::SelectUp
            | Action::SelectDown
            | Action::SelectConfirm
            | Action::SelectCancel => Scope::Selector,
            _ => Scope::Input,
        }
    }
}

/// The input surface an [`Action`] fires on. See [`Action::scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The chat input line and its global toggles (the default surface).
    Input,
    /// A mounted registry-backed selector overlay (tree / resume / settings /
    /// fork), which captures keys before they reach the input line.
    Selector,
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

    /// The canonical `key_id` string this chord matches in the rt input
    /// pipeline.
    ///
    /// This is the **load-bearing bridge** between the app-layer keybindings
    /// table and the rt driver's dispatch: the rt input pump tags every key
    /// with a canonical id via `hand_tui::rt::events::key_event_to_key_id`
    /// (modifier order `shift, ctrl, alt, super`, lowercase base), and the
    /// driver matches those ids against `KeyChord::to_key_id()` so a
    /// user-remapped chord (e.g. `alt+c`) drives its action verbatim. The two
    /// producers MUST agree character-for-character; the round-trip is pinned
    /// by unit tests here and mirrored by the rt-events tests.
    ///
    /// `Cmd`/`super` maps to the `super+` prefix to match the rt canonical
    /// vocabulary.
    #[must_use]
    pub fn to_key_id(&self) -> String {
        let mut out = String::new();
        if self.modifiers.shift {
            out.push_str("shift+");
        }
        if self.modifiers.ctrl {
            out.push_str("ctrl+");
        }
        if self.modifiers.alt {
            out.push_str("alt+");
        }
        if self.modifiers.cmd {
            out.push_str("super+");
        }
        out.push_str(&self.key.base_key_id());
        out
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

impl Key {
    /// The unmodified canonical base name this key contributes to a `key_id`
    /// string, matching `hand_tui::rt::events`' `base_key_name` exactly.
    ///
    /// Arrow keys collapse to `up`/`down`/`left`/`right`, page keys are
    /// camelCase (`pageUp` / `pageDown`) to mirror the rt canonical vocabulary,
    /// a literal space is `space`, and printable chars are lowercased so the
    /// shift is carried by the modifier prefix.
    #[must_use]
    pub fn base_key_id(&self) -> String {
        match self {
            Key::Char(' ') => "space".to_string(),
            Key::Char(c) => c.to_ascii_lowercase().to_string(),
            Key::Enter => "enter".to_string(),
            Key::Tab => "tab".to_string(),
            Key::Backspace => "backspace".to_string(),
            Key::Delete => "delete".to_string(),
            Key::Escape => "escape".to_string(),
            Key::ArrowUp => "up".to_string(),
            Key::ArrowDown => "down".to_string(),
            Key::ArrowLeft => "left".to_string(),
            Key::ArrowRight => "right".to_string(),
            Key::Home => "home".to_string(),
            Key::End => "end".to_string(),
            Key::PageUp => "pageUp".to_string(),
            Key::PageDown => "pageDown".to_string(),
            Key::F(n) => format!("f{n}"),
        }
    }
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

/// A single startup diagnostic from loading a keybindings file.
///
/// Surfaced as a yellow line in the startup transcript so a malformed override
/// is visible without crashing the app (VAL-COMPAT-003). The affected action
/// keeps its previous (default or lower-layer) binding; a conflicting chord is
/// disabled for *every* action it collides with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// An action name that is not in the [`Action`] table — skipped.
    UnknownAction { layer: String, name: String },
    /// A chord string that failed to parse — the override is skipped and the
    /// affected action keeps its prior binding.
    InvalidChord {
        layer: String,
        action: String,
        chord: String,
        reason: String,
    },
    /// One chord bound to multiple same-scope actions — the chord is disabled
    /// for all of them (neither wins).
    Conflict { chord: String, actions: Vec<String> },
}

impl Diagnostic {
    /// A one-line, user-facing rendering for the startup transcript.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Diagnostic::UnknownAction { layer, name } => {
                format!("keybindings ({layer}): unknown action '{name}' — skipped")
            }
            Diagnostic::InvalidChord {
                layer,
                action,
                chord,
                reason,
            } => format!(
                "keybindings ({layer}): invalid chord '{chord}' for '{action}' ({reason}) — kept default"
            ),
            Diagnostic::Conflict { chord, actions } => format!(
                "keybindings: chord '{chord}' bound to {} — disabled for all",
                actions.join(", ")
            ),
        }
    }
}

/// User-facing keybindings table.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    /// action -> chord, after merging defaults + user overrides.
    by_action: HashMap<Action, KeyChord>,
    /// chord -> action (for reverse lookup; populated by `Self::build`).
    /// Reverse resolution is scoped: a chord maps to at most one action *per
    /// scope*, so `Enter` resolves to `Submit` in the input line and
    /// `SelectConfirm` in a selector without either being treated as a
    /// conflict (see [`Action::scope`]).
    by_chord: HashMap<(Scope, KeyChord), Action>,
    /// Conflicts encountered at load time; reported via diagnostics.
    conflicts: Vec<(KeyChord, Vec<Action>)>,
    /// Non-fatal diagnostics from the last load (unknown actions, invalid
    /// chords, conflicts). Empty for [`Self::defaults`].
    diagnostics: Vec<Diagnostic>,
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
        by_action.insert(
            Action::OpenExternalEditor,
            KeyChord::with_mods(Key::Char('g'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::PasteClipboard,
            KeyChord::with_mods(Key::Char('v'), KeyModifiers::CTRL),
        );
        by_action.insert(
            Action::ToggleLastSummary,
            KeyChord::with_mods(Key::Char('r'), KeyModifiers::CTRL),
        );
        by_action.insert(Action::SelectUp, KeyChord::plain(Key::ArrowUp));
        by_action.insert(Action::SelectDown, KeyChord::plain(Key::ArrowDown));
        by_action.insert(Action::SelectConfirm, KeyChord::plain(Key::Enter));
        by_action.insert(Action::SelectCancel, KeyChord::plain(Key::Escape));

        Self::build(by_action, Vec::new())
    }

    /// Load and merge global + project YAML files. Either missing is OK.
    ///
    /// Unknown action names and malformed chord strings are recorded as
    /// [`Diagnostic`]s (and also logged via `tracing::warn!`) then skipped; the
    /// rest of the file is still applied. Only an I/O error (other than "not
    /// found") or a YAML syntax error propagates as an `Err` — a *semantically*
    /// bad entry never aborts the load, so the app always starts (VAL-COMPAT-003).
    pub fn load(
        global_path: Option<&Path>,
        project_path: Option<&Path>,
    ) -> Result<Self, KeyBindingsError> {
        let mut by_action = Self::defaults().by_action;
        let mut diagnostics = Vec::new();

        if let Some(p) = global_path {
            apply_file(&mut by_action, p, "global", &mut diagnostics)?;
        }
        if let Some(p) = project_path {
            apply_file(&mut by_action, p, "project", &mut diagnostics)?;
        }

        Ok(Self::build(by_action, diagnostics))
    }

    /// Load from the standard hand paths, project shadowing global:
    /// - global: `$HAND_HOME/.hand/keybindings.yaml` (or `~/.hand/…`)
    /// - project: `<cwd>/.hand/keybindings.yaml`
    ///
    /// `HAND_HOME` is honoured for the global layer so a test / sandboxed app
    /// can isolate its config (mirrors the session store's home resolution).
    /// Either layer absent is fine.
    pub fn load_for_cwd(cwd: &Path) -> Result<Self, KeyBindingsError> {
        let (global, project) = Self::standard_paths(cwd);
        Self::load(global.as_deref(), project.as_deref())
    }

    /// The standard global + project keybindings paths for `cwd`. Exposed so the
    /// driver can report which files it read on `/reload`.
    #[must_use]
    pub fn standard_paths(cwd: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
        let home = std::env::var_os("HAND_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir);
        let global = home.map(|h| h.join(".hand").join("keybindings.yaml"));
        let project = Some(cwd.join(".hand").join("keybindings.yaml"));
        (global, project)
    }

    /// Resolve an action to its bound chord, if any.
    pub fn resolve(&self, action: Action) -> Option<&KeyChord> {
        self.by_action.get(&action)
    }

    /// The canonical `key_id` string *live* for `action`, if any.
    ///
    /// This is what the rt driver matches an incoming key's id against (see
    /// [`KeyChord::to_key_id`]). Returns `None` when the action is unbound **or**
    /// when its chord conflicts with another same-scope action (and was therefore
    /// dropped from the reverse index) — a disabled chord must not silently fire
    /// the action (VAL-COMPAT-003), so the driver falls back to the built-in id.
    #[must_use]
    pub fn key_id_for(&self, action: Action) -> Option<String> {
        let chord = self.by_action.get(&action)?;
        if self.resolve_chord_in(action.scope(), chord) == Some(action) {
            Some(chord.to_key_id())
        } else {
            None
        }
    }

    /// Reverse-resolve a chord to its action *within `scope`*, if bound.
    ///
    /// When two same-scope actions are bound to the same chord (a conflict),
    /// the chord is removed from the reverse map — neither action wins. Use
    /// [`Self::conflicts`] to surface this to the user.
    pub fn resolve_chord_in(&self, scope: Scope, chord: &KeyChord) -> Option<Action> {
        self.by_chord.get(&(scope, chord.clone())).copied()
    }

    /// Reverse-resolve a chord to its input-scope action, if bound. Retained for
    /// existing input-line call sites; equivalent to
    /// `resolve_chord_in(Scope::Input, chord)`.
    pub fn resolve_chord(&self, chord: &KeyChord) -> Option<Action> {
        self.resolve_chord_in(Scope::Input, chord)
    }

    /// Reverse-resolve a canonical `key_id` string (as produced by the rt input
    /// pump) to its action within `scope`, if bound and not conflicting.
    #[must_use]
    pub fn action_for_key_id(&self, scope: Scope, key_id: &str) -> Option<Action> {
        for ((s, chord), action) in &self.by_chord {
            if *s == scope && chord.to_key_id() == key_id {
                return Some(*action);
            }
        }
        None
    }

    /// Conflicts (chord bound to multiple actions). Reported once per load.
    pub fn conflicts(&self) -> &[(KeyChord, Vec<Action>)] {
        &self.conflicts
    }

    /// Non-fatal diagnostics from the last load (empty for defaults).
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Internal: build the scoped reverse index and conflict list from an
    /// action -> chord map, appending any conflict diagnostics to `diagnostics`.
    ///
    /// Conflicts are detected **per scope** (see [`Action::scope`]): a chord
    /// bound to one input-line action and one selector action is not a conflict,
    /// because the two never contend for the same keypress.
    fn build(by_action: HashMap<Action, KeyChord>, mut diagnostics: Vec<Diagnostic>) -> Self {
        // Group actions by (scope, chord) to detect same-scope collisions.
        let mut grouped: HashMap<(Scope, KeyChord), Vec<Action>> = HashMap::new();
        for (action, chord) in &by_action {
            grouped
                .entry((action.scope(), chord.clone()))
                .or_default()
                .push(*action);
        }

        let mut by_chord = HashMap::new();
        let mut conflicts = Vec::new();
        for ((scope, chord), mut actions) in grouped {
            if actions.len() == 1 {
                by_chord.insert((scope, chord), actions[0]);
            } else {
                // Stable order so diagnostics are reproducible.
                actions.sort_by_key(|a| a.as_str());
                tracing::warn!(
                    chord = ?chord,
                    actions = ?actions,
                    "keybinding conflict: chord bound to multiple actions"
                );
                diagnostics.push(Diagnostic::Conflict {
                    chord: chord.to_key_id(),
                    actions: actions.iter().map(|a| a.as_str().to_string()).collect(),
                });
                conflicts.push((chord, actions));
            }
        }

        Self {
            by_action,
            by_chord,
            conflicts,
            diagnostics,
        }
    }
}

fn apply_file(
    by_action: &mut HashMap<Action, KeyChord>,
    path: &Path,
    layer: &str,
    diagnostics: &mut Vec<Diagnostic>,
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
            diagnostics.push(Diagnostic::UnknownAction {
                layer: layer.to_string(),
                name: action_name,
            });
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
                diagnostics.push(Diagnostic::InvalidChord {
                    layer: layer.to_string(),
                    action: action_name,
                    chord: chord_str,
                    reason: e.to_string(),
                });
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
    fn default_key_id_matches_defaults_for_every_action() {
        let kb = KeyBindings::defaults();
        for action in Action::ALL {
            let via_method = action.default_key_id();
            let via_defaults = kb
                .by_action
                .get(action)
                .map(KeyChord::to_key_id)
                .expect("every action has a default chord");
            assert_eq!(
                via_method, via_defaults,
                "default_key_id drifted from defaults() for {action:?}",
            );
        }
        // Pin a couple of concrete fallbacks the driver relies on.
        assert_eq!(Action::Submit.default_key_id(), "enter");
        assert_eq!(Action::CopyLastMessage.default_key_id(), "ctrl+x");
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

    // --- to_key_id: the rt-canonical bridge (VAL-COMPAT-001) -------------------

    #[test]
    fn to_key_id_matches_rt_canonical_strings() {
        // These strings MUST equal what hand_tui::rt::events::key_event_to_key_id
        // emits for the equivalent key, or the driver's dispatch silently misses.
        assert_eq!(
            KeyChord::with_mods(Key::Char('c'), KeyModifiers::CTRL).to_key_id(),
            "ctrl+c"
        );
        assert_eq!(
            KeyChord::with_mods(Key::Char('c'), KeyModifiers::ALT).to_key_id(),
            "alt+c"
        );
        assert_eq!(KeyChord::plain(Key::Enter).to_key_id(), "enter");
        assert_eq!(KeyChord::plain(Key::Escape).to_key_id(), "escape");
        assert_eq!(KeyChord::plain(Key::ArrowUp).to_key_id(), "up");
        assert_eq!(KeyChord::plain(Key::ArrowDown).to_key_id(), "down");
        assert_eq!(KeyChord::plain(Key::PageUp).to_key_id(), "pageUp");
        assert_eq!(KeyChord::plain(Key::Char(' ')).to_key_id(), "space");
        assert_eq!(KeyChord::plain(Key::F(5)).to_key_id(), "f5");
        // Modifier order is shift, ctrl, alt, super — matching the rt vocabulary.
        assert_eq!(
            KeyChord::with_mods(
                Key::Char('p'),
                KeyModifiers {
                    shift: true,
                    ctrl: true,
                    ..KeyModifiers::default()
                }
            )
            .to_key_id(),
            "shift+ctrl+p"
        );
        assert_eq!(
            KeyChord::with_mods(
                Key::Char('a'),
                KeyModifiers {
                    ctrl: true,
                    alt: true,
                    ..KeyModifiers::default()
                }
            )
            .to_key_id(),
            "ctrl+alt+a"
        );
        // Cmd collapses to super+ (matches rt's HYPER/META/SUPER → super).
        assert_eq!(
            KeyChord::with_mods(
                Key::Char('k'),
                KeyModifiers {
                    cmd: true,
                    ..KeyModifiers::default()
                }
            )
            .to_key_id(),
            "super+k"
        );
    }

    #[test]
    fn parse_chord_then_to_key_id_round_trips() {
        for raw in ["ctrl+s", "alt+enter", "escape", "up", "ctrl+shift+p"] {
            let chord = parse_chord(raw).unwrap();
            // Not a byte-identical round-trip (order is re-canonicalized), but
            // the produced key_id must re-parse to the same chord.
            let id = chord.to_key_id();
            let reparsed = parse_chord(&id).unwrap();
            assert_eq!(reparsed, chord, "round trip failed for {raw:?} -> {id:?}");
        }
    }

    // --- scope: input vs selector do not collide (VAL-COMPAT-003) --------------

    #[test]
    fn same_chord_across_scopes_is_not_a_conflict() {
        // Enter is Submit (input) AND SelectConfirm (selector) by default — this
        // is NOT a conflict, because a mounted selector captures the key first.
        let kb = KeyBindings::defaults();
        assert!(kb.conflicts().is_empty(), "{:?}", kb.conflicts());
        assert_eq!(
            kb.resolve_chord_in(Scope::Input, &KeyChord::plain(Key::Enter)),
            Some(Action::Submit)
        );
        assert_eq!(
            kb.resolve_chord_in(Scope::Selector, &KeyChord::plain(Key::Enter)),
            Some(Action::SelectConfirm)
        );
    }

    #[test]
    fn action_for_key_id_resolves_within_scope() {
        let kb = KeyBindings::defaults();
        assert_eq!(
            kb.action_for_key_id(Scope::Selector, "up"),
            Some(Action::SelectUp)
        );
        assert_eq!(
            kb.action_for_key_id(Scope::Input, "up"),
            Some(Action::HistoryPrev)
        );
        assert_eq!(kb.action_for_key_id(Scope::Selector, "f9"), None);
    }

    #[test]
    fn key_id_for_reflects_user_override() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "copy-last-message: alt+c\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert_eq!(
            kb.key_id_for(Action::CopyLastMessage).as_deref(),
            Some("alt+c")
        );
    }

    #[test]
    fn selector_navigation_key_is_remappable() {
        // A user rebinds selector-down to `j`; the registry-backed selectors read
        // this via key_id_for(SelectDown) (VAL-OVERLAY-021).
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "select-down: j\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert_eq!(kb.key_id_for(Action::SelectDown).as_deref(), Some("j"));
        assert_eq!(
            kb.action_for_key_id(Scope::Selector, "j"),
            Some(Action::SelectDown)
        );
        // The default `down` no longer drives SelectDown.
        assert_eq!(kb.action_for_key_id(Scope::Selector, "down"), None);
    }

    // --- diagnostics: visible, non-fatal (VAL-COMPAT-003) ----------------------

    #[test]
    fn unknown_action_records_diagnostic() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "bogus: ctrl+x\nsubmit: ctrl+s\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert!(
            kb.diagnostics().iter().any(|d| matches!(
                d,
                Diagnostic::UnknownAction { name, .. } if name == "bogus"
            )),
            "{:?}",
            kb.diagnostics()
        );
        // The valid sibling binding still applied.
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::with_mods(Key::Char('s'), KeyModifiers::CTRL))
        );
    }

    #[test]
    fn invalid_chord_records_diagnostic_and_keeps_default() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "submit: \"hyper+s\"\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert!(
            kb.diagnostics().iter().any(|d| matches!(
                d,
                Diagnostic::InvalidChord { action, .. } if action == "submit"
            )),
            "{:?}",
            kb.diagnostics()
        );
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::plain(Key::Enter))
        );
    }

    #[test]
    fn conflict_records_diagnostic_and_disables_chord() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir, "kb.yaml", "submit: ctrl+x\ncancel: ctrl+x\n");
        let kb = KeyBindings::load(Some(&path), None).unwrap();
        assert!(
            kb.diagnostics()
                .iter()
                .any(|d| matches!(d, Diagnostic::Conflict { chord, .. } if chord == "ctrl+x")),
            "{:?}",
            kb.diagnostics()
        );
        // Both actions lose the chord (neither wins).
        assert!(
            kb.resolve_chord(&KeyChord::with_mods(Key::Char('x'), KeyModifiers::CTRL))
                .is_none()
        );
    }

    #[test]
    fn defaults_have_no_diagnostics() {
        assert!(KeyBindings::defaults().diagnostics().is_empty());
    }

    // --- load_for_cwd: project shadows global, HAND_HOME isolation -------------

    #[test]
    fn load_for_cwd_project_shadows_global() {
        // Isolate HOME/HAND_HOME so this never reads a developer's real config.
        let home = TempDir::new().unwrap();
        let global_dir = home.path().join(".hand");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(global_dir.join("keybindings.yaml"), "submit: ctrl+s\n").unwrap();

        let project = TempDir::new().unwrap();
        let project_dir = project.path().join(".hand");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("keybindings.yaml"), "submit: alt+enter\n").unwrap();

        let _guard = HandHomeGuard::set(home.path());
        let kb = KeyBindings::load_for_cwd(project.path()).unwrap();
        assert_eq!(
            kb.resolve(Action::Submit),
            Some(&KeyChord::with_mods(Key::Enter, KeyModifiers::ALT)),
            "project layer wins over global",
        );
    }

    /// Serialize `HAND_HOME` mutation and restore it on drop so a test never
    /// leaks env state onto a sibling.
    struct HandHomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HandHomeGuard {
        fn set(home: &Path) -> Self {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("HAND_HOME");
            // SAFETY: LOCK is held for the guard's lifetime, serializing env mutation.
            unsafe {
                std::env::set_var("HAND_HOME", home);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for HandHomeGuard {
        fn drop(&mut self) {
            // SAFETY: LOCK is still held (we own the guard).
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("HAND_HOME", v),
                    None => std::env::remove_var("HAND_HOME"),
                }
            }
        }
    }
}
