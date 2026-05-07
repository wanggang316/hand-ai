//! Integration tests for `keys` parsing helpers.

mod common;

use hand_tui::{
    KeyName, decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat,
    matches_key, parse_key, parse_key_id,
};

#[test]
fn parse_key_recognizes_named_keys() {
    assert_eq!(parse_key("\x1b[A").name, KeyName::Up);
    assert_eq!(parse_key("\x1b[B").name, KeyName::Down);
    assert_eq!(parse_key("\x1b[C").name, KeyName::Right);
    assert_eq!(parse_key("\x1b[D").name, KeyName::Left);
}

#[test]
fn parse_key_recognizes_char_keys() {
    assert_eq!(parse_key("a").name, KeyName::Char('a'));
}

#[test]
fn parse_key_recognizes_control_codes() {
    assert_eq!(parse_key("\r").name, KeyName::Enter);
    assert_eq!(parse_key("\t").name, KeyName::Tab);
    assert_eq!(parse_key("\x7f").name, KeyName::Backspace);
}

#[test]
fn matches_key_handles_canonical_ids() {
    assert!(matches_key("\x1b[A", "up"));
    assert!(matches_key("a", "a"));
    assert!(!matches_key("a", "b"));
}

#[test]
fn parse_key_id_returns_some_for_known_inputs() {
    assert!(parse_key_id("\x1b[A").is_some());
    assert!(parse_key_id("a").is_some());
}

#[test]
fn release_and_repeat_default_false_for_legacy_input() {
    // Legacy (non-Kitty) sequences carry no event-type bit, so press is implied.
    assert!(!is_key_release("a"));
    assert!(!is_key_repeat("a"));
}

#[test]
fn decode_printable_key_returns_none_for_plain_ascii() {
    // Plain bytes aren't kitty/modifyOtherKeys CSI; the helpers
    // intentionally reject them.
    assert_eq!(decode_printable_key("a"), None);
    assert_eq!(decode_kitty_printable("a"), None);
}

#[test]
fn decode_kitty_printable_handles_csi_u_codepoint() {
    // CSI-u format: `<codepoint>u`; here `97u` = 'a'.
    assert_eq!(decode_kitty_printable("\x1b[97u"), Some("a".to_string()));
}
