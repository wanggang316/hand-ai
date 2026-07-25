//! Runtime inline-viewport height changes (TestBackend).
//!
//! The fixed-max strategy makes the inline viewport height a build-time
//! constant; [`set_inline_viewport_height`] is the controlled exception that
//! rebuilds the terminal at a new height for the modal overlay panel (grow at
//! mount, shrink back at unmount). These tests pin the invariants the rebuild
//! must hold:
//!
//! - **Ghost-free grow→shrink.** After growing and shrinking back, no cell of
//!   the previously-taller viewport survives above the shrunk viewport — the
//!   erase-first ordering wipes the band before the rebuild re-anchors.
//! - **No scrollback leak.** Room-making for a grow scrolls only committed
//!   transcript rows into scrollback, never live viewport cells (border
//!   glyphs); a shrink scrolls nothing at all.
//! - **Bottom-pinned shrink.** The shrunk viewport keeps its bottom edge where
//!   the grown one ended, so the box + footer do not jump when an overlay
//!   closes.
//! - **The teardown erase still works.** [`EraseOnDrop`] wipes the *rebuilt*
//!   viewport on drop exactly as it wipes an original one.

use hand_tui::rt::session::{EraseOnDrop, set_inline_viewport_height};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::Block;
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Build an inline terminal of `rows` viewport rows over a `width`×`height`
/// pane, with the backend cursor seeded at `cursor_row` so the viewport anchors
/// there (the shell-prompt row at session start).
fn inline_terminal_at(
    width: u16,
    height: u16,
    rows: u16,
    cursor_row: u16,
) -> Terminal<TestBackend> {
    let mut backend = TestBackend::new(width, height);
    backend
        .set_cursor_position(Position::new(0, cursor_row))
        .expect("seed cursor row");
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(rows),
        },
    )
    .expect("build inline test terminal")
}

/// Fill the whole current viewport with a bordered box, so the viewport carries
/// the border cells whose leak/ghost the invariants forbid.
fn paint_full_viewport_box(terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(Block::bordered(), area);
        })
        .expect("draw bordered viewport");
}

/// Right-trimmed rows of `buf` in `y0..y1`.
fn band_rows(buf: &ratatui::buffer::Buffer, y0: u16, y1: u16) -> Vec<String> {
    let area = buf.area;
    let lo = y0.max(area.y);
    let hi = y1.min(area.y + area.height);
    (lo..hi)
        .map(|y| {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            row.trim_end().to_string()
        })
        .collect()
}

/// Whether any row in the list carries a box-drawing border glyph.
fn any_border(rows: &[String]) -> bool {
    rows.iter().any(|row| {
        row.contains('─')
            || row.contains('│')
            || row.contains('┌')
            || row.contains('┐')
            || row.contains('└')
            || row.contains('┘')
    })
}

#[test]
fn grow_extends_downward_when_the_screen_has_room() {
    // Viewport 4 rows at row 2 of a 12-row pane; growing to 8 claims the blank
    // rows *below* it — the top edge stays put, nothing scrolls.
    let mut terminal = inline_terminal_at(20, 12, 4, 2);
    assert_eq!(terminal.get_frame().area(), Rect::new(0, 2, 20, 4));
    paint_full_viewport_box(&mut terminal);

    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 8)
        .expect("grow the inline viewport");

    assert_eq!(
        terminal.get_frame().area(),
        Rect::new(0, 2, 20, 8),
        "the grown viewport keeps its top row and extends downward"
    );
    assert!(
        band_rows(terminal.backend().scrollback(), 0, u16::MAX).is_empty(),
        "an in-room grow scrolls nothing into scrollback"
    );

    // The rebuilt terminal draws normally at the new height.
    paint_full_viewport_box(&mut terminal);
    let rows = band_rows(terminal.backend().buffer(), 2, 10);
    assert!(any_border(&rows), "the taller viewport repaints in full");
}

#[test]
fn grow_at_the_screen_bottom_scrolls_transcript_not_viewport_cells() {
    // Transcript text fills the rows above a bottom-anchored viewport. Growing
    // past the screen edge must scroll *transcript* rows into scrollback —
    // never the viewport's own (erased-first) border cells.
    let mut backend = TestBackend::with_lines([
        "transcript-0        ",
        "transcript-1        ",
        "transcript-2        ",
        "transcript-3        ",
        "                    ",
        "                    ",
        "                    ",
        "                    ",
    ]);
    backend
        .set_cursor_position(Position::new(0, 4))
        .expect("seed cursor row");
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("build inline test terminal");
    assert_eq!(terminal.get_frame().area(), Rect::new(0, 4, 20, 4));
    paint_full_viewport_box(&mut terminal);

    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 6)
        .expect("grow the inline viewport at the screen bottom");

    assert_eq!(
        terminal.get_frame().area(),
        Rect::new(0, 2, 20, 6),
        "the screen scrolled up exactly the two missing rows"
    );
    let scrollback = terminal.backend().scrollback();
    let scrolled = band_rows(scrollback, scrollback.area.top(), scrollback.area.bottom());
    assert!(
        scrolled.iter().any(|row| row.contains("transcript-0")),
        "the scrolled-off rows are transcript rows: {scrolled:?}"
    );
    assert!(
        !any_border(&scrolled),
        "no live viewport cell (border glyph) may leak into scrollback: {scrolled:?}"
    );
}

