//! Oversized-commit regression tests for the rt bottom-UI (TestBackend).
//!
//! M1 stage-3 runtime validation (VAL-CORE-033) exposed a defect: committing a
//! block taller than the room above the inline viewport made the bottom UI box
//! vanish and the demo stop responding. The root cause is a coordinate bug in
//! how the bottom area is placed, surfaced by `insert_before`:
//!
//! - The inline viewport is **not pinned to the screen top**. Ratatui's
//!   `insert_before` slides the viewport downward as scrollback fills; an
//!   oversized block (2×+ the viewport height) pushes the viewport origin
//!   (`Frame::area().y`) from 0 down to near the bottom of the pane in one commit.
//! - [`bottom_area_geometry`] returns rects in *viewport-local* coordinates (y=0
//!   = the top of the viewport). Painting those rects at absolute row 0 is only
//!   correct while the viewport sits at the top; once the viewport has drifted
//!   down, the box paints at rows the viewport no longer covers — off-screen —
//!   and the bottom UI disappears (the G3 "second block also vanishes" symptom is
//!   the same drift, once the viewport has already moved).
//!
//! The fix translates the geometry into the viewport's absolute rows via
//! [`BottomGeometry::offset_y`]`(area.y)` before drawing. These tests pin the
//! three properties a correct oversized commit must hold, all observable on
//! `TestBackend` (which faithfully models scroll regions, scrollback, and the
//! viewport origin):
//!
//! - **(a) complete, ordered scrollback.** Every row of an oversized block lands
//!   exactly once, in emission order, across scrollback + the live buffer.
//! - **(b) viewport-tracked bottom UI (non-drift).** After an oversized commit —
//!   and after several successive commits — the bottom box painted via
//!   `offset_y(area.y)` lands on the viewport's actual rows, its top/bottom
//!   border on the first/last active row, so it never renders off-screen.
//! - **(c) the offset is load-bearing.** Painting the same geometry *without*
//!   the offset leaves the drifted viewport blank — the fail-first guard proving
//!   the fix is what keeps the box visible.

use hand_tui::rt::history::HistorySink;
use hand_tui::rt::view::{MAX_VIEWPORT_ROWS, bottom_area_geometry};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Build the fixed-max inline viewport over a `TestBackend`, anchored to the
/// **top** of the pane (a fresh shell cursor at the origin). This is the launch
/// geometry that reproduces the defect: the viewport starts at `y = 0`, and an
/// oversized commit slides it down, so the bottom UI must follow it.
fn top_launched_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(MAX_VIEWPORT_ROWS.min(height)),
        },
    )
    .expect("build top-launched inline test terminal")
}

/// Build the fixed-max inline viewport anchored to the **bottom** of the pane
/// (shell cursor near the last row), the steady-state geometry after content has
/// filled the screen.
fn bottom_anchored_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    let mut backend = TestBackend::new(width, height);
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

/// Read every scrollback row (oldest first) then every live-buffer row
/// (top-down), right-trimmed — the committed-history stream in emission order.
fn committed_stream(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let mut out = Vec::new();
    let read = |buf: &ratatui::buffer::Buffer, out: &mut Vec<String>| {
        let area = buf.area;
        for y in area.y..area.y + area.height {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            out.push(row.trim_end().to_string());
        }
    };
    read(terminal.backend().scrollback(), &mut out);
    read(terminal.backend().buffer(), &mut out);
    out
}

/// The row markers (`blk-*`) present in the committed stream, in order.
fn seen_markers(terminal: &Terminal<TestBackend>) -> Vec<String> {
    committed_stream(terminal)
        .into_iter()
        .filter(|r| r.starts_with("blk-"))
        .collect()
}

/// The trimmed text of a single live-buffer row.
fn buffer_row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buf = terminal.backend().buffer();
    let mut row = String::new();
    for x in buf.area.x..buf.area.x + buf.area.width {
        if let Some(cell) = buf.cell((x, y)) {
            row.push_str(cell.symbol());
        }
    }
    row.trim_end().to_string()
}

/// Draw the bottom UI box the way the demo does: compute the viewport-local
/// geometry, translate it into the viewport's absolute rows with `offset_y`, and
/// render a bordered block over the active area. Returns the absolute active rect.
fn draw_bottom_ui(terminal: &mut Terminal<TestBackend>, input_rows: u16) -> Rect {
    let area = terminal.get_frame().area();
    let g = bottom_area_geometry(input_rows, true, area.width, area.height).offset_y(area.y);
    let active = g.active;
    terminal
        .draw(|frame| {
            frame.render_widget(Block::default().borders(Borders::ALL), active);
        })
        .expect("draw bottom ui");
    active
}

/// Commit `n` marker rows as one block, returning the block for reuse.
fn commit_block(terminal: &mut Terminal<TestBackend>, sink: &mut HistorySink, n: usize) {
    let block: Vec<Line> = (0..n).map(|i| Line::from(format!("blk-{i:02}"))).collect();
    sink.commit_rows(terminal, block).expect("commit");
}

// --- (a) complete, ordered scrollback --------------------------------------

#[test]
fn oversized_commit_lands_complete_and_ordered() {
    // 30x30 pane => viewport height 11. A 40-row block (> 3x the viewport) must
    // land in full, in order, across scrollback + the live buffer.
    let mut terminal = top_launched_terminal(30, 30);
    let mut sink = HistorySink::new();

    commit_block(&mut terminal, &mut sink, 40);

    let expected: Vec<String> = (0..40).map(|i| format!("blk-{i:02}")).collect();
    assert_eq!(
        seen_markers(&terminal),
        expected,
        "every oversized-block row lands exactly once, in emission order",
    );
}

