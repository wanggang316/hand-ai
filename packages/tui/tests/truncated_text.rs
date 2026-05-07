//! Integration tests for `TruncatedTextComponent`.

mod common;

use hand_tui::{Component, TruncatedTextComponent, utils, visible_width};

#[test]
fn short_text_pads_to_width() {
    let comp = TruncatedTextComponent::new("hi");
    let lines = comp.render(40);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("hi"));
    assert_eq!(visible_width(&lines[0]), 40);
}

#[test]
fn long_text_truncated_within_width() {
    let comp = TruncatedTextComponent::new("a very long text that needs cutting");
    let lines = comp.render(10);
    let stripped = utils::strip_ansi(&lines[0]);
    assert!(visible_width(&stripped) <= 10);
}

#[test]
fn set_text_updates_render_output() {
    let mut comp = TruncatedTextComponent::new("before");
    comp.set_text("after");
    assert_eq!(comp.text(), "after");
    let lines = comp.render(20);
    assert!(lines[0].starts_with("after"));
}

#[test]
fn first_line_only_when_input_has_newline() {
    let comp = TruncatedTextComponent::new("first\nsecond");
    let lines = comp.render(20);
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].contains("second"));
}

#[test]
fn padding_adds_blank_rows_and_left_indent() {
    let comp = TruncatedTextComponent::new("hi").with_padding(2, 1);
    let lines = comp.render(20);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].chars().all(|c| c == ' '));
    assert!(lines[1].starts_with("  hi"));
    assert!(lines[2].chars().all(|c| c == ' '));
}
