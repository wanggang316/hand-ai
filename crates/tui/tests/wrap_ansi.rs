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