// --- (b) viewport-tracked bottom UI (non-drift) ----------------------------

#[test]
fn oversized_commit_slides_the_viewport_down() {
    // The scenario that breaks the naive placement: the viewport starts at the
    // top and an oversized commit slides it toward the bottom of the pane.
    let mut terminal = top_launched_terminal(30, 30);
    let mut sink = HistorySink::new();
    assert_eq!(
        terminal.get_frame().area().y,
        0,
        "a top-launched viewport starts at row 0",
    );

    commit_block(&mut terminal, &mut sink, 40);

    let after = terminal.get_frame().area().y;
    assert!(
        after > 0,
        "an oversized commit must slide the viewport down off row 0 (got y={after})",
    );
    // On a 30-row pane the viewport bottom-anchors at y = 30 - 11 = 19.
    assert_eq!(after, 30 - MAX_VIEWPORT_ROWS, "viewport bottom-anchors");
}

#[test]
fn bottom_box_lands_on_the_viewport_after_oversized_commit() {
    // After the viewport slides down, the offset geometry must paint the box on
    // the viewport's real rows, top/bottom border intact — not off-screen.
    let mut terminal = top_launched_terminal(40, 30);
    let mut sink = HistorySink::new();
    commit_block(&mut terminal, &mut sink, 60);

    let active = draw_bottom_ui(&mut terminal, 1);
    // The active rect must sit inside the (drifted) viewport.
    let vp = terminal.get_frame().area();
    assert!(
        active.top() >= vp.top() && active.bottom() <= vp.bottom(),
        "the box must land inside the viewport (active {active:?}, viewport {vp:?})",
    );

    let top = buffer_row(&terminal, active.top());
    let bottom = buffer_row(&terminal, active.bottom() - 1);
    assert!(
        top.starts_with('┌') && top.ends_with('┐'),
        "top border intact on the viewport after oversized commit, got {top:?}",
    );
    assert!(
        bottom.starts_with('└') && bottom.ends_with('┘'),
        "bottom border intact on the viewport after oversized commit, got {bottom:?}",
    );
}

#[test]
fn bottom_box_survives_several_successive_commits() {
    // G3: "committing several successive blocks never makes the box vanish."
    // Interleave oversized commits with a re-draw of the offset bottom box and
    // assert it lands whole on the viewport every round.
    let mut terminal = top_launched_terminal(40, 30);
    let mut sink = HistorySink::new();

    for round in 0..5u16 {
        let block: Vec<Line> = (0..25)
            .map(|i| Line::from(format!("blk-{round}-{i:02}")))
            .collect();
        sink.commit_rows(&mut terminal, block).expect("commit");

        // Grow the box one row per round, offset into the viewport, then draw.
        let active = draw_bottom_ui(&mut terminal, round + 1);
        let header = buffer_row(&terminal, active.top());
        assert!(
            header.starts_with('┌') && header.ends_with('┐'),
            "header must stay a clean top border on the viewport, round {round}, got {header:?}",
        );
    }
}

#[test]
fn commit_then_grow_keeps_the_header_clean() {
    // "commit-then-grow does not corrupt the header": commit an oversized block,
    // then draw a taller box; the top border must be whole.
    let mut terminal = bottom_anchored_terminal(40, 30);
    let mut sink = HistorySink::new();

    commit_block(&mut terminal, &mut sink, 40);
    let active = draw_bottom_ui(&mut terminal, 6); // grown input body
    let header = buffer_row(&terminal, active.top());
    assert!(
        header.starts_with('┌') && header.ends_with('┐'),
        "commit-then-grow must leave a clean header, got {header:?}",
    );
}

// --- (c) the offset is load-bearing (fail-first) ---------------------------

#[test]
fn without_the_offset_the_drifted_viewport_is_left_blank() {
    // Fail-first guard: painting the *viewport-local* geometry (no offset) after
    // the viewport has slid down leaves the viewport's real rows blank — the
    // exact "box vanishes" bug. `offset_y` is what moves the box onto them.
    let mut terminal = top_launched_terminal(40, 30);
    let mut sink = HistorySink::new();
    commit_block(&mut terminal, &mut sink, 60);

    let area = terminal.get_frame().area();
    assert!(
        area.y > 0,
        "viewport must have drifted down for this to bite"
    );

    // Draw the box at the *un-offset* (viewport-local) geometry, as the buggy
    // code did.
    let g = bottom_area_geometry(1, true, area.width, area.height);
    let unoffset = g.active;
    terminal
        .draw(|frame| {
            frame.render_widget(Block::default().borders(Borders::ALL), unoffset);
        })
        .expect("draw un-offset box");

    // The viewport's own rows must be blank — the box landed above the viewport
    // (off its visible band) because it was never offset.
    let vp = terminal.get_frame().area();
    let mut any_border = false;
    for y in vp.top()..vp.bottom() {
        if buffer_row(&terminal, y).contains('┌') {
            any_border = true;
        }
    }
    assert!(
        !any_border,
        "without the offset the box must NOT land on the drifted viewport (this is the bug)",
    );

    // And with the offset, the box *does* land on the viewport — the fix.
    let active = draw_bottom_ui(&mut terminal, 1);
    let top = buffer_row(&terminal, active.top());
    assert!(
        top.starts_with('┌'),
        "with offset_y the box lands on the viewport, got {top:?}",
    );
}
