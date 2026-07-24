//! Verifies the keybindings test fixtures at `tests/fixtures/tui/keybindings/`
//! load through the real [`KeyBindings`] loader with the documented behaviour.
//!
//! No terminal, no subprocess: these load the actual fixture YAML files (the same
//! ones the tmux `scenario.sh` copies into an isolated `$HAND_HOME/.hand/`) and
//! assert the resolved table. They pin, in CI, that the fixtures stay in sync with
//! the loader — the interactive-driver behaviours the fixtures exercise
//! (VAL-COMPAT-001/002/003/006, VAL-OVERLAY-021) are themselves unit-tested inside
//! the driver crate.

use std::path::{Path, PathBuf};

use hand_coding_agent::modes::interactive::slash_commands::SlashCommandTable;
use hand_coding_agent::{Action, Diagnostic, Key, KeyBindings, KeyChord, KeyModifiers, Scope};

/// The keybindings fixtures directory at the workspace root.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("tests/fixtures/tui/keybindings")
}

fn fixture(name: &str) -> PathBuf {
    let p = fixtures_dir().join(name);
    assert!(p.exists(), "missing fixture: {}", p.display());
    p
}

/// Load a single fixture as the global layer (no project layer).
fn load_global(name: &str) -> KeyBindings {
    KeyBindings::load(Some(&fixture(name)), None).expect("fixture loads without a hard error")
}

// --- VAL-COMPAT-001: an override applies verbatim -----------------------------

#[test]
fn valid_copy_override_applies_verbatim() {
    let kb = load_global("valid-copy-alt-c.yaml");
    assert_eq!(
        kb.resolve(Action::CopyLastMessage),
        Some(&KeyChord::with_mods(Key::Char('c'), KeyModifiers::ALT)),
        "copy-last-message remapped to Alt+C",
    );
    // The driver dispatches by the canonical key_id, so that must match too.
    assert_eq!(
        kb.key_id_for(Action::CopyLastMessage).as_deref(),
        Some("alt+c")
    );
    assert!(kb.diagnostics().is_empty(), "{:?}", kb.diagnostics());
}

// --- VAL-OVERLAY-021: a custom nav key drives registry-backed selectors -------

#[test]
fn valid_selector_override_binds_custom_nav_keys() {
    let kb = load_global("valid-select-down-j.yaml");
    assert_eq!(kb.key_id_for(Action::SelectDown).as_deref(), Some("j"));
    assert_eq!(kb.key_id_for(Action::SelectUp).as_deref(), Some("k"));
    // Selector-scope binding does not disturb the input line's history keys.
    assert_eq!(
        kb.action_for_key_id(Scope::Input, "down"),
        Some(Action::HistoryNext)
    );
}

// --- VAL-COMPAT-002: project shadows global -----------------------------------

#[test]
fn project_layer_shadows_global_layer() {
    let kb = KeyBindings::load(
        Some(&fixture("global-submit-ctrl-s.yaml")),
        Some(&fixture("project-submit-alt-enter.yaml")),
    )
    .expect("both layers load");
    assert_eq!(
        kb.resolve(Action::Submit),
        Some(&KeyChord::with_mods(Key::Enter, KeyModifiers::ALT)),
        "project (Alt+Enter) wins over global (Ctrl+S)",
    );
}

// --- VAL-COMPAT-003: invalid entries diagnose but never crash -----------------

#[test]
fn unknown_action_fixture_diagnoses_and_keeps_running() {
    let kb = load_global("invalid-unknown-action.yaml");
    assert!(
        kb.diagnostics()
            .iter()
            .any(|d| matches!(d, Diagnostic::UnknownAction { name, .. } if name == "bogus-action")),
        "expected an unknown-action diagnostic: {:?}",
        kb.diagnostics(),
    );
    // The valid sibling override still applied.
    assert_eq!(
        kb.key_id_for(Action::CopyLastMessage).as_deref(),
        Some("alt+c")
    );
}

#[test]
fn bad_chord_fixture_diagnoses_and_keeps_default() {
    let kb = load_global("invalid-bad-chord.yaml");
    assert!(
        kb.diagnostics()
            .iter()
            .any(|d| matches!(d, Diagnostic::InvalidChord { action, .. } if action == "submit")),
        "expected an invalid-chord diagnostic: {:?}",
        kb.diagnostics(),
    );
    // Submit keeps its default (Enter) — the malformed override was skipped.
    assert_eq!(
        kb.resolve(Action::Submit),
        Some(&KeyChord::plain(Key::Enter))
    );
}

#[test]
fn conflict_fixture_disables_the_chord_for_both_actions() {
    let kb = load_global("invalid-conflict.yaml");
    assert!(
        kb.diagnostics()
            .iter()
            .any(|d| matches!(d, Diagnostic::Conflict { chord, .. } if chord == "ctrl+z")),
        "expected a conflict diagnostic: {:?}",
        kb.diagnostics(),
    );
    // The conflicting chord drives neither action.
    assert!(
        kb.action_for_key_id(Scope::Input, "ctrl+z").is_none(),
        "conflicting ctrl+z must be disabled for both",
    );
}

// --- VAL-COMPAT-006: /hotkeys reflects the loaded fixture, no dead entries ----

#[test]
fn hotkeys_listing_over_a_fixture_has_no_dead_entries() {
    let kb = load_global("invalid-conflict.yaml");
    let text = SlashCommandTable::hotkeys_text(&kb);
    // The disabled pair surfaces as (disabled), not a phantom key.
    assert!(
        text.matches("(disabled)").count() >= 2,
        "conflicting submit/cancel must show (disabled): {text}",
    );
    assert!(text.contains("Keyboard shortcuts:"), "{text}");
    // A remapped fixture surfaces its key verbatim.
    let remapped = load_global("valid-copy-alt-c.yaml");
    assert!(
        SlashCommandTable::hotkeys_text(&remapped).contains("Alt+C"),
        "the override must show in the listing",
    );
}
