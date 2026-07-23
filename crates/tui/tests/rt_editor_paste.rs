//! Integration tests for the rt editor's paste pipeline
//! (`hand_tui::rt::components::Editor`).
//!
//! These pin the *behaviour* the plan's validation contract probes from
//! outside, driven end to end over the public API:
//! - VAL-EDITOR-009 bracketed paste lands verbatim — multi-line as multiple
//!   lines at the caret, no embedded newline triggers a submit, no per-char
//!   shortcut mis-fires.
//! - VAL-EDITOR-010 the two fold-marker forms (`+M lines` / `M chars`), the
//!   payload stored out-of-band, and the marker expanded to the full original on
//!   submit.
//! - VAL-EDITOR-011 marker atomic delete + dense renumber + single-undo restore
//!   (backspace-over-`]`), and the forward-delete downgrade to literal (asymmetric
//!   by Decision Log pin).
//! - VAL-EDITOR-012 dropped-file-path → `@mention` (quoted / `file://`,
//!   injected cwd + existence predicate), atomic undo of the mention, and a
//!   non-existent path inserted verbatim.
//! - VAL-EDITOR-023 escape / CSI bytes defused — the payload lands inert.
//! - VAL-EDITOR-024 recall completeness — a submitted message with a fold marker
//!   recalls the full expanded payload with no orphan marker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{Editor, PasteContent, dropped_file_mention_transform};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{HandleOutcome, RtComponent};

// --- helpers ----------------------------------------------------------------

/// A named-key `RtKey` with the given crossterm code and modifiers.
fn key(id: &str, code: KeyCode, mods: KeyModifiers) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: KeyEvent::new(code, mods),
    }
}

/// A bare printable-character `RtKey`.
fn ch(c: char) -> RtKey {
    key(&c.to_string(), KeyCode::Char(c), KeyModifiers::NONE)
}

fn backspace() -> RtKey {
    key("backspace", KeyCode::Backspace, KeyModifiers::NONE)
}

fn delete() -> RtKey {
    key("delete", KeyCode::Delete, KeyModifiers::NONE)
}

fn enter() -> RtKey {
    key("enter", KeyCode::Enter, KeyModifiers::NONE)
}

fn up() -> RtKey {
    key("up", KeyCode::Up, KeyModifiers::NONE)
}

fn down() -> RtKey {
    key("down", KeyCode::Down, KeyModifiers::NONE)
}

/// A payload with `n` short lines joined by `\n`.
fn many_lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// === VAL-EDITOR-009 — bracketed paste lands verbatim ========================

#[test]
fn multiline_paste_lands_as_multiple_lines_no_submit() {
    let mut ed = Editor::new();
    ed.insert_paste("one\ntwo\nthree");
    assert_eq!(ed.text(), "one\ntwo\nthree", "multi-line paste lands whole");
    assert_eq!(ed.line_count(), 3, "each embedded newline is a real line");
    assert!(
        ed.take_submit().is_none(),
        "an embedded newline never triggers a submit"
    );
    // The caret sits at the end of the pasted run, ready to keep typing.
    assert_eq!(ed.cursor(), (2, "three".len()));
}

#[test]
fn paste_is_one_atomic_undo_unit() {
    let mut ed = Editor::new();
    for c in "hi ".chars() {
        assert_eq!(ed.handle_key(&ch(c)), HandleOutcome::Consumed);
    }
    ed.insert_paste("A\nB\nC");
    for c in " bye".chars() {
        ed.handle_key(&ch(c));
    }
    assert_eq!(ed.text(), "hi A\nB\nC bye");
    // Undo peels the trailing typing burst, then the paste in one step.
    ed.undo();
    assert_eq!(ed.text(), "hi A\nB\nC");
    ed.undo();
    assert_eq!(ed.text(), "hi ", "the whole paste undoes atomically");
}

// === VAL-EDITOR-010 — two fold-marker forms + expansion =====================

#[test]
fn over_line_threshold_folds_to_lines_marker() {
    let mut ed = Editor::new();
    let payload = many_lines(40); // > 10 lines
    ed.insert_paste(&payload);
    assert_eq!(
        ed.text(),
        "[paste #1 +40 lines]",
        "over the line threshold folds to a +M lines marker"
    );
    // The payload is stored out-of-band, not in the visible buffer.
    let content = ed.paste_markers().get(&1).expect("marker #1 stored");
    assert_eq!(
        content,
        &PasteContent {
            id: 1,
            text: payload.clone(),
            line_count: 40,
            char_count: payload.chars().count(),
        }
    );
    // Expanded text substitutes the full original back.
    assert_eq!(
        ed.expanded_text(),
        payload,
        "expansion restores the payload"
    );
}

