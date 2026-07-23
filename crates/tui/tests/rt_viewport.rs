//! Unit tests for the rt bottom-area viewport geometry (`hand_tui::rt::view`).
//!
//! Strategy B for ratatui#984: the inline viewport is fixed at its maximum
//! height and the bottom area (loader + auto-growing input) is laid out inside
//! it. These tests pin the pure geometry decisions the external validator's
//! probes ultimately rely on (VAL-CORE-007/008/020/035):
//!
//! - the input body grows one row at a time from 1 up to 8 and shrinks back,
//!   while the *fixed* viewport height never changes (so a grow can never eat
//!   history and a shrink never has to move the viewport) — VAL-CORE-007/035;
//! - loading and unloading the loader row only re-partitions the interior; the
//!   freed rows are outside the active area and repaint blank, leaving no ghost
//!   — VAL-CORE-008;
//! - on a 40×10 pane the active bottom area is fully within the viewport and the
//!   viewport within the terminal, with no stair-step past the bottom edge —
//!   VAL-CORE-020.
//!
//! The final group drives the fixed viewport over ratatui's `TestBackend` to
//! confirm, end-to-end, that a grow-then-shrink cycle leaves no border/spinner
//! ghost in the viewport buffer and touches no scrollback.

use hand_tui::rt::view::{
    BORDER_ROWS, LOADER_ROWS, MAX_INPUT_ROWS, MAX_VIEWPORT_ROWS, MIN_INPUT_ROWS,
    bottom_area_geometry, clamp_input_rows,
};
use ratatui::backend::TestBackend;
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// A generous terminal height so the fixed viewport is never clamped by the pane.
const TALL: u16 = 40;
/// Full inline width used for the geometry cases.
const WIDTH: u16 = 80;

// --- input auto-grow clamp --------------------------------------------------

#[test]
fn input_rows_clamp_into_one_to_eight() {
    // Zero and one both floor at the single always-visible row.
    assert_eq!(clamp_input_rows(0), MIN_INPUT_ROWS);
    assert_eq!(clamp_input_rows(1), 1);
    // Mid-range passes through untouched.
    assert_eq!(clamp_input_rows(4), 4);
    // The ceiling holds: growing past 8 rows does not enlarge the box.
    assert_eq!(clamp_input_rows(MAX_INPUT_ROWS), MAX_INPUT_ROWS);
    assert_eq!(clamp_input_rows(MAX_INPUT_ROWS + 5), MAX_INPUT_ROWS);
}

#[test]
fn viewport_height_is_fixed_across_the_full_grow_range() {
    // The whole point of strategy B: as the input grows 1 -> 8 (with the loader
    // showing), the viewport height reported for the terminal never changes, so a
    // grow never has to enlarge the viewport (which would eat history) and a
    // shrink never has to move it.
    let mut heights = Vec::new();
    for rows in MIN_INPUT_ROWS..=MAX_INPUT_ROWS {
        let g = bottom_area_geometry(rows, true, WIDTH, TALL);
        heights.push(g.viewport_height);
    }
    assert!(
        heights.iter().all(|&h| h == MAX_VIEWPORT_ROWS),
        "viewport height must stay fixed at {MAX_VIEWPORT_ROWS}, got {heights:?}",
    );
    assert_eq!(MAX_VIEWPORT_ROWS, BORDER_ROWS + LOADER_ROWS + MAX_INPUT_ROWS);
}

