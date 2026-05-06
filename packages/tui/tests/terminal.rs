//! Integration tests for the in-memory `TestTerminal`.

mod common;

use hand_tui::{Terminal, TestTerminal};

#[test]
fn write_records_output() {
    let mut term = TestTerminal::new(80, 24);
    term.write("hello");
    term.write(" world");
    assert_eq!(term.last_output(), Some(" world"));
    assert_eq!(term.output.len(), 2);
}

#[test]
fn columns_and_rows_match_constructor() {
    let term = TestTerminal::new(40, 12);
    assert_eq!(term.columns(), 40);
    assert_eq!(term.rows(), 12);
}

#[test]
fn cursor_visibility_writes_escapes() {
    let mut term = TestTerminal::new(80, 24);
    term.hide_cursor();
    term.show_cursor();
    assert_eq!(term.output, vec!["\x1b[?25l", "\x1b[?25h"]);
}

#[test]
fn clear_helpers_emit_csi_sequences() {
    let mut term = TestTerminal::new(80, 24);
    term.clear_line();
    term.clear_from_cursor();
    term.clear_screen();
    assert_eq!(term.output, vec!["\x1b[2K\r", "\x1b[J", "\x1b[2J\x1b[H"]);
}

#[test]
fn move_by_uses_signed_directions() {
    let mut term = TestTerminal::new(80, 24);
    term.move_by(0); // no-op
    term.move_by(3);
    term.move_by(-2);
    assert_eq!(term.output, vec!["\x1b[3B", "\x1b[2A"]);
}

#[test]
fn set_size_updates_dimensions() {
    let mut term = TestTerminal::new(80, 24);
    term.set_size(120, 40);
    assert_eq!(term.columns(), 120);
    assert_eq!(term.rows(), 40);
}

#[test]
fn set_title_emits_osc_sequence() {
    let mut term = TestTerminal::new(80, 24);
    term.set_title("hello");
    assert_eq!(term.last_output(), Some("\x1b]0;hello\x07"));
}
