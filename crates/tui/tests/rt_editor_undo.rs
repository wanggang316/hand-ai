//! Integration tests for the rt editor's kill-ring and coalescing undo/redo
//! (`hand_tui::rt::components::Editor`).
//!
//! These pin the *behaviour* the plan's validation contract probes from outside:
//! - VAL-EDITOR-013 kill-ring yank / yank-pop wrap-around, per-editor scope
//! - VAL-EDITOR-014 coalescing undo (pause / newline / paste / delete each start
//!   a new unit), undo-after-submit restore, calm boundary no-ops, and
//!   typing-after-undo discarding the redo branch; redo is pinned at the unit
//!   layer (the hand UI binds no redo key, so it is exercised through the API).
//!
//! Behaviour is driven end to end over the public API and over structured
//! `RtKey`s through `handle_key`, matching how the focus view dispatches input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{Editor, KillRing};
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

/// Type each char of `s` into the editor as a separate key press.
fn type_str(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        assert_eq!(
            ed.handle_key(&ch(c)),
            HandleOutcome::Consumed,
            "printable char must be consumed"
        );
    }
}

/// Ctrl-chord helper (e.g. `ctrl+w`).
fn ctrl(c: char) -> RtKey {
    key(
        &format!("ctrl+{c}"),
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    )
}

/// Alt-chord helper (e.g. `alt+y`).
fn alt(c: char) -> RtKey {
    key(&format!("alt+{c}"), KeyCode::Char(c), KeyModifiers::ALT)
}

/// Bare Enter.
fn enter() -> RtKey {
    key("enter", KeyCode::Enter, KeyModifiers::NONE)
}

// --- VAL-EDITOR-013: kill-ring yank + yank-pop wrap-around -------------------

#[test]
fn kill_word_yank_reinserts_at_caret() {
    let mut ed = Editor::new();
    ed.insert_str("quick brown fox");
    // Kill the trailing word (Ctrl-W kills the word before the caret).
    ed.handle_key(&ctrl('w'));
    assert_eq!(ed.text(), "quick brown ");
    // Yank puts it back at the caret.
    ed.handle_key(&ctrl('y'));
    assert_eq!(
        ed.text(),
        "quick brown fox",
        "yank restored the killed word"
    );
}

/// Move the caret to `col` on the current single line via Home + Right presses.
fn seek_col(ed: &mut Editor, col: usize) {
    ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    for _ in 0..col {
        ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
    }
}

#[test]
fn kill_to_line_start_and_end_land_on_the_ring() {
    // Ctrl-U kills back to line start.
    let mut ed = Editor::new();
    ed.insert_str("prefixsuffix");
    seek_col(&mut ed, "prefix".chars().count());
    ed.handle_key(&ctrl('u'));
    assert_eq!(ed.text(), "suffix", "killed back to line start");
    ed.handle_key(&ctrl('y'));
    assert_eq!(ed.text(), "prefixsuffix", "yank restored the head");

    // Ctrl-K kills to line end.
    let mut ed2 = Editor::new();
    ed2.insert_str("headtail");
    seek_col(&mut ed2, "head".chars().count());
    ed2.handle_key(&ctrl('k'));
    assert_eq!(ed2.text(), "head", "killed to line end");
    ed2.handle_key(&ctrl('y'));
    assert_eq!(ed2.text(), "headtail", "yank restored the tail");
}

#[test]
fn yank_pop_walks_older_kills_and_wraps() {
    let mut ed = Editor::new();
    // Three separate kills push [one, two, three] (three newest).
    for word in ["one", "two", "three"] {
        ed.insert_str(word);
        ed.handle_key(&ctrl('u')); // kill whole line to the ring
        assert_eq!(ed.text(), "");
    }
    // Yank newest.
    ed.handle_key(&ctrl('y'));
    assert_eq!(ed.text(), "three");
    // Yank-pop swaps in progressively older entries.
    ed.handle_key(&alt('y'));
    assert_eq!(ed.text(), "two");
    ed.handle_key(&alt('y'));
    assert_eq!(ed.text(), "one");
    // And wraps back to the newest.
    ed.handle_key(&alt('y'));
    assert_eq!(ed.text(), "three", "yank-pop wrapped around the ring");
}

#[test]
fn yank_pop_without_a_preceding_yank_is_inert() {
    let mut ed = Editor::new();
    ed.insert_str("word");
    ed.handle_key(&ctrl('u')); // kill to ring
    assert_eq!(ed.text(), "");
    // No preceding yank: yank-pop does nothing.
    ed.handle_key(&alt('y'));
    assert_eq!(ed.text(), "", "yank-pop before any yank is inert");
}

#[test]
fn kill_ring_is_per_editor_not_shared() {
    // Two editors keep independent rings — a kill in one is never yankable in the
    // other (the informed exclusion: no shared, cross-editor ring).
    let mut a = Editor::new();
    let mut b = Editor::new();
    a.insert_str("secret");
    a.handle_key(&ctrl('u')); // kill "secret" onto A's ring
    assert_eq!(a.text(), "");
    // B has an empty ring; a yank is a no-op.
    b.insert_str("visible");
    b.handle_key(&ctrl('y'));
    assert_eq!(b.text(), "visible", "B's ring is empty; yank did nothing");
    assert!(b.kill_ring().is_empty(), "B never saw A's kill");
}

