//! Integration tests for `EditorComponent`.

mod common;

use hand_tui::{Component, EditorComponent, InputEvent, utils};

#[test]
fn new_editor_has_one_empty_line() {
    let ed = EditorComponent::new();
    assert_eq!(ed.line_count(), 1);
    assert_eq!(ed.text(), "");
    assert_eq!(ed.cursor(), (0, 0));
}

#[test]
fn set_text_resets_buffer_and_cursor() {
    let mut ed = EditorComponent::new();
    ed.set_text("hello\nworld");
    assert_eq!(ed.text(), "hello\nworld");
    assert_eq!(ed.line_count(), 2);
    assert_eq!(ed.cursor(), (0, 0));
}

#[test]
fn paste_inserts_text_and_creates_marker() {
    let mut ed = EditorComponent::new();
    let pasted = "line1\nline2\nline3";
    ed.paste(pasted);
    // Long pastes are summarized via paste markers; submit_text expands them
    // back to the original payload.
    assert_eq!(ed.submit_text(), pasted);
}

#[test]
fn typed_input_appears_in_buffer() {
    let mut ed = EditorComponent::new();
    ed.handle_input(&InputEvent::Raw("h".into()));
    ed.handle_input(&InputEvent::Raw("i".into()));
    assert_eq!(ed.text(), "hi");
}

#[test]
fn render_includes_buffer_text_when_focused() {
    let mut ed = EditorComponent::new().with_border(false);
    ed.set_text("abc");
    let lines = ed.render(20);
    let joined = lines
        .iter()
        .map(|l| utils::strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("abc"));
}

#[test]
fn viewport_height_is_clamped_to_at_least_one() {
    let mut ed = EditorComponent::new();
    ed.set_viewport_height(0);
    // Subsequent rendering must not panic; ensure_cursor_visible runs.
    let _ = ed.render(40);
    assert_eq!(ed.cursor(), (0, 0));
}

#[test]
fn undo_after_set_text_is_noop_because_history_cleared() {
    let mut ed = EditorComponent::new();
    ed.set_text("baseline");
    // set_text clears history, so undo should leave the buffer unchanged.
    ed.undo();
    assert_eq!(ed.text(), "baseline");
}