#[test]
fn active_area_grows_and_shrinks_with_the_input() {
    // The active (painted) area height tracks the input row count exactly, one
    // row per grow, and comes back down on shrink. Border + loader are constant
    // overhead on top.
    let overhead = BORDER_ROWS + LOADER_ROWS;
    for rows in MIN_INPUT_ROWS..=MAX_INPUT_ROWS {
        let g = bottom_area_geometry(rows, true, WIDTH, TALL);
        assert_eq!(
            g.active.height,
            overhead + rows,
            "active height must be border+loader+input for {rows} input rows",
        );
        // The active area is always within the fixed viewport.
        assert!(
            g.active.y + g.active.height <= g.viewport_height,
            "active area must fit inside the fixed viewport",
        );
    }

    // A shrink from the max back to a single row: active height drops by exactly
    // the seven rows the input gave up.
    let grown = bottom_area_geometry(MAX_INPUT_ROWS, true, WIDTH, TALL);
    let collapsed = bottom_area_geometry(MIN_INPUT_ROWS, true, WIDTH, TALL);
    assert_eq!(
        grown.active.height - collapsed.active.height,
        MAX_INPUT_ROWS - MIN_INPUT_ROWS,
    );
    // Both anchor to the bottom of the same fixed viewport, so the shrink leaves
    // a blank band *above* the box rather than moving the box.
    assert_eq!(
        grown.active.y + grown.active.height,
        collapsed.active.y + collapsed.active.height,
        "the active area stays bottom-anchored across a shrink",
    );
    assert!(
        collapsed.active.y > grown.active.y,
        "the collapsed box sits lower, freeing rows above it",
    );
}

// --- loader load / unload ---------------------------------------------------

#[test]
fn loader_partitions_interior_and_unload_reclaims_its_row() {
    let with_loader = bottom_area_geometry(3, true, WIDTH, TALL);
    let no_loader = bottom_area_geometry(3, false, WIDTH, TALL);

    // With the loader: a loader row is carved out above the input body.
    let loader = with_loader.loader.expect("loader row present when visible");
    assert_eq!(loader.height, LOADER_ROWS);
    assert_eq!(
        loader.y + LOADER_ROWS,
        with_loader.input.y,
        "input body sits directly below the loader row",
    );

    // Without the loader: no loader rect, and the active area is exactly one row
    // shorter — the loader's row is reclaimed, not left as a ghost.
    assert!(no_loader.loader.is_none(), "no loader rect when hidden");
    assert_eq!(
        with_loader.active.height - no_loader.active.height,
        LOADER_ROWS,
        "unloading the loader shrinks the active area by its single row",
    );

    // The viewport height is unchanged across load/unload.
    assert_eq!(with_loader.viewport_height, no_loader.viewport_height);
}

#[test]
fn input_body_never_collapses_below_one_row() {
    // Even with the loader showing and only one input row, the input body keeps
    // at least its single row and the loader still fits.
    let g = bottom_area_geometry(MIN_INPUT_ROWS, true, WIDTH, TALL);
    assert!(g.loader.is_some());
    assert!(g.input.height >= MIN_INPUT_ROWS, "input keeps its row");
}

// --- tiny-terminal clamp (40x10, VAL-CORE-020) ------------------------------

#[test]
fn tiny_pane_clamps_active_area_within_bounds_no_stair_step() {
    // A 40x10 pane cannot hold the full 11-row max viewport. The viewport height
    // clamps to the pane height, and the active area is clamped within it: the
    // bottom UI stays entirely in bounds with no row drawn past the bottom edge.
    let width = 40;
    let height = 10;

    for rows in MIN_INPUT_ROWS..=MAX_INPUT_ROWS {
        for loader in [true, false] {
            let g = bottom_area_geometry(rows, loader, width, height);
            assert!(
                g.viewport_height <= height,
                "viewport ({}) must not exceed the {height}-row pane",
                g.viewport_height,
            );
            assert!(
                g.active.y + g.active.height <= g.viewport_height,
                "active area (y={}, h={}) must fit within the viewport ({}) — no stair-step",
                g.active.y,
                g.active.height,
                g.viewport_height,
            );
            assert!(
                g.active.width == width,
                "active area spans the full pane width",
            );
            // The input body stays within the active area's interior.
            assert!(
                g.input.y >= g.active.y + 1
                    && g.input.y + g.input.height <= g.active.y + g.active.height,
                "input body must sit inside the border",
            );
        }
    }
}

