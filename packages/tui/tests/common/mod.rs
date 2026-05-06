//! Shared helpers for integration tests.
//!
//! These exercise the public `hand_tui` API surface from a consumer's POV.
//! Keep helpers minimal — add what the next test actually needs.

#![allow(dead_code)]

use hand_tui::{Component, TestTerminal, utils};

/// Build an in-memory terminal of the given size.
pub fn capture_terminal(cols: u16, rows: u16) -> TestTerminal {
    TestTerminal::new(cols, rows)
}

/// Render a component once at `width` columns and return its lines.
pub fn render_once(component: &dyn Component, width: u16) -> Vec<String> {
    component.render(width)
}

/// Compare lines after stripping ANSI escapes.
pub fn assert_visible_lines_eq(actual: &[String], expected: &[&str]) {
    let stripped: Vec<String> = actual.iter().map(|s| utils::strip_ansi(s)).collect();
    assert_eq!(
        stripped.len(),
        expected.len(),
        "line count mismatch: got {:?}, want {:?}",
        stripped,
        expected
    );
    for (i, (got, want)) in stripped.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "line {} mismatch", i);
    }
}

/// Assert the visible width of an ANSI string matches an expected value.
pub fn assert_visible_width(s: &str, expected: usize) {
    assert_eq!(utils::visible_width(s), expected, "visible_width({:?})", s);
}
