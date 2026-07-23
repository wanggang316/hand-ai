//! Viewport erase/re-anchor boundary tests for the rt runtime (TestBackend).
//!
//! These pin the two "the runtime never wipes the fixed inline viewport it
//! reserved" defects the M1 runtime validation exposed — the leaks that had no
//! in-process guard because the rt layer had *zero* `TestBackend` scrollback
//! tests, so they slipped past unit testing entirely:
//!
//! - **Exit erase (VAL-CORE-016 / VAL-CORE-036).** On quit the runtime must wipe
//!   the inline viewport's rows before the session restores, so a bordered
//!   bottom-UI box is not left on screen as a ghost with the shell prompt
//!   overwriting inside it. In `TestBackend` terms: after
//!   [`clear_viewport_region`](hand_tui::rt::session::clear_viewport_region) the
//!   viewport buffer rows are blank while the transcript in scrollback is intact,
//!   and the viewport origin (`Frame::area()`) has not drifted.
//! - **Resize erase (VAL-CORE-010 / VAL-CORE-026).** ratatui recomputes an inline
//!   viewport lazily on the next `draw` after a backend size change, and that
//!   recompute (`compute_inline_size` → `append_lines`) scrolls the viewport's
//!   *current* cells — an old-width border box, or overlay rows — into native
//!   scrollback *before* it re-anchors. Wiping the viewport to blank *first*
//!   (via `clear_viewport_region`) means only blank rows can spill: no stale
//!   old-width fragment ever reaches scrollback.
//!
//! Every test drives the same fixed-max inline viewport the session builds
//! (`Viewport::Inline(MAX_VIEWPORT_ROWS)`) over a `TestBackend`, reproducing the
//! runtime's own draw/commit/resize ordering.

use hand_tui::rt::history::HistorySink;
use hand_tui::rt::session::{EraseOnDrop, clear_viewport_region};
use hand_tui::rt::view::{MAX_VIEWPORT_ROWS, bottom_area_geometry};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Position;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Build the fixed-max inline viewport terminal over a `TestBackend`, exactly as
/// the session does (`Viewport::Inline(MAX_VIEWPORT_ROWS)`), clamped to the pane
/// height so a short pane never reserves more rows than exist.
fn inline_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(MAX_VIEWPORT_ROWS.min(height)),
        },
    )
    .expect("build inline test terminal")
}

/// Build the fixed-max inline viewport terminal with the viewport anchored to the
/// **bottom** of the pane — the real launch geometry (a shell cursor sitting near
/// the last row when the session starts). This is the geometry that actually
/// leaks: with slack rows *above* the viewport, ratatui's inline resize recompute
/// (`compute_inline_size` → `append_lines`) scrolls the viewport's current
/// content off the top into scrollback. A top-anchored viewport (cursor at the
/// origin) has no rows to scroll off, so it never reproduces the defect.
fn bottom_anchored_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    let mut backend = TestBackend::new(width, height);
    // Put the cursor on the last row so the inline viewport anchors at the bottom.
    backend
        .set_cursor_position(Position::new(0, height.saturating_sub(1)))
        .expect("seed cursor at pane bottom");
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(MAX_VIEWPORT_ROWS.min(height)),
        },
    )
    .expect("build bottom-anchored inline test terminal")
}

/// Right-trim the rows of `buf` intersecting `y0..y1` (a sub-band of the pane),
/// top-down. Used to read just the viewport's rows, or just the rows above it.
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

/// The rows of just the inline viewport (its sub-rect of the pane's live buffer).
fn viewport_rows(terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    let area = terminal.get_frame().area();
    band_rows(terminal.backend().buffer(), area.top(), area.bottom())
}

/// Every committed-transcript row, wherever it lives after an inline commit: the
/// scrollback (oldest first) followed by the pane rows *above* the viewport (a
/// small transcript with room to spare stays in the live buffer above the
/// viewport rather than scrolling off into scrollback).
fn transcript_rows(terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    let area = terminal.get_frame().area();
    let scrollback = terminal.backend().scrollback();
    let mut out = band_rows(scrollback, scrollback.area.top(), scrollback.area.bottom());
    out.extend(band_rows(terminal.backend().buffer(), 0, area.top()));
    out
}

/// The scrollback rows (content that scrolled off the top), oldest first.
fn scrollback_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let scrollback = terminal.backend().scrollback();
    band_rows(scrollback, scrollback.area.top(), scrollback.area.bottom())
}