#[test]
fn tiny_pane_drops_loader_before_it_would_overflow() {
    // On a pane so short the interior can hold only the input row, the loader is
    // dropped rather than pushing the input out of bounds — the caret always has
    // a home. A 4-row pane leaves a 2-row interior (border eats 2); with a loader
    // requested, the input still keeps a row.
    let g = bottom_area_geometry(3, true, 40, 4);
    assert!(g.viewport_height <= 4);
    assert!(
        g.active.y + g.active.height <= g.viewport_height,
        "active area fits the 4-row pane",
    );
    assert!(
        g.input.height >= 1,
        "the input body keeps at least one row even when the loader is squeezed out",
    );
}

// --- end-to-end over TestBackend: no ghost across grow -> shrink ------------
//
// Drive the *fixed* inline viewport (built at MAX_VIEWPORT_ROWS) through a paint
// that grows to 8 input rows with the loader, then a paint that collapses to 1
// input row with the loader gone. Because every draw repaints the whole fixed
// buffer, the collapsed frame must leave the freed rows blank — no border,
// spinner, or stale text ghost — and the scrollback must stay untouched.

/// Build the fixed-max inline viewport terminal over a `TestBackend`.
fn fixed_viewport_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(MAX_VIEWPORT_ROWS),
        },
    )
    .expect("build fixed-max inline test terminal")
}

/// Paint the bottom area for one frame: a bordered block over the active rect, a
/// spinner glyph in the loader row, and marker text in the input body. Rows
/// outside the active rect are left as the frame's cleared background.
fn paint(terminal: &mut Terminal<TestBackend>, input_rows: u16, loader: bool, marker: &str) {
    let width = terminal.get_frame().area().width;
    let height = terminal.get_frame().area().height;
    let g = bottom_area_geometry(input_rows, loader, width, height);
    let marker = marker.to_string();
    // Inset one column so the marker sits inside the left border rather than
    // overwriting it.
    let input_body = ratatui::layout::Rect::new(
        g.input.x + 1,
        g.input.y,
        g.input.width.saturating_sub(2),
        g.input.height,
    );
    terminal
        .draw(|frame| {
            frame.render_widget(Block::bordered(), g.active);
            if let Some(loader_rect) = g.loader {
                frame.render_widget(Paragraph::new("SPINNER"), loader_rect);
            }
            frame.render_widget(Paragraph::new(marker.clone()), input_body);
        })
        .expect("draw the bottom area");
}

/// Collect the viewport buffer as trimmed row strings.
fn viewport_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    let area = buf.area;
    (area.y..area.y + area.height)
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

#[test]
fn grow_then_shrink_leaves_no_ghost_in_viewport_or_scrollback() {
    let mut terminal = fixed_viewport_terminal(30, MAX_VIEWPORT_ROWS + 4);

    // Grow to the full 8-row input with the loader showing.
    paint(&mut terminal, MAX_INPUT_ROWS, true, "GROWN");
    let grown = viewport_rows(&terminal);
    assert!(
        grown.iter().any(|r| r.contains("SPINNER")),
        "the spinner is painted while the loader shows",
    );
    assert!(
        grown.iter().any(|r| r.contains("GROWN")),
        "the input marker is painted",
    );

    // Collapse to a single input row with the loader gone.
    paint(&mut terminal, MIN_INPUT_ROWS, false, "SMALL");
    let shrunk = viewport_rows(&terminal);

    // No spinner ghost, no stale "GROWN" text, and the border of the tall box is
    // gone from the rows it used to occupy — every one of those is a shrink ghost
    // the fixed-viewport repaint must have cleared.
    assert!(
        !shrunk.iter().any(|r| r.contains("SPINNER")),
        "no spinner ghost after the loader unloads: {shrunk:?}",
    );
    assert!(
        !shrunk.iter().any(|r| r.contains("GROWN")),
        "no stale input text after the shrink: {shrunk:?}",
    );
    assert!(
        shrunk.iter().any(|r| r.contains("SMALL")),
        "the collapsed input paints its new marker: {shrunk:?}",
    );

    // The top rows the grown box occupied are now blank (the box shrank and
    // bottom-anchored, freeing the rows above it).
    assert_eq!(
        shrunk[0], "",
        "the row the grown border's top used must repaint blank, got {:?}",
        shrunk[0],
    );

    // A pure height change must never touch scrollback.
    terminal.backend().assert_scrollback_empty();
}
