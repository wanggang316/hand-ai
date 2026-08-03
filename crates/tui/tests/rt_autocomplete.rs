//! Integration tests for the rt editor's autocomplete popup and its two
//! providers (`hand_tui::rt::components`).
//!
//! These pin the *behaviour* the plan's validation contract probes from the
//! outside, driving the editor end to end over structured `RtKey`s through
//! `handle_key` — exactly how the focus view dispatches input:
//!
//! - VAL-EDITOR-005  slash popup lifecycle, per-line start
//! - VAL-EDITOR-006  path matching: basename prefix / extension / slash substring
//! - VAL-EDITOR-007  Enter submits verbatim (no accept), Tab is the only accept
//! - VAL-EDITOR-008  zero-match closes the popup, backspace-to-prefix reopens it
//! - VAL-EDITOR-021  popup navigation, 8-row window, Esc closes leaving buffer
//! - VAL-EDITOR-022  trigger negatives + intermediate-path-component exclusion
//! - VAL-EDITOR-025  Tab-accept is one undo unit (single undo cleanly reverts)
//! - VAL-EDITOR-026  a grown editor over a short pane keeps popup rows in bounds

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{
    Autocomplete, AutocompleteProvider, CombinedProvider, Editor, MAX_VISIBLE, PathEntry,
    PathProvider, SlashCommand, SlashProvider,
};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::RtComponent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::{TerminalOptions, Viewport};

// --- helpers ----------------------------------------------------------------

/// A named-key `RtKey`.
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

fn tab() -> RtKey {
    key("tab", KeyCode::Tab, KeyModifiers::NONE)
}
fn enter() -> RtKey {
    key("enter", KeyCode::Enter, KeyModifiers::NONE)
}
fn esc() -> RtKey {
    key("escape", KeyCode::Esc, KeyModifiers::NONE)
}
fn up() -> RtKey {
    key("up", KeyCode::Up, KeyModifiers::NONE)
}
fn down() -> RtKey {
    key("down", KeyCode::Down, KeyModifiers::NONE)
}
fn backspace() -> RtKey {
    key("backspace", KeyCode::Backspace, KeyModifiers::NONE)
}

/// Type each char of `s` into the editor as a separate key press (spaces routed
/// through the `space` id, matching the real dispatcher).
fn type_str(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        let k = if c == ' ' {
            key("space", KeyCode::Char(' '), KeyModifiers::NONE)
        } else {
            ch(c)
        };
        ed.handle_key(&k);
    }
}

/// A slash provider over the given command names.
fn slash_provider(names: &[&str]) -> Arc<dyn AutocompleteProvider> {
    Arc::new(SlashProvider::new(
        names.iter().map(|n| SlashCommand::new(*n)).collect(),
    ))
}

/// A path provider over the given file entries.
fn path_provider(files: &[&str]) -> Arc<dyn AutocompleteProvider> {
    Arc::new(PathProvider::new(
        files.iter().map(|p| PathEntry::file(*p)).collect(),
    ))
}

/// A combined router over a slash + path provider.
fn combined(cmds: &[&str], files: &[&str]) -> Arc<dyn AutocompleteProvider> {
    Arc::new(CombinedProvider::new(vec![
        Box::new(SlashProvider::new(
            cmds.iter().map(|n| SlashCommand::new(*n)).collect(),
        )),
        Box::new(PathProvider::new(
            files.iter().map(|p| PathEntry::file(*p)).collect(),
        )),
    ]))
}

/// The labels currently in the popup, in order.
fn popup_labels(ed: &Editor) -> Vec<String> {
    ed.autocomplete()
        .items()
        .iter()
        .map(|i| i.label.clone())
        .collect()
}

/// Render the editor into a fixed inline backend and return the painted rows.
fn render_rows(ed: &Editor, cols: u16, rows: u16, area: Rect) -> Vec<String> {
    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)),
        },
    )
    .unwrap();
    terminal
        .draw(|frame| {
            let buf = frame.buffer_mut();
            ed.render(area, buf);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..rows)
        .map(|y| {
            let mut s = String::new();
            for x in 0..cols {
                s.push_str(buffer[(x, y)].symbol());
            }
            s.trim_end().to_string()
        })
        .collect()
}

// --- VAL-EDITOR-005: slash popup lifecycle, per-line start ------------------