#[test]
fn shrink_pins_the_bottom_edge_and_leaves_no_ghost_above() {
    // An 8-row viewport at row 2 shrinks to 4: the new viewport keeps the old
    // bottom edge (rows 6..10) and the freed band above it (rows 2..6) is blank
    // — no border fragment of the taller frame survives.
    let mut terminal = inline_terminal_at(20, 12, 8, 2);
    paint_full_viewport_box(&mut terminal);

    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 4)
        .expect("shrink the inline viewport");

    assert_eq!(
        terminal.get_frame().area(),
        Rect::new(0, 6, 20, 4),
        "the shrunk viewport keeps its bottom edge pinned"
    );
    paint_full_viewport_box(&mut terminal);

    let freed = band_rows(terminal.backend().buffer(), 2, 6);
    assert!(
        !any_border(&freed),
        "the freed band above the shrunk viewport must be blank, got: {freed:?}"
    );
    assert!(
        freed.iter().all(String::is_empty),
        "no residue of any kind above the shrunk viewport: {freed:?}"
    );
    assert!(
        band_rows(terminal.backend().scrollback(), 0, u16::MAX).is_empty(),
        "a shrink scrolls nothing into scrollback"
    );
}

#[test]
fn grow_then_shrink_round_trip_keeps_insert_before_working() {
    // The rebuilt terminal is still an inline viewport: history commits via
    // insert_before keep landing above it after a grow→shrink round trip.
    let mut terminal = inline_terminal_at(20, 12, 4, 2);
    paint_full_viewport_box(&mut terminal);

    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 8)
        .expect("grow the inline viewport");
    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 4)
        .expect("shrink the inline viewport back");
    assert_eq!(terminal.get_frame().area(), Rect::new(0, 6, 20, 4));

    terminal
        .insert_before(1, |buf| {
            buf.set_string(0, 0, "COMMITTED", ratatui::style::Style::default());
        })
        .expect("insert_before still works after the round trip");
    // insert_before pushes a not-yet-bottom viewport down one row, so re-read
    // the origin and look at everything above it.
    let top = terminal.get_frame().area().top();
    let above = band_rows(terminal.backend().buffer(), 0, top);
    assert!(
        above.iter().any(|row| row.contains("COMMITTED")),
        "the committed line lands above the viewport: {above:?}"
    );
}

#[test]
fn teardown_erase_wipes_the_rebuilt_viewport() {
    // The teardown erase (what `EraseOnDrop::drop` runs) keys off the *current*
    // viewport: after a rebuild it wipes the rebuilt region exactly as it would
    // wipe an original one — the exit path is unaffected by the height change.
    // The wrapper itself is exercised around a rebuilt terminal too: the
    // primitive mutates the terminal in place through `DerefMut`, so the
    // wrapper keeps guarding the same slot.
    let terminal = inline_terminal_at(20, 12, 4, 2);
    let mut wrapper = EraseOnDrop::new(terminal);
    set_inline_viewport_height(&mut wrapper, TestBackend::new(1, 1), 8)
        .expect("grow the inline viewport through the EraseOnDrop wrapper");
    paint_full_viewport_box(&mut wrapper);

    // `EraseOnDrop::drop` consumes the backend with the terminal, so run its
    // exact operation on the unwrapped terminal and observe the result.
    let mut terminal = wrapper.into_inner();
    let area = terminal.get_frame().area();
    assert_eq!(
        area,
        Rect::new(0, 2, 20, 8),
        "guarding the rebuilt viewport"
    );
    hand_tui::rt::session::clear_viewport_region(&mut terminal).expect("teardown erase");
    let rows = band_rows(terminal.backend().buffer(), area.top(), area.bottom());
    assert!(
        rows.iter().all(String::is_empty),
        "the teardown erase wipes the rebuilt viewport region: {rows:?}"
    );
}

#[test]
fn zero_rows_is_clamped_to_one() {
    // A degenerate request never builds an empty viewport.
    let mut terminal = inline_terminal_at(20, 12, 4, 2);
    set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), 0)
        .expect("clamp a zero-row request");
    assert_eq!(terminal.get_frame().area().height, 1);
}