#[test]
fn over_char_threshold_single_line_folds_to_chars_marker() {
    let mut ed = Editor::new();
    let payload = "x".repeat(1500); // single line, > 1000 chars
    ed.insert_paste(&payload);
    assert_eq!(
        ed.text(),
        "[paste #1 1500 chars]",
        "a long single line folds to a M chars marker"
    );
    assert_eq!(ed.expanded_text(), payload);
}

#[test]
fn submit_expands_marker_to_full_payload() {
    let mut ed = Editor::new();
    for c in "here: ".chars() {
        ed.handle_key(&ch(c));
    }
    let payload = many_lines(20);
    ed.insert_paste(&payload);
    assert_eq!(
        ed.text(),
        "here: [paste #1 +20 lines]",
        "marker sits inline"
    );
    ed.handle_key(&enter());
    let submitted = ed.take_submit().expect("submitted");
    assert_eq!(
        submitted,
        format!("here: {payload}"),
        "submit expands the marker to the full payload"
    );
    // The buffer is cleared and the registry reset for the next message.
    assert_eq!(ed.text(), "");
    assert!(ed.paste_markers().is_empty(), "registry reset after submit");
}

// === VAL-EDITOR-011 — atomic delete / dense renumber / undo =================

#[test]
fn backspace_over_close_bracket_deletes_whole_marker() {
    let mut ed = Editor::new();
    ed.insert_paste(&many_lines(30));
    assert_eq!(ed.text(), "[paste #1 +30 lines]");
    // Caret parked at the closing bracket; one backspace removes the whole token.
    ed.handle_key(&backspace());
    assert_eq!(ed.text(), "", "the entire marker token vanished atomically");
    assert!(
        ed.paste_markers().is_empty(),
        "payload dropped with the token"
    );
}

#[test]
fn marker_delete_renumbers_survivors_densely() {
    let mut ed = Editor::new();
    // Three folds → #1 #2 #3, each separated by a space.
    ed.insert_paste(&many_lines(11));
    ed.handle_key(&ch(' '));
    ed.insert_paste(&many_lines(12));
    ed.handle_key(&ch(' '));
    ed.insert_paste(&many_lines(13));
    assert_eq!(
        ed.text(),
        "[paste #1 +11 lines] [paste #2 +12 lines] [paste #3 +13 lines]"
    );
    // Delete #2 (move caret to its closing bracket, then backspace).
    let close2 = ed.text().find("+12 lines]").unwrap() + "+12 lines]".len();
    // Position the caret on line 0 at the byte column of #2's ']'.
    // (single logical line here, so byte col == char index into the string)
    set_cursor_col(&mut ed, close2);
    ed.handle_key(&backspace());
    assert_eq!(
        ed.text(),
        "[paste #1 +11 lines]  [paste #2 +13 lines]",
        "the old #3 renumbered densely to #2"
    );
    // The registry is dense too: #1 and #2 both resolve, #3 is gone.
    assert!(ed.paste_markers().contains_key(&1));
    assert!(ed.paste_markers().contains_key(&2));
    assert!(!ed.paste_markers().contains_key(&3));
    assert_eq!(
        ed.paste_markers().get(&2).unwrap().line_count,
        13,
        "the renumbered marker still points at the third payload"
    );
}

#[test]
fn single_undo_restores_deleted_marker_and_payload() {
    let mut ed = Editor::new();
    let payload = many_lines(25);
    ed.insert_paste(&payload);
    ed.handle_key(&backspace()); // atomic delete
    assert_eq!(ed.text(), "");
    ed.undo();
    assert_eq!(
        ed.text(),
        "[paste #1 +25 lines]",
        "one undo brought the token back"
    );
    assert_eq!(
        ed.expanded_text(),
        payload,
        "and the hidden payload restored with it"
    );
}

#[test]
fn forward_delete_downgrades_marker_to_literal() {
    let mut ed = Editor::new();
    let payload = many_lines(15);
    ed.insert_paste(&payload);
    // Caret to the open bracket, then forward-Delete downgrades the token.
    set_cursor_col(&mut ed, 0);
    ed.handle_key(&delete());
    // The `[` char is removed by the forward-delete; the rest stays literal.
    assert_eq!(
        ed.text(),
        "paste #1 +15 lines]",
        "the token is now literal text, not an atomic token"
    );
    assert!(
        ed.paste_markers().is_empty(),
        "the payload no longer expands — downgraded to literal"
    );
    // Expansion is now a no-op passthrough (no live marker to substitute).
    assert_eq!(ed.expanded_text(), ed.text());
}