/// Paint the bottom UI: a bordered block over the whole active bottom area with a
/// marker inside, so the viewport carries visible content (a "box" that would
/// ghost on exit / leak on resize).
fn paint_bottom_box(terminal: &mut Terminal<TestBackend>, marker: &str) {
    let width = terminal.get_frame().area().width;
    let height = terminal.get_frame().area().height;
    let g = bottom_area_geometry(1, true, width, height);
    let marker = marker.to_string();
    terminal
        .draw(|frame| {
            frame.render_widget(Block::bordered(), g.active);
            let inner = ratatui::layout::Rect::new(
                g.input.x + 1,
                g.input.y,
                g.input.width.saturating_sub(2),
                g.input.height,
            );
            frame.render_widget(Paragraph::new(marker.clone()), inner);
        })
        .expect("draw the bottom box");
}

// ============================================================================
// Exit erase — the ghost-box defect (VAL-CORE-016 / VAL-CORE-036)
// ============================================================================

#[test]
fn exit_erase_wipes_the_viewport_box_and_keeps_transcript() {
    // A transcript in scrollback, a bordered bottom box painted in the live
    // viewport — the state at quit time.
    let mut terminal = inline_terminal(24, MAX_VIEWPORT_ROWS + 4);
    let mut sink = HistorySink::new();
    sink.commit_lines(
        &mut terminal,
        vec![
            Line::from("transcript line 1"),
            Line::from("transcript line 2"),
        ],
    )
    .expect("commit transcript");
    paint_bottom_box(&mut terminal, "BOTTOMUI");

    // The box is on screen before the erase — the exact ghost the shell prompt
    // would overwrite inside.
    let before = viewport_rows(&mut terminal);
    assert!(
        before.iter().any(|r| r.contains('┌') || r.contains('│')),
        "the bottom box border is painted before exit: {before:?}",
    );
    assert!(
        before.iter().any(|r| r.contains("BOTTOMUI")),
        "the bottom-UI marker is painted before exit: {before:?}",
    );

    let origin_before = terminal.get_frame().area();

    // Erase the viewport region the way the exit path must, before restore.
    clear_viewport_region(&mut terminal).expect("erase viewport on exit");

    // Every viewport row is blank now: no ghost border, no marker left behind for
    // the shell prompt to land inside.
    let after = viewport_rows(&mut terminal);
    assert!(
        after.iter().all(|r| r.is_empty()),
        "the viewport must be wiped clean on exit, got {after:?}",
    );

    // The committed transcript above the viewport is untouched — the erase only
    // wipes the reserved bottom band, never history.
    let transcript = transcript_rows(&mut terminal);
    assert!(
        transcript.iter().any(|r| r.contains("transcript line 1"))
            && transcript.iter().any(|r| r.contains("transcript line 2")),
        "the transcript above the viewport survives the exit erase, got {transcript:?}",
    );

    // The viewport origin did not drift — the erase re-anchors nothing.
    assert_eq!(
        terminal.get_frame().area(),
        origin_before,
        "the viewport origin must not drift across the exit erase",
    );
}

#[test]
fn erase_on_drop_into_inner_skips_the_erase_and_deref_is_transparent() {
    // The exit path wraps the scheduler-owned terminal in `EraseOnDrop` so the
    // wipe fires deterministically when the terminal drops at shutdown (no
    // reliance on a final scheduler frame). Two properties pin the wrapper:
    //
    // - It is a transparent stand-in for the terminal: a holder draws, reads its
    //   frame area, and commits through `Deref`/`DerefMut` exactly as through a
    //   bare `Terminal`.
    // - `into_inner` is the escape hatch that hands the terminal back *without*
    //   erasing, so a caller can take over the erase timing.
    //
    // (The wipe itself — that dropping the wrapper blanks the viewport — is the
    // exact operation `exit_erase_wipes_the_viewport_box_and_keeps_transcript`
    // pins, since `Drop` calls the same `clear_viewport_region`.)
    let terminal = inline_terminal(30, MAX_VIEWPORT_ROWS + 4);
    let mut wrapper = EraseOnDrop::new(terminal);

    // DerefMut reaches `get_frame`/`draw`.
    let area = wrapper.get_frame().area();
    assert_eq!(area.width, 30, "Deref exposes the inner viewport width");
    let g = bottom_area_geometry(1, true, area.width, wrapper.get_frame().area().height);
    wrapper
        .draw(|frame| frame.render_widget(Block::bordered(), g.active))
        .expect("draw through the wrapper");

    // `into_inner` hands the terminal back without erasing, so the just-drawn box
    // is still present on the recovered terminal.
    let inner = wrapper.into_inner();
    let rows = band_rows(inner.backend().buffer(), 0, MAX_VIEWPORT_ROWS + 4);
    assert!(
        rows.iter().any(|r| r.contains('┌') || r.contains('│')),
        "into_inner skips the erase, so the box survives: {rows:?}",
    );
}

// ============================================================================
// Resize erase — old-width box must not leak to scrollback (VAL-CORE-010)
// ============================================================================
//
// These use a *bottom-anchored* viewport and a **widen** — the resize direction
// that actually leaks. A widen issues no horizontal-shrink full clear, so the
// inline resize recompute's `append_lines` scrolls the viewport's current
// old-width content off the top and into scrollback unless the runtime wipes the
// viewport first. Each test is a real guard: neutralize the
// `clear_viewport_region` call and the old-width border/marker reappears in
// scrollback.

