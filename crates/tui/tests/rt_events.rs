//! Unit tests for the rt input pipeline (`hand_tui::rt::events`).
//!
//! Covers, per the feature's testable assertions:
//! - `should_dispatch`: release/repeat filtering (VAL-CORE-014)
//! - `key_event_to_key_id`: canonical KeyId strings for a representative key set
//!   (VAL-CORE-031)
//! - Esc vs alt-chord disambiguation (VAL-CORE-015, VAL-CORE-030)
//! - Paste delivered as a single event (VAL-CORE-039)
//!
//! Each KeyId case pins the canonical string a structured crossterm event must
//! map to, in the canonical modifier order `shift, ctrl, alt, super`.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode,
};
use hand_tui::rt::events::{
    RtInputEvent, RtKey, key_event_to_key_id, should_dispatch, translate_event,
};

// --- helpers ---------------------------------------------------------------

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn key_kind(code: KeyCode, mods: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind,
        state: KeyEventState::NONE,
    }
}

// =============================================================================
// should_dispatch — release/repeat filtering (VAL-CORE-014)
// =============================================================================

#[test]
fn should_dispatch_press_only() {
    assert!(should_dispatch(KeyEventKind::Press));
    assert!(!should_dispatch(KeyEventKind::Release));
    assert!(!should_dispatch(KeyEventKind::Repeat));
}

#[test]
fn translate_filters_release_and_repeat_to_none() {
    // One physical press under enhanced reporting = Press + Release (+ Repeat).
    // Only the Press becomes a dispatched action.
    let press = translate_event(Event::Key(key_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Press,
    )));
    let release = translate_event(Event::Key(key_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )));
    let repeat = translate_event(Event::Key(key_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )));

    assert!(matches!(press, Some(RtInputEvent::Key(_))));
    assert_eq!(release, None, "release must be filtered");
    assert_eq!(repeat, None, "repeat must be filtered");
}

// =============================================================================
// key_event_to_key_id — canonical KeyId strings (VAL-CORE-031)
// =============================================================================

/// Assert our structured mapping produces the given canonical KeyId string.
fn assert_keyid(event: KeyEvent, expected: &str) {
    let ours = key_event_to_key_id(&event);
    assert_eq!(
        ours.as_deref(),
        Some(expected),
        "key_event_to_key_id({event:?}) = {ours:?}, expected {expected:?}",
    );
}