#[test]
fn slash_popup_opens_at_line_start_and_filters_by_prefix() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help", "history", "model", "quit"]));
    assert!(!ed.autocomplete_visible(), "closed before any trigger");

    type_str(&mut ed, "/h");
    assert!(ed.autocomplete_visible(), "slash at line start opens popup");
    assert_eq!(popup_labels(&ed), vec!["/help", "/history"]);
}

#[test]
fn slash_accept_keeps_leading_slash() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help", "history"]));
    type_str(&mut ed, "/hi");
    assert_eq!(popup_labels(&ed), vec!["/history"]);
    // Tab accepts, leaving the well-formed `/history` (slash preserved).
    ed.handle_key(&tab());
    assert_eq!(ed.text(), "/history");
    assert!(!ed.autocomplete_visible(), "popup closes on accept");
}

#[test]
fn slash_triggers_on_a_later_line_start_not_mid_line() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    // A `/` mid-line does not trigger.
    type_str(&mut ed, "see /h");
    assert!(
        !ed.autocomplete_visible(),
        "mid-line slash must not trigger"
    );
    // A newline, then `/` at the new line's start does trigger (per-line start).
    ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
    type_str(&mut ed, "/h");
    assert!(
        ed.autocomplete_visible(),
        "slash at a later line start triggers"
    );
    assert_eq!(popup_labels(&ed), vec!["/help"]);
}

// --- VAL-EDITOR-006: path matching semantics --------------------------------

#[test]
fn path_basename_prefix_match() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["README.md", "main.rs", "lib.rs"]));
    type_str(&mut ed, "@RE");
    assert_eq!(popup_labels(&ed), vec!["README.md"]);
}

#[test]
fn path_extension_match() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["main.rs", "lib.rs", "readme.md"]));
    type_str(&mut ed, "@.rs");
    let got = popup_labels(&ed);
    assert!(got.contains(&"main.rs".to_string()), "got: {got:?}");
    assert!(got.contains(&"lib.rs".to_string()), "got: {got:?}");
    assert!(!got.contains(&"readme.md".to_string()), "got: {got:?}");
}

#[test]
fn path_slash_query_is_relative_substring() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&[
        "src/main.rs",
        "src/inner/util.rs",
        "vendor/src/x.rs",
        "other.txt",
    ]));
    type_str(&mut ed, "@src/");
    let got = popup_labels(&ed);
    assert!(got.contains(&"src/main.rs".to_string()), "got: {got:?}");
    assert!(
        got.contains(&"vendor/src/x.rs".to_string()),
        "slash query is a substring, must include vendor/src: {got:?}"
    );
    assert!(!got.contains(&"other.txt".to_string()), "got: {got:?}");
}

#[test]
fn path_accept_inserts_at_path_and_quotes_spaces() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["my file.txt"]));
    type_str(&mut ed, "@my");
    ed.handle_key(&tab());
    assert_eq!(
        ed.text(),
        "@\"my file.txt\"",
        "spaced path quote-wrapped on accept"
    );
}

// --- combined routing -------------------------------------------------------

#[test]
fn combined_router_dispatches_by_trigger() {
    // Slash query → only the slash provider answers.
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(combined(&["alpha"], &["alpha.txt"]));
    type_str(&mut ed, "/al");
    assert_eq!(popup_labels(&ed), vec!["/alpha"], "slash routes to slash");

    // At query → only the path provider answers.
    let mut ed2 = Editor::new();
    ed2.set_autocomplete_provider(combined(&["alpha"], &["alpha.txt"]));
    type_str(&mut ed2, "@al");
    assert_eq!(popup_labels(&ed2), vec!["alpha.txt"], "at routes to path");
}

// --- VAL-EDITOR-007: Enter accepts the candidate while the popup is open -----

#[test]
fn enter_accepts_the_candidate_while_popup_is_open() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help", "history"]));
    type_str(&mut ed, "/h");
    assert!(ed.autocomplete_visible());
    // Standard completion UX: with the popup open, Enter accepts the highlighted
    // candidate (like Tab) and closes the popup — it does NOT submit the raw `/h`.
    ed.handle_key(&enter());
    assert_eq!(ed.text(), "/help", "Enter splices the selected candidate");
    assert!(!ed.autocomplete_visible(), "accept closes the popup");
    assert!(
        ed.take_submit().is_none(),
        "Enter accepts, it does not submit, while the popup is open"
    );
    // With the popup now closed, Enter submits as normal.
    ed.handle_key(&enter());
    assert_eq!(
        ed.take_submit().as_deref(),
        Some("/help"),
        "Enter submits once the popup is closed"
    );
}