// === VAL-EDITOR-023 — escape / CSI bytes defused ============================

#[test]
fn pasted_escape_sequence_is_defused() {
    let mut ed = Editor::new();
    // A payload carrying a raw CSI colour sequence and a bell.
    ed.insert_paste("safe\x1b[31mRED\x07 tail");
    let text = ed.text();
    assert!(!text.contains('\x1b'), "no ESC survives into the buffer");
    assert!(!text.contains('\x07'), "no BEL survives into the buffer");
    assert_eq!(
        text, "safe[31mRED tail",
        "the sequence lands as inert visible text"
    );
    // The caret is at the end of the defused run, on line 0 (no cursor jump).
    assert_eq!(ed.cursor().0, 0);
}

// === VAL-EDITOR-012 — dropped path → @mention ===============================

/// Build a transform whose cwd is `/work` and whose only existing path is
/// `/work/src/lib.rs` (plus an absolute `/outside/x.txt`).
fn mention_transform() -> hand_tui::rt::components::PasteTransform {
    let exists: Arc<dyn Fn(&Path) -> bool + Send + Sync> =
        Arc::new(|p: &Path| p == Path::new("/work/src/lib.rs") || p == Path::new("/outside/x.txt"));
    dropped_file_mention_transform(PathBuf::from("/work"), exists)
}

#[test]
fn existing_dropped_path_becomes_mention() {
    let mut ed = Editor::new().with_paste_transform(mention_transform());
    // A quoted relative path inside cwd.
    ed.insert_paste("'src/lib.rs'");
    assert_eq!(
        ed.text(),
        "@src/lib.rs",
        "the dropped path rewrote to an @mention; the raw form is gone"
    );
    assert!(
        !ed.text().contains('\''),
        "the original quoted form is not visible"
    );
}

#[test]
fn file_url_dropped_path_becomes_relative_mention() {
    let mut ed = Editor::new().with_paste_transform(mention_transform());
    ed.insert_paste("file:///work/src/lib.rs");
    assert_eq!(
        ed.text(),
        "@src/lib.rs",
        "file:// absolute inside cwd → relative"
    );
}

#[test]
fn dropped_mention_undoes_atomically() {
    let mut ed = Editor::new().with_paste_transform(mention_transform());
    for c in "see ".chars() {
        ed.handle_key(&ch(c));
    }
    ed.insert_paste("'src/lib.rs'");
    assert_eq!(ed.text(), "see @src/lib.rs");
    ed.undo();
    assert_eq!(ed.text(), "see ", "one undo removed the entire mention");
}

#[test]
fn nonexistent_dropped_path_inserts_verbatim() {
    let mut ed = Editor::new().with_paste_transform(mention_transform());
    ed.insert_paste("src/missing.rs");
    assert_eq!(
        ed.text(),
        "src/missing.rs",
        "a path not on disk is inserted verbatim, not rewritten"
    );
    assert!(!ed.text().starts_with('@'), "no mention was produced");
}

// === VAL-EDITOR-024 — recall completeness ===================================

#[test]
fn recall_returns_full_expanded_payload_no_orphan_marker() {
    let mut ed = Editor::new();
    for c in "context: ".chars() {
        ed.handle_key(&ch(c));
    }
    let payload = many_lines(50);
    ed.insert_paste(&payload);
    ed.handle_key(&enter()); // submit
    let submitted = ed.take_submit().expect("submitted");
    assert_eq!(submitted, format!("context: {payload}"));
    // Up recalls the *expanded* text, with no `[paste #…]` marker left over.
    ed.handle_key(&up());
    let recalled = ed.text();
    assert!(
        !recalled.contains("[paste #"),
        "recall must not surface an orphan marker"
    );
    assert_eq!(
        recalled,
        format!("context: {payload}"),
        "recall is the full payload"
    );
    // Down past the newest restores an empty buffer.
    ed.handle_key(&down());
    assert_eq!(ed.text(), "", "Down past newest restores empty buffer");
}

// --- test-only cursor helper -------------------------------------------------

/// Drive the caret to byte column `col` on the current line by walking Right
/// from the line start — exercising the public key path rather than reaching
/// into private fields (which tests outside the crate cannot touch).
fn set_cursor_col(ed: &mut Editor, col: usize) {
    // Home to the line start, then step Right by graphemes until the byte column
    // matches. The buffer here is single-line ASCII, so Right advances one byte
    // per press; the loop is robust to that.
    ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    let mut guard = 0;
    while ed.cursor().1 < col && guard < 10_000 {
        ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
        guard += 1;
    }
}
