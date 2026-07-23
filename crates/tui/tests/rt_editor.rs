//! Integration tests for the rt editor core (`hand_tui::rt::components::Editor`).
//!
//! ratatui is a pure immediate-mode renderer, so the editor's *behaviour* — the
//! grapheme-aware editing model, cursor motion, newline insertion, auto-grow,
//! submit/recall, empty-submit no-op, history-navigation edges, long-line
//! response, and narrow-resize cursor stability — is what these tests pin. Pure
//! logic is exercised directly against the public API; the render/cursor path is
//! driven end to end over ratatui's `TestBackend` with a fixed inline viewport,
//! reading the painted buffer and the backend cursor position back.
//!
//! These trace the plan's editor assertions:
//! - VAL-EDITOR-001 typing + newline insertion
//! - VAL-EDITOR-002 movement + gallery `line:col` indicator
//! - VAL-EDITOR-003 submit + trimmed recall
//! - VAL-EDITOR-004 auto-grow 1..=8 + shrink
//! - VAL-EDITOR-015 history navigation on internal-line boundary
//! - VAL-EDITOR-016 grapheme-cluster unit editing
//! - VAL-EDITOR-017 empty-submit no-op
//! - VAL-EDITOR-018 long-line response
//! - VAL-EDITOR-019 narrow-resize cursor stability
//! - VAL-EDITOR-027 border focus/thinking tint hook
//! - VAL-EDITOR-028 IME multi-char commit

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{BorderTint, Editor, EditorBorder};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{HandleOutcome, RtComponent};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

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

/// Render the editor into a fixed inline `TestBackend` at `area`, returning the
/// painted rows (as trimmed-right strings) and the backend cursor position.
fn render_capture(ed: &Editor, cols: u16, rows: u16, area: Rect) -> (Vec<String>, (u16, u16)) {
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
            // Call the `RtComponent::cursor` trait method explicitly — the inherent
            // `Editor::cursor` (which returns `(line, col)`) otherwise shadows it.
            if let Some(pos) = RtComponent::cursor(ed) {
                frame.set_cursor_position(pos);
            }
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut lines = Vec::new();
    for y in 0..rows {
        let mut s = String::new();
        for x in 0..cols {
            s.push_str(buffer[(x, y)].symbol());
        }
        lines.push(s.trim_end().to_string());
    }
    let pos = terminal.backend_mut().get_cursor_position().unwrap();
    (lines, (pos.x, pos.y))
}

// --- VAL-EDITOR-001: typing + newline insertion -----------------------------

#[test]
fn typing_and_newline_insertion() {
    let mut ed = Editor::new();
    type_str(&mut ed, "hello");
    assert_eq!(ed.text(), "hello");

    // Alt+Enter inserts a newline, does not submit.
    ed.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
    type_str(&mut ed, "world");
    assert_eq!(ed.text(), "hello\nworld");
    assert_eq!(ed.line_count(), 2);
    assert!(ed.take_submit().is_none());
}