#[test]
fn tab_accepts_the_candidate() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    type_str(&mut ed, "/h");
    // Tab accepts (the other accept gesture alongside Enter-while-open).
    ed.handle_key(&tab());
    assert_eq!(ed.text(), "/help");
    assert!(ed.take_submit().is_none(), "Tab does not submit");
}

// --- VAL-EDITOR-008: zero-match closes, backspace-to-prefix reopens ---------

#[test]
fn zero_match_closes_popup_and_backspace_reopens() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    type_str(&mut ed, "/he");
    assert!(ed.autocomplete_visible(), "prefix matches → open");
    // Type past the match: `/hex` matches nothing → popup vanishes (no empty box).
    type_str(&mut ed, "x");
    assert!(!ed.autocomplete_visible(), "zero match closes the popup");
    // Backspace back to the matching prefix reopens it.
    ed.handle_key(&backspace());
    assert!(
        ed.autocomplete_visible(),
        "backspace to matching prefix reopens"
    );
    assert_eq!(popup_labels(&ed), vec!["/help"]);
}

// --- VAL-EDITOR-021: navigation, 8-row window, Esc --------------------------

#[test]
fn up_down_move_indicator_not_buffer_caret_or_history() {
    let mut ed = Editor::new();
    ed.add_to_history("earlier prompt");
    ed.set_autocomplete_provider(slash_provider(&["aa", "ab", "ac"]));
    type_str(&mut ed, "/a");
    let caret_before = ed.cursor();
    assert_eq!(ed.autocomplete().selected_index(), 0);

    ed.handle_key(&down());
    assert_eq!(
        ed.autocomplete().selected_index(),
        1,
        "Down moves indicator"
    );
    ed.handle_key(&up());
    assert_eq!(ed.autocomplete().selected_index(), 0, "Up moves indicator");

    // The buffer caret never moved, and Up did not recall history.
    assert_eq!(ed.cursor(), caret_before, "buffer caret unchanged");
    assert_eq!(ed.text(), "/a", "Up did not recall history");
}

#[test]
fn popup_window_caps_at_eight_rows() {
    let names: Vec<String> = (0..20).map(|i| format!("cmd{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&refs));
    type_str(&mut ed, "/cmd");
    assert_eq!(ed.autocomplete().len(), 20, "all 20 match");
    assert_eq!(
        ed.autocomplete().visible_rows(),
        MAX_VISIBLE,
        "window caps at 8 rows"
    );

    // Rendered popup shows at most 8 candidate rows (below the 3-row box). Popup
    // rows carry the `▸ ` / `  ` indicator gutter; the box interior does not, so
    // filter to rows that start with the popup gutter to avoid counting the
    // box's own `│ /cmd …│` content line.
    let rows = render_rows(&ed, 40, 20, Rect::new(0, 0, 40, 20));
    let candidate_rows = rows
        .iter()
        .filter(|r| r.starts_with("▸ /cmd") || r.starts_with("  /cmd"))
        .count();
    assert_eq!(
        candidate_rows, MAX_VISIBLE,
        "exactly 8 candidate rows painted (window cap), got {candidate_rows}: {rows:?}"
    );
}

#[test]
fn esc_closes_popup_leaving_buffer_untouched() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    type_str(&mut ed, "/he");
    assert!(ed.autocomplete_visible());
    ed.handle_key(&esc());
    assert!(!ed.autocomplete_visible(), "Esc closes the popup");
    assert_eq!(ed.text(), "/he", "Esc leaves the buffer as typed");
}

// --- VAL-EDITOR-022: trigger negatives + intermediate-component exclusion ----

#[test]
fn mid_line_slash_does_not_trigger() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    type_str(&mut ed, "run /help");
    assert!(
        !ed.autocomplete_visible(),
        "a `/` in the middle of a line is not a command"
    );
}

#[test]
fn embedded_at_in_email_does_not_trigger() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["host.rs"]));
    type_str(&mut ed, "mail@host");
    assert!(
        !ed.autocomplete_visible(),
        "an `@` embedded in a word (mail@host) is not a mention"
    );
}

