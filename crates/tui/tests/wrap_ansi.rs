//! Integration tests for ANSI-aware wrapping.

mod common;

use hand_tui::utils::wrap_text_with_ansi;
use hand_tui::{visible_width, wrap_text};

#[test]
fn no_wrap_when_input_shorter_than_width() {
    let lines = wrap_text("hello", 10);
    assert_eq!(lines, vec!["hello"]);
}

#[test]
fn wraps_on_word_boundary_when_possible() {
    let lines = wrap_text("hello world foo", 6);
    assert!(lines.len() > 1);
    for l in &lines {
        assert!(visible_width(l) <= 6, "line {:?} exceeds width", l);
    }
}

#[test]
fn preserves_explicit_newlines() {
    let lines = wrap_text("line1\nline2", 80);
    assert_eq!(lines, vec!["line1", "line2"]);
}

#[test]
fn crlf_wraps_identically_to_lf() {
    assert_eq!(
        wrap_text_with_ansi("first\r\nsecond\r\nthird", 80),
        wrap_text_with_ansi("first\nsecond\nthird", 80),
    );
}

#[test]
fn bare_cr_is_a_line_break() {
    let lines = wrap_text_with_ansi("first\rsecond", 80);
    assert_eq!(lines, vec!["first", "second"]);
}

#[test]
fn mixed_line_endings_all_break() {
    let lines = wrap_text_with_ansi("first\nsecond\r\nthird\rfourth", 80);
    assert_eq!(lines, vec!["first", "second", "third", "fourth"]);
}

#[test]
fn ansi_state_carries_across_crlf_and_cr() {
    let lines = wrap_text_with_ansi("\x1b[31mfirst\r\nsecond\rthird\x1b[0m", 80);
    assert_eq!(
        lines,
        vec!["\x1b[31mfirst", "\x1b[31msecond", "\x1b[31mthird\x1b[0m",]
    );
}

#[test]
fn no_stray_cr_in_wrapped_output() {
    let lines = wrap_text_with_ansi("hello world\r\nfoo bar baz qux\r\nend", 8);
    assert!(lines.len() > 3);
    for l in &lines {
        assert!(!l.contains('\r'), "line {:?} contains a stray CR", l);
    }
}

#[test]
fn legacy_wrap_text_honors_crlf_and_cr() {
    let lines = wrap_text("first\r\nsecond\rthird", 80);
    assert_eq!(lines, vec!["first", "second", "third"]);
}

#[test]
fn preserves_ansi_codes_across_wraps() {
    let s = format!("\x1b[31m{}\x1b[0m", "hello ".repeat(10));
    let lines = wrap_text_with_ansi(&s, 20);
    assert!(lines.len() > 1);
    for l in &lines {
        assert!(visible_width(l) <= 20, "line {:?} too wide", l);
    }
    // Color must persist in continuation lines.
    assert!(lines.iter().any(|l| l.contains("\x1b[31m")));
}

#[test]
fn empty_input_yields_single_empty_line() {
    let lines = wrap_text("", 10);
    assert!(lines.len() <= 1);
}

#[test]
fn very_long_word_still_fits_within_width() {
    let lines = wrap_text(&"a".repeat(50), 10);
    for l in &lines {
        assert!(visible_width(l) <= 10);
    }
}