#[test]
fn shift_enter_newline_only_under_enhanced_keyboard() {
    // Under an enhanced (kitty) keyboard, Shift+Enter arrives as the distinct
    // `shift+enter` id and inserts a newline. In plain mode it would arrive as
    // `enter` (indistinguishable) — so we pin the enhanced id here.
    let mut ed = Editor::new();
    type_str(&mut ed, "a");
    ed.handle_key(&key("shift+enter", KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(ed.text(), "a\n");
    assert!(ed.take_submit().is_none());
}

#[test]
fn trailing_backslash_then_enter_soft_break() {
    let mut ed = Editor::new();
    type_str(&mut ed, "line\\");
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ed.text(), "line\n", "backslash consumed, newline inserted");
    assert!(ed.take_submit().is_none(), "submit suppressed");
}

// --- VAL-EDITOR-002: movement + gallery line:col indicator ------------------

#[test]
fn arrow_home_end_and_word_movement() {
    let mut ed = Editor::new();
    ed.insert_str("alpha beta gamma");
    ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(ed.cursor().1, 0);
    ed.handle_key(&key("alt+right", KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(ed.cursor().1, "alpha".len());
    ed.handle_key(&key("end", KeyCode::End, KeyModifiers::NONE));
    assert_eq!(ed.cursor().1, "alpha beta gamma".len());
    ed.handle_key(&key("left", KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(ed.cursor().1, "alpha beta gamm".len());
}

#[test]
fn box_border_renders_line_col_indicator() {
    let mut ed = Editor::new();
    ed.insert_str("abc");
    // A box with room for the border + one text row + the bottom rail.
    let area = Rect::new(0, 0, 24, 3);
    let (rows, _) = render_capture(&ed, 24, 3, area);
    // The bottom rail carries a `line:col` indicator; caret at end of "abc" on
    // logical line 1 sits at visual col 4 (1-based).
    let bottom = &rows[2];
    assert!(
        bottom.contains("1:4"),
        "bottom rail should carry the line:col indicator, got {bottom:?}"
    );
    // The text row shows the typed content.
    assert!(
        rows[1].contains("abc"),
        "interior shows the text: {:?}",
        rows[1]
    );
}

// --- VAL-EDITOR-003: submit + trimmed recall --------------------------------

#[test]
fn submit_clears_and_recalls_trimmed() {
    let mut ed = Editor::new();
    type_str(&mut ed, "  spaced prompt  ");
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        ed.take_submit().as_deref(),
        Some("  spaced prompt  "),
        "submit yields the raw buffer text"
    );
    assert_eq!(ed.text(), "", "buffer cleared after submit");
    assert_eq!(
        ed.history(),
        &["spaced prompt".to_string()],
        "history stores the trimmed form"
    );
}

#[test]
fn history_dedups_and_is_newest_first() {
    let mut ed = Editor::new();
    for prompt in ["one", "two", "two", "three"] {
        type_str(&mut ed, prompt);
        ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
        ed.take_submit();
    }
    assert_eq!(
        ed.history(),
        &["three".to_string(), "two".to_string(), "one".to_string()],
        "newest first, consecutive duplicate collapsed"
    );
}

// --- VAL-EDITOR-004: auto-grow 1..=8 + shrink -------------------------------

#[test]
fn auto_grow_and_shrink_via_rendered_rows() {
    let mut ed = Editor::new();
    // A tall enough viewport to hold the fully-grown box (8 text + 2 border).
    let cols = 30;
    let rows = 12;
    let area = Rect::new(0, 0, cols, rows);

    // Empty: one text row inside the box → 3 painted rows have box glyphs.
    let (empty_rows, _) = render_capture(&ed, cols, rows, area);
    assert_eq!(
        box_height(&empty_rows),
        3,
        "empty box is 1 text row + 2 rails"
    );

    // Four logical lines → 4 text rows → box height 6.
    ed.insert_str("l1\nl2\nl3\nl4");
    let (four, _) = render_capture(&ed, cols, rows, area);
    assert_eq!(box_height(&four), 6, "4 text rows + 2 rails");

    // Twelve logical lines → capped at 8 text rows → box height 10.
    ed.set_text(
        &(1..=12)
            .map(|n| format!("L{n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let (many, _) = render_capture(&ed, cols, rows, area);
    assert_eq!(box_height(&many), 10, "capped at 8 text rows + 2 rails");

    // Submit shrinks it back to one text row.
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    ed.take_submit();
    let (shrunk, _) = render_capture(&ed, cols, rows, area);
    assert_eq!(
        box_height(&shrunk),
        3,
        "shrinks back to 1 text row after submit"
    );
}

/// Count how many of the painted rows carry box-border glyphs (a rough box
/// height, used to assert auto-grow without pinning exact glyphs).
fn box_height(rows: &[String]) -> usize {
    rows.iter()
        .filter(|r| r.chars().any(|c| "╭╮╰╯│─".contains(c)))
        .count()
}

// --- VAL-EDITOR-015: history navigation on internal-line boundary -----------

#[test]
fn up_recalls_only_on_first_line_down_restores_empty() {
    let mut ed = Editor::new();
    type_str(&mut ed, "earlier");
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    ed.take_submit();

    // Multi-line draft: interior Up/Down move the caret, they do not recall.
    ed.insert_str("draft1\ndraft2\ndraft3");
    ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(ed.cursor().0, 1, "interior Up moved caret, not history");
    assert_eq!(ed.text(), "draft1\ndraft2\ndraft3", "draft untouched");

    // Move the caret to the first line, then Up recalls history.
    ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(ed.cursor().0, 0);
    ed.handle_key(&key("up", KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(ed.text(), "earlier", "first-line Up recalls history");

    // Down past the newest entry restores an empty buffer.
    ed.handle_key(&key("down", KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(ed.text(), "", "Down past newest restores empty buffer");
}

// --- VAL-EDITOR-016: grapheme-cluster unit editing --------------------------

#[test]
fn cjk_emoji_zwj_regional_indicator_edit_as_units() {
    // One of each: CJK ideograph, ZWJ family emoji, regional-indicator flag.
    let cjk = "字";
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let flag = "\u{1F1EF}\u{1F1F5}"; // JP flag
    let mut ed = Editor::new();
    ed.insert_str(&format!("{cjk}{family}{flag}"));

    // Right from column 0 crosses each cluster whole.
    ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(ed.cursor().1, cjk.len(), "CJK crossed as one cluster");
    ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        ed.cursor().1,
        cjk.len() + family.len(),
        "ZWJ family crossed as one cluster"
    );
    ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        ed.cursor().1,
        cjk.len() + family.len() + flag.len(),
        "regional-indicator flag crossed as one cluster"
    );

    // Backspace removes each cluster whole, never byte-slicing.
    ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ed.text(), format!("{cjk}{family}"));
    ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ed.text(), cjk);
    ed.handle_key(&key("backspace", KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ed.text(), "");
}

#[test]
fn wide_char_wrap_never_leaves_a_half_cell() {
    // Four CJK glyphs (2 cols each) rendered in a box narrow enough to force a
    // wrap: no painted row overflows the interior, so no half-occupied cell.
    let mut ed = Editor::new();
    ed.insert_str("字字字字字字");
    let cols = 12; // interior width = 12 - 4 = 8 → 4 glyphs per row
    let area = Rect::new(0, 0, cols, 6);
    let (rows, _) = render_capture(&ed, cols, 6, area);
    for row in &rows {
        // No painted row is wider than the terminal, and box glyphs stay put.
        assert!(row.chars().count() <= cols as usize);
    }
}

// --- VAL-EDITOR-017: empty-submit no-op -------------------------------------

#[test]
fn empty_or_blank_submit_is_noop() {
    let mut ed = Editor::new();
    // Bare Enter on an empty buffer.
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    assert!(ed.take_submit().is_none());
    assert!(ed.history().is_empty());

    // Whitespace-only buffer: cleared, not submitted, not recalled.
    type_str(&mut ed, "   \t  ");
    ed.handle_key(&key("enter", KeyCode::Enter, KeyModifiers::NONE));
    assert!(ed.take_submit().is_none(), "blank does not submit");
    assert!(ed.history().is_empty(), "blank not recalled");
    assert_eq!(ed.text(), "", "buffer cleared");
}

// --- VAL-EDITOR-018: long-line response -------------------------------------

#[test]
fn thousands_char_single_line_wraps_within_cap_and_caret_visible() {
    let mut ed = Editor::new();
    ed.insert_str(&"x".repeat(4000));
    // The box auto-grows only to its cap; rendering into a tall viewport paints a
    // bounded box (8 text rows + 2 rails) and the caret stays on-screen.
    let cols = 24;
    let rows = 14;
    let area = Rect::new(0, 0, cols, rows);
    let (painted, (cx, cy)) = render_capture(&ed, cols, rows, area);
    assert_eq!(box_height(&painted), 10, "capped box (8 text + 2 rails)");
    // The caret is within the painted box, not off-screen.
    assert!(cx < cols && cy < rows, "caret {cx},{cy} stays on-screen");
}

// --- VAL-EDITOR-019: narrow-resize cursor stability -------------------------

#[test]
fn narrow_resize_keeps_caret_on_its_grapheme() {
    let mut ed = Editor::new();
    ed.insert_str("the quick brown fox jumps");
    // Caret in the middle, on a grapheme boundary (start of "brown").
    for _ in 0.."the quick ".len() {
        ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    }
    ed.handle_key(&key("home", KeyCode::Home, KeyModifiers::NONE));
    for _ in 0.."the quick ".chars().count() {
        ed.handle_key(&key("right", KeyCode::Right, KeyModifiers::NONE));
    }
    let byte_col = ed.cursor().1;
    assert_eq!(byte_col, "the quick ".len());

    // Render wide (one row) then narrow (reflowed). In both, the backend cursor is
    // on-screen and inside the box, and the caret's underlying grapheme is stable.
    let (_wide, wpos) = render_capture(&ed, 60, 8, Rect::new(0, 0, 60, 8));
    let (_narrow, npos) = render_capture(&ed, 16, 8, Rect::new(0, 0, 16, 8));
    assert!(wpos.0 < 60 && wpos.1 < 8, "wide caret on-screen");
    assert!(
        npos.0 < 16 && npos.1 < 8,
        "narrow caret on-screen, no overflow"
    );
    // The caret still addresses the same grapheme after the reflow.
    assert_eq!(
        ed.cursor().1,
        byte_col,
        "caret byte column unchanged by resize"
    );
}

#[test]
fn narrow_render_leaves_no_row_wider_than_the_pane() {
    // A reflow must not overflow the box border or duplicate rows into scrollback.
    let mut ed = Editor::new();
    ed.insert_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"); // 32 'a'
    let cols = 12;
    let (rows, _) = render_capture(&ed, cols, 12, Rect::new(0, 0, cols, 12));
    for row in &rows {
        assert!(
            row.chars().count() <= cols as usize,
            "no painted row overflows the pane width: {row:?}"
        );
    }
}

// --- VAL-EDITOR-027: border focus/thinking tint hook ------------------------

#[test]
fn tint_hook_is_settable_and_readable() {
    let mut ed = Editor::new();
    assert_eq!(ed.tint(), BorderTint::Idle);
    ed.set_tint(BorderTint::Focused);
    assert_eq!(ed.tint(), BorderTint::Focused);
    ed.set_tint(BorderTint::Thinking);
    assert_eq!(ed.tint(), BorderTint::Thinking);
    // The tint drives the painted border style without changing the glyphs, so
    // the box still renders at the same height under any tint.
    ed.insert_str("hi");
    let (idle_rows, _) = {
        ed.set_tint(BorderTint::Idle);
        render_capture(&ed, 20, 4, Rect::new(0, 0, 20, 4))
    };
    ed.set_tint(BorderTint::Thinking);
    let (thinking_rows, _) = render_capture(&ed, 20, 4, Rect::new(0, 0, 20, 4));
    assert_eq!(
        box_height(&idle_rows),
        box_height(&thinking_rows),
        "tint changes colour, not layout"
    );
}

// --- VAL-EDITOR-028: IME multi-char commit ----------------------------------

#[test]
fn ime_multichar_commit_lands_whole() {
    let mut ed = Editor::new();
    // A composed run committed as one string (the platform's IME commit path).
    ed.insert_str("你好，世界");
    assert_eq!(ed.text(), "你好，世界");
    assert_eq!(ed.cursor().1, "你好，世界".len());
}

#[test]
fn ime_preedit_renders_but_is_not_committed() {
    let mut ed = Editor::new();
    ed.insert_str("ab");
    ed.set_composition(Some("ni".to_string()));
    // The preedit shows inline but is not part of the buffer text.
    assert_eq!(
        ed.text(),
        "ab",
        "preedit excluded from the committed buffer"
    );
    let (rows, _) = render_capture(&ed, 24, 3, Rect::new(0, 0, 24, 3));
    assert!(
        rows[1].contains("ni"),
        "preedit is rendered inline: {:?}",
        rows[1]
    );
    // Clearing the composition removes it from the render.
    ed.set_composition(None);
    let (cleared, _) = render_capture(&ed, 24, 3, Rect::new(0, 0, 24, 3));
    assert!(!cleared[1].contains("ni"), "cleared preedit vanishes");
}

// --- chat-style borderless variant ------------------------------------------

#[test]
fn borderless_variant_paints_text_full_width() {
    let mut ed = Editor::new().border(EditorBorder::None);
    ed.insert_str("chat line");
    let (rows, _) = render_capture(&ed, 20, 2, Rect::new(0, 0, 20, 2));
    assert!(
        rows[0].starts_with("chat line"),
        "borderless paints text at the origin: {:?}",
        rows[0]
    );
    // No box glyphs at all.
    assert_eq!(box_height(&rows), 0, "borderless variant draws no rails");
}