#[test]
fn keyid_canonical_for_representative_keys() {
    // Each pair is (structured crossterm event, the canonical KeyId string for
    // the same logical key). At least twelve representative keys, pinned in the
    // canonical modifier order `shift, ctrl, alt, super`.

    // ctrl+c.
    assert_keyid(key(KeyCode::Char('c'), KeyModifiers::CONTROL), "ctrl+c");

    // shift+ctrl+p.
    assert_keyid(
        key(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        "shift+ctrl+p",
    );

    // escape.
    assert_keyid(key(KeyCode::Esc, KeyModifiers::NONE), "escape");

    // enter.
    assert_keyid(key(KeyCode::Enter, KeyModifiers::NONE), "enter");

    // tab.
    assert_keyid(key(KeyCode::Tab, KeyModifiers::NONE), "tab");

    // alt+enter.
    assert_keyid(key(KeyCode::Enter, KeyModifiers::ALT), "alt+enter");

    // shift+enter.
    assert_keyid(key(KeyCode::Enter, KeyModifiers::SHIFT), "shift+enter");

    // arrow keys.
    assert_keyid(key(KeyCode::Up, KeyModifiers::NONE), "up");
    assert_keyid(key(KeyCode::Down, KeyModifiers::NONE), "down");
    assert_keyid(key(KeyCode::Left, KeyModifiers::NONE), "left");
    assert_keyid(key(KeyCode::Right, KeyModifiers::NONE), "right");

    // function keys.
    assert_keyid(key(KeyCode::F(1), KeyModifiers::NONE), "f1");
    assert_keyid(key(KeyCode::F(12), KeyModifiers::NONE), "f12");

    // space.
    assert_keyid(key(KeyCode::Char(' '), KeyModifiers::NONE), "space");

    // plain lowercase letter.
    assert_keyid(key(KeyCode::Char('a'), KeyModifiers::NONE), "a");

    // alt+letter.
    assert_keyid(key(KeyCode::Char('a'), KeyModifiers::ALT), "alt+a");
}

#[test]
fn keyid_canonical_modifier_order_is_shift_ctrl_alt() {
    // The legacy canonical order is shift, ctrl, alt, super. Pin it directly so
    // a regression in ordering is caught even if the legacy diff drifts.
    let ev = key(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(key_event_to_key_id(&ev).as_deref(), Some("shift+ctrl+p"));

    let ev = key(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(key_event_to_key_id(&ev).as_deref(), Some("ctrl+alt+a"));
}

#[test]
fn keyid_super_hyper_meta_collapse_to_super() {
    for m in [KeyModifiers::SUPER, KeyModifiers::HYPER, KeyModifiers::META] {
        let ev = key(KeyCode::Char('k'), m);
        assert_eq!(
            key_event_to_key_id(&ev).as_deref(),
            Some("super+k"),
            "modifier {m:?} should map to super+"
        );
    }
}

#[test]
fn keyid_backtab_is_shift_tab_equivalent() {
    // crossterm reports Shift+Tab as `KeyCode::BackTab` carrying
    // `KeyModifiers::SHIFT` (see crossterm's `\x1b[Z` parse). Both the BackTab
    // form and a plain Tab+SHIFT canonicalize to "shift+tab", matching legacy
    // \x1b[Z.
    let backtab = key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(key_event_to_key_id(&backtab).as_deref(), Some("shift+tab"));

    assert_keyid(key(KeyCode::Tab, KeyModifiers::SHIFT), "shift+tab");
}

#[test]
fn keyid_none_for_uncanonicalizable_keys() {
    assert_eq!(
        key_event_to_key_id(&key(KeyCode::Null, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        key_event_to_key_id(&key(KeyCode::CapsLock, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        key_event_to_key_id(&key(KeyCode::Media(MediaKeyCode::Play), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        key_event_to_key_id(&key(
            KeyCode::Modifier(ModifierKeyCode::LeftControl),
            KeyModifiers::NONE
        )),
        None
    );
}

// =============================================================================
// Esc vs alt-chord disambiguation (VAL-CORE-015, VAL-CORE-030)
// =============================================================================

#[test]
fn lone_esc_is_a_single_escape_event() {
    // A lone Esc under crossterm is KeyCode::Esc with no modifiers. It maps to
    // a single "escape" dispatch and never carries a following key.
    let ev = translate_event(Event::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
    match ev {
        Some(RtInputEvent::Key(RtKey { key_id, .. })) => {
            assert_eq!(key_id.as_deref(), Some("escape"));
        }
        other => panic!("expected a single escape Key event, got {other:?}"),
    }
}

#[test]
fn alt_chord_is_single_event_never_split() {
    // crossterm delivers Alt as a modifier flag, so Alt+a is ONE event whose id
    // is "alt+a" — never an "escape" followed by "a".
    let ev = translate_event(Event::Key(key(KeyCode::Char('a'), KeyModifiers::ALT)));
    match ev {
        Some(RtInputEvent::Key(RtKey { key_id, raw })) => {
            assert_eq!(key_id.as_deref(), Some("alt+a"));
            assert_eq!(raw.code, KeyCode::Char('a'));
            assert!(raw.modifiers.contains(KeyModifiers::ALT));
            assert_ne!(
                key_id.as_deref(),
                Some("escape"),
                "alt-chord must not degrade to escape"
            );
        }
        other => panic!("expected a single alt+a Key event, got {other:?}"),
    }
}

// =============================================================================
// Paste as a single event (VAL-CORE-039)
// =============================================================================

#[test]
fn multiline_paste_is_a_single_event_with_full_payload() {
    let payload = "line one\nline two\nline three".to_string();
    let ev = translate_event(Event::Paste(payload.clone()));
    match ev {
        Some(RtInputEvent::Paste(got)) => assert_eq!(got, payload),
        other => panic!("expected a single Paste event, got {other:?}"),
    }
}

#[test]
fn paste_does_not_emit_key_events() {
    // A paste payload full of characters that would otherwise be key actions
    // must not produce any Key events — it is exactly one Paste.
    let ev = translate_event(Event::Paste("abc\tdef".to_string()));
    assert!(matches!(ev, Some(RtInputEvent::Paste(_))));
}

// =============================================================================
// Resize / focus mapping
// =============================================================================

#[test]
fn resize_maps_cols_then_rows() {
    let ev = translate_event(Event::Resize(120, 40));
    assert_eq!(
        ev,
        Some(RtInputEvent::Resize {
            cols: 120,
            rows: 40
        })
    );
}

#[test]
fn focus_events_map_through() {
    assert_eq!(
        translate_event(Event::FocusGained),
        Some(RtInputEvent::FocusGained)
    );
    assert_eq!(
        translate_event(Event::FocusLost),
        Some(RtInputEvent::FocusLost)
    );
}