// --- VAL-EDITOR-014: coalescing undo/redo -----------------------------------

#[test]
fn typing_burst_undoes_in_one_step() {
    let mut ed = Editor::new();
    type_str(&mut ed, "hello world");
    ed.undo();
    assert_eq!(ed.text(), "", "one undo peels the whole typing burst");
}

#[test]
fn pause_splits_the_typing_burst() {
    let mut ed = Editor::new();
    type_str(&mut ed, "foo");
    ed.pause();
    type_str(&mut ed, "bar");
    ed.undo();
    assert_eq!(ed.text(), "foo", "undo peeled only the post-pause burst");
    ed.undo();
    assert_eq!(ed.text(), "", "second undo peeled the pre-pause burst");
}

#[test]
fn newline_starts_a_new_undo_unit() {
    let mut ed = Editor::new();
    type_str(&mut ed, "line1");
    ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
    type_str(&mut ed, "line2");
    ed.undo();
    assert_eq!(ed.text(), "line1\n", "undo peeled the second-line burst");
    ed.undo();
    assert_eq!(ed.text(), "line1", "undo peeled the newline");
    ed.undo();
    assert_eq!(ed.text(), "", "undo peeled the first-line burst");
}

#[test]
fn paste_is_one_atomic_undo_unit() {
    let mut ed = Editor::new();
    type_str(&mut ed, "a");
    ed.insert_paste("BIGPASTE");
    type_str(&mut ed, "b");
    ed.undo();
    assert_eq!(ed.text(), "aBIGPASTE", "post-paste typing peeled first");
    ed.undo();
    assert_eq!(ed.text(), "a", "the whole paste undid in one step");
    ed.undo();
    assert_eq!(ed.text(), "", "the pre-paste typing peeled last");
}

#[test]
fn delete_starts_a_new_undo_unit() {
    let mut ed = Editor::new();
    type_str(&mut ed, "abc");
    ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ed.text(), "ab");
    // The delete is its own unit — undo restores just the deleted char.
    ed.undo();
    assert_eq!(ed.text(), "abc", "undo restored the deleted char");
    ed.undo();
    assert_eq!(ed.text(), "", "undo peeled the typing burst");
}

#[test]
fn undo_after_submit_restores_the_sent_text() {
    let mut ed = Editor::new();
    type_str(&mut ed, "dispatch this");
    ed.handle_key(&enter());
    assert_eq!(ed.take_submit().as_deref(), Some("dispatch this"));
    assert_eq!(ed.text(), "", "buffer cleared on submit");
    ed.undo();
    assert_eq!(
        ed.text(),
        "dispatch this",
        "undo restored the submitted text"
    );
}

#[test]
fn undo_and_redo_are_calm_noops_at_the_boundaries() {
    let mut ed = Editor::new();
    // Nothing to undo/redo: no panic, no change.
    ed.undo();
    ed.redo();
    assert_eq!(ed.text(), "");

    type_str(&mut ed, "xy");
    ed.undo();
    assert_eq!(ed.text(), "");
    // Past the bottom is a no-op.
    ed.undo();
    assert_eq!(ed.text(), "");
    // Redo replays, then past the top is a no-op.
    ed.redo();
    assert_eq!(ed.text(), "xy");
    ed.redo();
    assert_eq!(ed.text(), "xy", "redo past the top is inert");
}

#[test]
fn typing_after_undo_discards_the_redo_branch() {
    let mut ed = Editor::new();
    type_str(&mut ed, "aaa");
    ed.pause();
    type_str(&mut ed, "bbb");
    ed.undo(); // drop the "bbb" burst
    assert_eq!(ed.text(), "aaa");
    // A fresh edit discards the redo branch: the dropped burst is unreachable.
    type_str(&mut ed, "ccc");
    assert_eq!(ed.text(), "aaaccc");
    ed.redo();
    assert_eq!(ed.text(), "aaaccc", "redo branch was discarded");
}

#[test]
fn redo_is_pinned_at_the_unit_layer() {
    // The hand UI binds no redo key; redo semantics are exercised through the API
    // at unit granularity (an informed exclusion — no new keystroke is added).
    let mut ed = Editor::new();
    type_str(&mut ed, "aaa");
    ed.pause();
    type_str(&mut ed, "bbb");
    ed.undo();
    ed.undo();
    assert_eq!(ed.text(), "");
    ed.redo();
    assert_eq!(ed.text(), "aaa", "redo replays one unit");
    ed.redo();
    assert_eq!(ed.text(), "aaabbb", "redo replays the next unit");
}

// --- kill-ring pure logic, from the public type -----------------------------

#[test]
fn kill_ring_type_yank_pop_wraps() {
    let mut ring = KillRing::new(4);
    ring.push("a".to_string());
    ring.push("b".to_string());
    assert_eq!(ring.yank(), Some("b"));
    assert_eq!(ring.yank_pop(), Some("a"));
    assert_eq!(ring.yank_pop(), Some("b"), "wrap-around");
    ring.reset();
    assert!(ring.yank_pop().is_none(), "reset disarms yank-pop");
}
