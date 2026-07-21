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
fn undo_after_set_text_restores_previous_buffer() {
    let mut ed = EditorComponent::new();
    ed.set_text("baseline");
    ed.set_text("replaced");
    // set_text records a whole-buffer undo entry, so undo walks back
    // through programmatic replacements.
    ed.undo();
    assert_eq!(ed.text(), "baseline");
}

/// Build a paste payload big enough to be stored behind a marker
/// (`lines` > 10 triggers the out-of-band path).
fn big_paste(lines: usize, tag: &str) -> String {
    (0..lines)
        .map(|i| format!("{tag}{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Press Backspace with the cursor `graphemes_left` positions before the
/// end of the buffer.
fn backspace_at_offset_from_end(ed: &mut EditorComponent, graphemes_left: usize) {
    for _ in 0..graphemes_left {
        ed.handle_input(&InputEvent::Raw("\x1b[D".into()));
    }
    ed.handle_input(&InputEvent::Raw("\x7f".into()));
}

#[test]
fn deleting_paste_marker_drops_registry_entry_and_renumbers() {
    let mut ed = EditorComponent::new();
    let first = big_paste(15, "a");
    let second = big_paste(12, "b");
    ed.paste(&first);
    ed.paste(&second);
    assert_eq!(ed.text(), "[paste #1 +15 lines][paste #2 +12 lines]");

    // Walk the cursor back over the second marker so Backspace lands on
    // the closing bracket of marker #1, then delete it.
    backspace_at_offset_from_end(&mut ed, "[paste #2 +12 lines]".len());

    assert_eq!(ed.text(), "[paste #1 +12 lines]");
    assert_eq!(ed.paste_markers().len(), 1);
    assert_eq!(ed.paste_markers()[&1].id, 1);
    assert_eq!(ed.submit_text(), second);
}

#[test]
fn undo_after_paste_marker_delete_restores_text_and_registry() {
    let mut ed = EditorComponent::new();
    let first = big_paste(15, "a");
    let second = big_paste(12, "b");
    ed.paste(&first);
    ed.paste(&second);
    backspace_at_offset_from_end(&mut ed, "[paste #2 +12 lines]".len());
    assert_eq!(ed.text(), "[paste #1 +12 lines]");

    ed.undo();
    assert_eq!(ed.text(), "[paste #1 +15 lines][paste #2 +12 lines]");
    assert_eq!(ed.paste_markers().len(), 2);
    assert_eq!(ed.submit_text(), format!("{first}{second}"));

    ed.redo();
    assert_eq!(ed.text(), "[paste #1 +12 lines]");
    assert_eq!(ed.submit_text(), second);
}

#[test]
fn undo_after_set_text_restores_paste_registry() {
    let mut ed = EditorComponent::new();
    let big = big_paste(15, "x");
    ed.paste(&big);
    ed.set_text("replaced");
    assert!(ed.paste_markers().is_empty());

    ed.undo();
    assert_eq!(ed.text(), "[paste #1 +15 lines]");
    assert_eq!(ed.submit_text(), big);
}

#[test]
fn paste_id_reuse_after_marker_delete_does_not_collide() {
    let mut ed = EditorComponent::new();
    let first = big_paste(15, "a");
    let second = big_paste(12, "b");
    let third = big_paste(11, "c");
    ed.paste(&first);
    ed.paste(&second);
    // Backspace at end-of-buffer removes marker #2 and frees its id.
    ed.handle_input(&InputEvent::Raw("\x7f".into()));
    assert_eq!(ed.text(), "[paste #1 +15 lines]");

    ed.paste(&third);
    assert_eq!(ed.text(), "[paste #1 +15 lines][paste #2 +11 lines]");
    assert_eq!(ed.submit_text(), format!("{first}{third}"));
}
