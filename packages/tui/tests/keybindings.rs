//! Integration tests for the keybindings manager.

mod common;

use std::collections::HashMap;

use hand_tui::{Keybinding, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS};

#[test]
fn defaults_resolve_to_known_keys() {
    let mgr = KeybindingsManager::new();
    let up = mgr.get(Keybinding::EditorCursorUp);
    assert!(up.iter().any(|k| k == "up"), "got {:?}", up);
    let submit = mgr.get(Keybinding::InputSubmit);
    assert!(submit.iter().any(|k| k == "enter"));
}

#[test]
fn id_round_trips_via_from_id() {
    for binding in [
        Keybinding::EditorCursorUp,
        Keybinding::InputSubmit,
        Keybinding::SelectCancel,
    ] {
        let id = binding.id();
        assert_eq!(Keybinding::from_id(id), Some(binding));
    }
    assert_eq!(Keybinding::from_id("nope.does.not.exist"), None);
}

#[test]
fn user_override_replaces_default() {
    let mut cfg: KeybindingsConfig = HashMap::new();
    cfg.insert(
        Keybinding::EditorCursorUp.id().to_string(),
        Some(vec!["ctrl+p".into()]),
    );
    let mgr = KeybindingsManager::with_config(&cfg);
    let keys = mgr.get(Keybinding::EditorCursorUp);
    assert_eq!(keys, &["ctrl+p".to_string()]);
}

#[test]
fn user_override_can_disable_a_binding() {
    let mut cfg: KeybindingsConfig = HashMap::new();
    cfg.insert(Keybinding::InputSubmit.id().to_string(), None);
    let mgr = KeybindingsManager::with_config(&cfg);
    assert!(mgr.get(Keybinding::InputSubmit).is_empty());
    assert!(!mgr.matches("\r", Keybinding::InputSubmit));
}

#[test]
fn conflicting_user_bindings_surface() {
    let mut cfg: KeybindingsConfig = HashMap::new();
    cfg.insert(
        Keybinding::EditorCursorUp.id().to_string(),
        Some(vec!["ctrl+p".into()]),
    );
    cfg.insert(
        Keybinding::SelectUp.id().to_string(),
        Some(vec!["ctrl+p".into()]),
    );
    let mgr = KeybindingsManager::with_config(&cfg);
    let conflicts = mgr.conflicts();
    assert!(conflicts.iter().any(|c| c.key_id == "ctrl+p"));
}

#[test]
fn matches_handles_named_keys() {
    let mgr = KeybindingsManager::new();
    assert!(mgr.matches("\x1b[A", Keybinding::EditorCursorUp));
    assert!(mgr.matches("\r", Keybinding::InputSubmit));
}

#[test]
fn default_table_covers_every_known_binding() {
    for (binding, _) in KeybindingsManager::new().all() {
        assert!(
            TUI_KEYBINDINGS.contains_key(binding.id()),
            "missing default for {:?}",
            binding
        );
    }
}