#[test]
fn resize_erase_keeps_old_width_box_out_of_scrollback() {
    // Narrow, bottom-anchored pane with a bordered bottom box — the frame a
    // mid-stream resize interrupts.
    let mut terminal = bottom_anchored_terminal(40, MAX_VIEWPORT_ROWS + 6);
    paint_bottom_box(&mut terminal, "NARROWBOX");

    // The runtime wipes the viewport region *before* the next draw autoresizes
    // (which would otherwise scroll the old-width box into scrollback via
    // `append_lines`), then the backend widens.
    clear_viewport_region(&mut terminal).expect("erase viewport before resize re-anchor");
    terminal.backend_mut().resize(100, MAX_VIEWPORT_ROWS + 6);

    // Redraw at the new width: this is the draw that autoresizes / re-anchors.
    paint_bottom_box(&mut terminal, "WIDEBOX");

    // No old-width fragment reached scrollback: no border glyphs, no stale marker.
    for row in &scrollback_rows(&terminal) {
        assert!(
            !row.contains('┌') && !row.contains('┐') && !row.contains('│') && !row.contains('└'),
            "no old-width border fragment may leak to scrollback, got {row:?}",
        );
        assert!(
            !row.contains("NARROWBOX"),
            "the old-width box marker must not leak to scrollback, got {row:?}",
        );
    }
}

#[test]
fn resize_erase_keeps_old_width_overlay_out_of_scrollback() {
    // A full-width overlay band painted over the bottom box (the string a
    // capture-pane probe searches for), on a bottom-anchored pane, then a widen
    // while it is "open".
    let mut terminal = bottom_anchored_terminal(40, MAX_VIEWPORT_ROWS + 6);
    let overlay_marker = "OVERLAYROW";
    let area = terminal.get_frame().area();
    let width = area.width;
    let g = bottom_area_geometry(1, true, width, terminal.get_frame().area().height);
    terminal
        .draw(|frame| {
            frame.render_widget(Block::bordered(), g.active);
            // A full-width overlay band across the top of the viewport.
            let overlay = ratatui::layout::Rect::new(area.x, area.top(), width, 1);
            frame.render_widget(
                Paragraph::new(overlay_marker.repeat(width as usize / overlay_marker.len())),
                overlay,
            );
        })
        .expect("draw overlay + box");

    // Erase before the resize re-anchor, then widen and redraw without the overlay
    // (Esc-close mid-resize).
    clear_viewport_region(&mut terminal).expect("erase viewport before resize");
    terminal.backend_mut().resize(100, MAX_VIEWPORT_ROWS + 6);
    paint_bottom_box(&mut terminal, "AFTER");

    for row in &scrollback_rows(&terminal) {
        assert!(
            !row.contains(overlay_marker),
            "the old-width overlay row must not leak to scrollback, got {row:?}",
        );
    }
}

// ============================================================================
// Viewport non-drift across a resize (VAL-CORE-010 re-anchor)
// ============================================================================

#[test]
fn resize_re_anchors_viewport_without_drift_or_leak() {
    // Paint a bottom box on a narrow bottom-anchored pane, widen with the
    // erase-first ordering, and redraw. The viewport must re-anchor to the new
    // width while keeping its fixed height (no drift of the reserved band), and no
    // old-width fragment may leak into scrollback.
    let mut terminal = bottom_anchored_terminal(30, MAX_VIEWPORT_ROWS + 8);
    paint_bottom_box(&mut terminal, "BEFORE");

    let area_before = terminal.get_frame().area();
    assert_eq!(area_before.width, 30, "starts at the narrow width");
    assert_eq!(
        area_before.height, MAX_VIEWPORT_ROWS,
        "starts at the fixed viewport height",
    );

    clear_viewport_region(&mut terminal).expect("erase before resize");
    terminal.backend_mut().resize(60, MAX_VIEWPORT_ROWS + 8);
    paint_bottom_box(&mut terminal, "AFTER");

    // The viewport width tracked the resize (re-anchored to the new width) and the
    // height stayed the fixed max — no drift of the reserved band.
    let area = terminal.get_frame().area();
    assert_eq!(area.width, 60, "viewport re-anchors to the new width");
    assert_eq!(
        area.height, MAX_VIEWPORT_ROWS,
        "viewport keeps its fixed height across the resize",
    );

    // No old-width box fragment leaked into scrollback.
    for row in &scrollback_rows(&terminal) {
        assert!(
            !row.contains("BEFORE") && !row.contains('┌'),
            "no old-width bottom-box fragment leaked to scrollback, got {row:?}",
        );
    }
}
