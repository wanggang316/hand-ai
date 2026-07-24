//! App-layer keybindings wired into the rt input pipeline.
//!
//! This is the M3 "keybindings wiring" seam (Decision Log 2026-07-24, option A):
//! the durable app-layer table [`crate::core::keybindings::KeyBindings`] is the
//! single source of truth for what a key does. The legacy `hand_tui::keybindings`
//! registry is an M4 retirement target and is deliberately **not** consulted here.
//!
//! Two things live here:
//!
//! - [`SharedKeybindings`] — the live table behind an `Arc<Mutex<…>>`, so `/reload`
//!   can swap it (new chords fire, old ones stop) while the input loop and the
//!   selectors read it.
//! - [`NavKeys`] — a resolved snapshot of the registry-backed selector navigation
//!   ids (up / down / confirm / cancel), taken when a selector opens. Snapshotting
//!   keeps the selector's `handle_key` a pure, sync function while still honouring
//!   the user's custom keys (VAL-OVERLAY-021).
//!
//! The rt input pump tags every key with a canonical id string
//! (`hand_tui::rt::events::key_event_to_key_id`); the driver matches those ids
//! against [`KeyChord::to_key_id`](crate::core::keybindings::KeyChord::to_key_id),
//! which is pinned to produce the identical string. Nothing here re-implements key
//! parsing — the rt pipeline (M1) owns that, read-only.

use std::sync::{Arc, Mutex};

use crate::core::keybindings::{Action, KeyBindings};

/// The live app-layer keybindings, shared into the input loop and swapped by
/// `/reload`. A blocking `Mutex`: every read is a tiny, non-awaiting lookup.
pub type SharedKeybindings = Arc<Mutex<KeyBindings>>;

/// Wrap a [`KeyBindings`] in the shared handle.
#[must_use]
pub fn new_shared_keybindings(bindings: KeyBindings) -> SharedKeybindings {
    Arc::new(Mutex::new(bindings))
}

/// Resolve the canonical `key_id` string currently bound to `action`.
///
/// Falls back to `default_id` when the action is unbound (e.g. the user cleared
/// it via a conflicting override), so a global toggle never silently vanishes.
#[must_use]
pub fn resolved_key_id(bindings: &SharedKeybindings, action: Action, default_id: &str) -> String {
    bindings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .key_id_for(action)
        .unwrap_or_else(|| default_id.to_string())
}

/// A resolved snapshot of the registry-backed selector navigation ids.
///
/// Taken when a selector mounts, from the live [`KeyBindings`], so the mounted
/// selector honours the user's custom nav keys for its whole lifetime. Only the
/// *registry-backed* selectors (tree / resume / settings / fork) consult this;
/// the hardcoded-dispatch selectors (model / oauth / scoped-models / thinking /
/// theme) keep their built-in keys, so VAL-OVERLAY-021's two families stay
/// distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavKeys {
    /// The key_id that moves the selection up (default `"up"`).
    pub up: String,
    /// The key_id that moves the selection down (default `"down"`).
    pub down: String,
    /// The key_id that confirms the highlighted row (default `"enter"`).
    pub confirm: String,
    /// The key_id that cancels / dismisses the selector (default `"escape"`).
    pub cancel: String,
}

impl Default for NavKeys {
    fn default() -> Self {
        Self {
            up: "up".to_string(),
            down: "down".to_string(),
            confirm: "enter".to_string(),
            cancel: "escape".to_string(),
        }
    }
}

impl NavKeys {
    /// Snapshot the selector navigation ids from the live table, falling back to
    /// the built-in ids for any unbound action.
    #[must_use]
    pub fn from_bindings(bindings: &KeyBindings) -> Self {
        let d = Self::default();
        Self {
            up: bindings.key_id_for(Action::SelectUp).unwrap_or(d.up),
            down: bindings.key_id_for(Action::SelectDown).unwrap_or(d.down),
            confirm: bindings
                .key_id_for(Action::SelectConfirm)
                .unwrap_or(d.confirm),
            cancel: bindings
                .key_id_for(Action::SelectCancel)
                .unwrap_or(d.cancel),
        }
    }

