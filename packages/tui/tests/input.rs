//! Integration tests for `InputComponent`.

mod common;

use hand_tui::{Component, Focusable, InputComponent, InputEvent, utils, visible_width};

#[test]
fn new_input_is_empty() {
    let input = InputComponent::new();
    assert_eq!(input.text(), "");
}

#[test]
fn set_text_and_clear_round_trip() {
    let mut input = InputComponent::new();
    input.set_text("hello");
    assert_eq!(input.text(), "hello");
    input.clear();
    assert_eq!(input.text(), "");
}

#[test]
fn placeholder_renders_when_empty() {
    let input = InputComponent::new().with_placeholder("type here");
    let lines = input.render(40);
    assert_eq!(lines.len(), 1);
    let stripped = utils::strip_ansi(&lines[0]);
    assert!(stripped.contains("type here"));
}

#[test]
fn placeholder_hidden_once_text_set() {
    let mut input = InputComponent::new().with_placeholder("type here");
    input.set_text("data");
    let lines = input.render(40);
    let stripped = utils::strip_ansi(&lines[0]);
    assert!(stripped.contains("data"));
    assert!(!stripped.contains("type here"));
}

#[test]
fn prefix_renders_before_text() {
    let mut input = InputComponent::new().with_prefix("> ");
    input.set_text("cmd");
    let lines = input.render(40);
    let stripped = utils::strip_ansi(&lines[0]);
    assert!(stripped.starts_with("> "));
    assert!(stripped.contains("cmd"));
}

#[test]
fn render_truncates_long_text_to_width() {
    let mut input = InputComponent::new();
    input.set_text(&"x".repeat(100));
    let lines = input.render(20);
    assert!(visible_width(&lines[0]) <= 20);
}

#[test]
fn unfocused_input_ignores_events() {
    let mut input = InputComponent::new();
    input.set_focused(false);
    let result = input.handle_input(&InputEvent::Raw("a".into()));
    assert!(matches!(result, hand_tui::HandleResult::Ignored));
}

#[test]
fn focused_input_consumes_typed_text() {
    let mut input = InputComponent::new();
    // new() defaults to focused = true.
    input.handle_input(&InputEvent::Raw("h".into()));
    input.handle_input(&InputEvent::Raw("i".into()));
    assert_eq!(input.text(), "hi");
}