#[test]
fn space_after_query_closes_popup() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["main.rs"]));
    type_str(&mut ed, "@main");
    assert!(ed.autocomplete_visible());
    type_str(&mut ed, " ");
    assert!(
        !ed.autocomplete_visible(),
        "a space after the query terminates the token and closes the popup"
    );
}

#[test]
fn slash_free_query_never_lists_intermediate_path_components() {
    // Regression pin: `@RE` (basename prefix) must never list an entry that only
    // matched on an intermediate path component (`worktrees` contains "re").
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&[
        "README.md",
        ".gitignore",
        ".claude/worktrees/cache.txt",
    ]));
    type_str(&mut ed, "@RE");
    let got = popup_labels(&ed);
    assert!(got.contains(&"README.md".to_string()), "got: {got:?}");
    assert!(
        !got.iter().any(|l| l.contains("worktrees")),
        "intermediate path component matched, got: {got:?}"
    );
    assert!(
        !got.contains(&".gitignore".to_string()),
        "basename not prefixed by RE leaked, got: {got:?}"
    );
}

// --- VAL-EDITOR-025: Tab-accept is one undo unit ----------------------------

#[test]
fn tab_accept_reverts_cleanly_in_one_undo() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help"]));
    type_str(&mut ed, "/he");
    ed.handle_key(&tab());
    assert_eq!(ed.text(), "/help", "accept splices the full command");
    // A single undo cleanly reverts the accept back to what the user had typed —
    // the migration fix (accept is one undo unit, does not corrupt the buffer).
    ed.undo();
    assert_eq!(
        ed.text(),
        "/he",
        "one undo reverts the accept to the typed prefix"
    );
}

#[test]
fn path_accept_reverts_cleanly_in_one_undo() {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(path_provider(&["src/main.rs"]));
    type_str(&mut ed, "see @src/m");
    ed.handle_key(&tab());
    assert_eq!(ed.text(), "see @src/main.rs");
    ed.undo();
    assert_eq!(
        ed.text(),
        "see @src/m",
        "one undo reverts the path accept, buffer intact"
    );
}

// --- VAL-EDITOR-026: grown editor over a short pane keeps rows in bounds -----

#[test]
fn tall_editor_over_short_pane_keeps_popup_in_bounds() {
    // Grow the editor to its 8-row interior cap, then open a popup in a pane that
    // is barely taller than the box. Every painted popup row must stay inside the
    // area — none may overwrite a line above the box (a history row).
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(slash_provider(&["help", "history", "model"]));
    // Fill the box with enough newlines to hit the interior cap.
    for _ in 0..12 {
        ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
    }
    type_str(&mut ed, "/h");
    assert!(ed.autocomplete_visible());

    // A 10-row pane: the box grows to 8 interior + 2 border = 10 rows, leaving no
    // room below. The popup must paint zero rows rather than spill past the area.
    let cols = 40;
    let rows = 10;
    let area = Rect::new(0, 0, cols, rows);
    let painted = render_rows(&ed, cols, rows, area);
    // No painted row may exceed the pane width, and there are exactly `rows` rows.
    assert_eq!(painted.len(), rows as usize);
    for (y, line) in painted.iter().enumerate() {
        assert!(
            line.chars().count() <= cols as usize,
            "row {y} overflowed pane width: {line:?}"
        );
    }

    // A taller pane (16 rows) leaves room: the popup paints below the box, all
    // within bounds.
    let tall_rows = 16;
    let tall_area = Rect::new(0, 0, cols, tall_rows);
    let tall = render_rows(&ed, cols, tall_rows, tall_area);
    assert_eq!(tall.len(), tall_rows as usize);
    // The popup's selected row (`▸`) must appear, and only in the band below the
    // 10-row box (rows 10..16), never above it.
    let indicator_rows: Vec<usize> = tall
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains('▸'))
        .map(|(y, _)| y)
        .collect();
    assert!(
        !indicator_rows.is_empty(),
        "popup indicator painted in a tall pane: {tall:?}"
    );
    for y in indicator_rows {
        assert!(
            y >= 10,
            "popup row {y} landed inside/above the box (must be below it): {tall:?}"
        );
    }
}

// --- popup pure-state sanity (public surface) -------------------------------

#[test]
fn popup_default_is_closed() {
    let ac = Autocomplete::new();
    assert!(!ac.is_visible());
    assert!(ac.is_empty());
    assert!(ac.selected().is_none());
}