    /// Snapshot from the shared handle (locks briefly).
    #[must_use]
    pub fn snapshot(bindings: &SharedKeybindings) -> Self {
        Self::from_bindings(&bindings.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whether `key_id` moves the selection up.
    #[must_use]
    pub fn is_up(&self, key_id: &str) -> bool {
        key_id == self.up
    }

    /// Whether `key_id` moves the selection down.
    #[must_use]
    pub fn is_down(&self, key_id: &str) -> bool {
        key_id == self.down
    }

    /// Whether `key_id` confirms the highlighted row.
    #[must_use]
    pub fn is_confirm(&self, key_id: &str) -> bool {
        key_id == self.confirm
    }

    /// Whether `key_id` cancels the selector.
    #[must_use]
    pub fn is_cancel(&self, key_id: &str) -> bool {
        key_id == self.cancel
    }

    /// The `↑/↓ navigate   Enter pick   Esc cancel` hint line, rendered from the
    /// resolved keys so the hint always tells the truth (VAL-OVERLAY-021).
    ///
    /// `pick_verb` / `cancel_verb` let each selector keep its own wording (`pick`
    /// vs `open`, `cancel`).
    #[must_use]
    pub fn hint_line(&self, pick_verb: &str, cancel_verb: &str) -> String {
        format!(
            "{}/{} navigate   {} {}   {} {}",
            hint_label(&self.up),
            hint_label(&self.down),
            hint_label(&self.confirm),
            pick_verb,
            hint_label(&self.cancel),
            cancel_verb,
        )
    }
}

/// A short, human-facing label for a `key_id`, so the default arrows still read
/// as `↑` / `↓` and custom keys show verbatim.
#[must_use]
pub fn hint_label(key_id: &str) -> String {
    match key_id {
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "enter" => "Enter".to_string(),
        "escape" => "Esc".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keybindings::{Key, KeyBindings, KeyBindingsFile};
    use std::io::Write;

    fn bindings_from(yaml: &str) -> KeyBindings {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("kb.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(yaml.as_bytes()).unwrap();
        KeyBindings::load(Some(&path), None).unwrap()
    }

    #[test]
    fn navkeys_default_matches_built_in_ids() {
        let nav = NavKeys::from_bindings(&KeyBindings::defaults());
        assert_eq!(nav, NavKeys::default());
        assert!(nav.is_up("up"));
        assert!(nav.is_down("down"));
        assert!(nav.is_confirm("enter"));
        assert!(nav.is_cancel("escape"));
    }

    #[test]
    fn navkeys_follow_custom_selector_bindings() {
        let kb = bindings_from("select-down: j\nselect-up: k\n");
        let nav = NavKeys::from_bindings(&kb);
        assert!(nav.is_down("j"));
        assert!(nav.is_up("k"));
        // The defaults no longer navigate.
        assert!(!nav.is_down("down"));
        assert!(!nav.is_up("up"));
    }

    #[test]
    fn hint_line_reflects_custom_keys() {
        let kb = bindings_from("select-down: j\n");
        let nav = NavKeys::from_bindings(&kb);
        let hint = nav.hint_line("pick", "cancel");
        assert!(hint.contains("↑/j"), "custom down key shown: {hint}");
        assert!(hint.contains("Enter pick"), "{hint}");
        assert!(hint.contains("Esc cancel"), "{hint}");
    }

    #[test]
    fn resolved_key_id_falls_back_when_unbound() {
        // Bind two input actions to the same chord so it conflicts and is dropped
        // for both — copy-last-message then has no chord.
        let kb = bindings_from("copy-last-message: ctrl+t\ntoggle-thinking: ctrl+t\n");
        let shared = new_shared_keybindings(kb);
        assert_eq!(
            resolved_key_id(&shared, Action::CopyLastMessage, "ctrl+x"),
            "ctrl+x",
            "unbound action falls back to its built-in id",
        );
    }

    #[test]
    fn resolved_key_id_follows_override() {
        let kb = bindings_from("copy-last-message: alt+c\n");
        let shared = new_shared_keybindings(kb);
        assert_eq!(
            resolved_key_id(&shared, Action::CopyLastMessage, "ctrl+x"),
            "alt+c"
        );
    }

    #[test]
    fn key_chord_bridge_stays_canonical() {
        // Guard the bridge from within the driver crate too: the app-layer chord
        // and the id the driver matches must agree.
        use crate::core::keybindings::{KeyChord, KeyModifiers};
        assert_eq!(
            KeyChord::with_mods(Key::Char('g'), KeyModifiers::CTRL).to_key_id(),
            "ctrl+g"
        );
        let _ = KeyBindingsFile::default();
    }
}
