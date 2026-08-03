//! Resize + reflow tests for the rt runtime.
//!
//! Resize handling under the fixed-max-viewport strategy (ratatui#984 workaround
//! B) has three moving parts, each pinned here so the external validator's tmux
//! probes have a matching in-process proof:
//!
//! - **Geometry recompute (VAL-CORE-009).** A `Resize { cols, rows }` folds its
//!   whole new geometry into the tracked [`TerminalSize`], and re-laying the
//!   bottom area out against it re-anchors to the new width/height. Folding is
//!   idempotent: a same-size event reports "unchanged" so a resize storm settling
//!   where it started drives no churn.
//! - **History re-wraps to the new width (VAL-CORE-009/010).** Narrowing the
//!   terminal re-wraps subsequent history to the narrower width, and a commit
//!   that lands *right after* a backend resize (with no intervening draw) still
//!   wraps to the new width — [`HistorySink::commit_lines`] autoresizes before it
//!   reads the wrap width, so the stale width can never be used.
//! - **Height shrink clamps, scrollback untouched (VAL-CORE-011).** Shrinking the
//!   pane below the bottom area's wanted height clamps the active area to fit; a
//!   pure height change touches no scrollback.
//! - **Storm coalescing (VAL-CORE-021).** A burst of resize events folds to a
//!   single final geometry, and driven through the live [`FrameScheduler`] a burst
//!   of resize-triggered `request_frame()`s coalesces to a handful of draws, not
//!   one per event — ending at a single correct re-anchored layout.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hand_tui::rt::history::HistorySink;
use hand_tui::rt::scheduler::{FrameScheduler, MIN_FRAME_INTERVAL};
use hand_tui::rt::view::{
    MAX_INPUT_ROWS, MAX_VIEWPORT_ROWS, MIN_INPUT_ROWS, TerminalSize, bottom_area_geometry,
};
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use ratatui::{Terminal, TerminalOptions, Viewport};

// ============================================================================
// TerminalSize fold — pure resize-event -> tracked-size -> geometry (VAL-CORE-009)
// ============================================================================

#[test]
fn apply_resize_overwrites_and_reports_change() {
    let mut size = TerminalSize::new(100, 30);

    // A different size overwrites and reports the change.
    assert!(size.apply_resize(40, 30), "a width change is a change");
    assert_eq!(size, TerminalSize::new(40, 30));

    // The same size again is a no-op that reports no change — the coalescing
    // hook that lets a storm settling at a stable size skip redundant reflow.
    assert!(
        !size.apply_resize(40, 30),
        "re-applying the same size reports no change",
    );
    assert_eq!(size, TerminalSize::new(40, 30));

    // A height-only change still counts.
    assert!(size.apply_resize(40, 10), "a height change is a change");
    assert_eq!(size, TerminalSize::new(40, 10));
}

#[test]
fn resize_storm_folds_to_the_single_final_size() {
    // A storm of resize events — the exact stream a fast drag produces — folds
    // one after another into the tracked size; only the *last* survives, and
    // every same-size repeat along the way is a reported no-op. This is the pure
    // core of "a storm ends at one correct layout": geometry is derived from the
    // final size alone, never from any intermediate.
    let mut size = TerminalSize::new(80, 24);
    let storm = [
        (78, 24),
        (78, 24), // duplicate — no change
        (70, 22),
        (55, 20),
        (40, 18),
        (40, 18), // duplicate — no change
        (120, 40),
    ];

    let mut changes = 0;
    for (cols, rows) in storm {
        if size.apply_resize(cols, rows) {
            changes += 1;
        }
    }

    assert_eq!(changes, 5, "only the size-changing events count");
    assert_eq!(
        size,
        TerminalSize::new(120, 40),
        "the tracked size is the storm's final size, nothing intermediate",
    );
}

#[test]
fn bottom_geometry_recomputes_against_the_resized_width() {
    // Narrowing then widening the tracked size re-derives the bottom area against
    // the new width each time: the active rect spans the full (new) width and
    // stays bottom-anchored in the fixed viewport.
    let wide = TerminalSize::new(100, 40);
    let narrow = TerminalSize::new(40, 40);

    let g_wide = wide.bottom_geometry(3, true);
    let g_narrow = narrow.bottom_geometry(3, true);

    assert_eq!(g_wide.active.width, 100, "wide active spans the wide pane");
    assert_eq!(
        g_narrow.active.width, 40,
        "narrow active spans the narrow pane",
    );

    // Same input/loader, so the height layout is identical across the width
    // change — width does not perturb the vertical anchoring.
    assert_eq!(g_wide.active.height, g_narrow.active.height);
    assert_eq!(g_wide.active.y, g_narrow.active.y);
    assert_eq!(
        g_wide.viewport_height, g_narrow.viewport_height,
        "a pure width change never moves the fixed viewport",
    );

    // `bottom_geometry` is exactly `bottom_area_geometry` against the tracked
    // size — the convenience wrapper adds no drift.
    assert_eq!(g_narrow, bottom_area_geometry(3, true, 40, 40));
}

// ============================================================================
// Height shrink clamps the active area within the pane (VAL-CORE-011)
// ============================================================================

#[test]
fn height_shrink_below_wanted_clamps_active_within_the_pane() {
    // Grow to the full 8-row input with the loader on: the bottom area wants the
    // full MAX_VIEWPORT_ROWS. Then shrink the pane below that.
    let tall = TerminalSize::new(80, 40);
    let g_tall = tall.bottom_geometry(MAX_INPUT_ROWS, true);
    assert_eq!(g_tall.viewport_height, MAX_VIEWPORT_ROWS);
    assert_eq!(g_tall.active.height, MAX_VIEWPORT_ROWS);

    // A pane shorter than the wanted bottom-area height: the active area clamps to
    // the pane, never drawing past the bottom edge, and the input body keeps at
    // least its row (the caret always has a home).
    let short_rows = MAX_VIEWPORT_ROWS - 3;
    let short = TerminalSize::new(80, short_rows);
    let g_short = short.bottom_geometry(MAX_INPUT_ROWS, true);

    assert!(
        g_short.viewport_height <= short_rows,
        "viewport clamps to the short pane",
    );
    assert!(
        g_short.active.y + g_short.active.height <= g_short.viewport_height,
        "active area (y={}, h={}) stays within the clamped viewport ({})",
        g_short.active.y,
        g_short.active.height,
        g_short.viewport_height,
    );
    assert!(
        g_short.input.height >= MIN_INPUT_ROWS,
        "the input body keeps at least one row after the height clamp",
    );
    assert_eq!(
        g_short.active.width, 80,
        "the active area still spans the full width after a height shrink",
    );
}

// ============================================================================
// History re-wraps to the new width (VAL-CORE-009 / VAL-CORE-010)
// ============================================================================

/// Build the fixed-max inline viewport terminal over a `TestBackend`.
fn inline_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
    Terminal::with_options(
        TestBackend::new(width, height),
        TerminalOptions {
            viewport: Viewport::Inline(MAX_VIEWPORT_ROWS.min(height)),
        },
    )
    .expect("build inline test terminal")
}

/// Read every scrollback row (oldest first) then every visible-buffer row
/// (top-down), each row's text right-trimmed — the committed-history stream in
/// emission order for a viewport that was never itself drawn into.
fn committed_stream(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let backend = terminal.backend();
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
    read(backend.scrollback(), &mut out);
    read(backend.buffer(), &mut out);
    out
}

#[test]
fn commit_after_narrow_resize_wraps_to_the_new_width() {
    // A wide terminal: commit a line that fits on one row at width 40.
    let mut terminal = inline_terminal(40, MAX_VIEWPORT_ROWS + 6);
    let mut sink = HistorySink::new();

    // The payload is 30 columns of 'x' — one row at width 40, but three rows once
    // the pane narrows to 12. A trailing marker lets us find the wrap boundary.
    let payload = "x".repeat(30);

    // Narrow the *backend* to width 12 with no intervening draw, then commit.
    // `commit_lines` must autoresize first and wrap to 12, not the stale 40.
    terminal.backend_mut().resize(12, MAX_VIEWPORT_ROWS + 6);
    sink.commit_lines(&mut terminal, vec![Line::from(payload.clone())])
        .expect("commit after narrow succeeds");

    let stream = committed_stream(&terminal);
    let content: Vec<&String> = stream.iter().filter(|r| !r.is_empty()).collect();

    // 30 'x' at width 12 wraps into 3 rows (12 + 12 + 6). No committed row may be
    // wider than the new width — the stale-width single row must not survive.
    for row in &content {
        assert!(
            row.chars().count() <= 12,
            "no committed row may exceed the new width 12, got {} cols: {row:?}",
            row.chars().count(),
        );
    }
    let x_rows: Vec<&&String> = content
        .iter()
        .filter(|r| r.chars().all(|c| c == 'x'))
        .collect();
    assert_eq!(
        x_rows.len(),
        3,
        "30 'x' wraps to 3 rows at width 12, got {x_rows:?}",
    );
    // Reassembling the wrapped rows recovers the original payload — nothing lost.
    let reassembled: String = x_rows.iter().map(|r| r.as_str()).collect();
    assert_eq!(reassembled, payload, "wrapped rows recover the source text");
}

#[test]
fn commit_after_widen_resize_uses_the_new_wider_width() {
    // Start narrow, widen the backend, then commit: the line that would have
    // wrapped at the narrow width now fits on a single wider row.
    let mut terminal = inline_terminal(12, MAX_VIEWPORT_ROWS + 6);
    let mut sink = HistorySink::new();

    let payload = "y".repeat(30);
    terminal.backend_mut().resize(50, MAX_VIEWPORT_ROWS + 6);
    sink.commit_lines(&mut terminal, vec![Line::from(payload.clone())])
        .expect("commit after widen succeeds");

    let stream = committed_stream(&terminal);
    let y_rows: Vec<&String> = stream
        .iter()
        .filter(|r| !r.is_empty() && r.chars().all(|c| c == 'y'))
        .collect();
    assert_eq!(
        y_rows.len(),
        1,
        "30 'y' fits on one row at the widened width 50, got {y_rows:?}",
    );
    assert_eq!(y_rows[0].chars().count(), 30);
}

#[test]
fn post_resize_commit_stays_internally_ordered_and_rewraps() {
    // Simulate mid-stream resize (VAL-CORE-010): a block committed *after* a
    // narrow re-wraps to the new width and keeps its own lines in emission order
    // — the marker line above its wrapped payload, the payload rows in sequence,
    // with nothing lost. (The ordering of a block committed *before* the resize
    // relative to one committed after is a real-terminal scrollback property the
    // tmux probe checks: `TestBackend` re-strides its fixed-width scrollback
    // buffer on a width change, which is not how a terminal's immutable scrollback
    // text behaves, so it is not asserted here.)
    let mut terminal = inline_terminal(40, MAX_VIEWPORT_ROWS + 10);
    let mut sink = HistorySink::new();

    // Narrow the backend, then commit a block whose payload overflows the new
    // width. The commit must re-wrap to the narrow width, not the stale 40.
    terminal.backend_mut().resize(10, MAX_VIEWPORT_ROWS + 10);
    let wide_payload = "z".repeat(24); // 3 rows at width 10
    sink.commit_lines(
        &mut terminal,
        vec![Line::from("MARKER"), Line::from(wide_payload.clone())],
    )
    .expect("commit after narrow");

    let stream = committed_stream(&terminal);
    let content: Vec<&String> = stream.iter().filter(|r| !r.is_empty()).collect();

    // The marker line sits above its payload's wrapped rows — internal order kept.
    let marker = content
        .iter()
        .position(|r| r.contains("MARKER"))
        .expect("marker present");
    let first_z = content
        .iter()
        .position(|r| r.chars().all(|c| c == 'z'))
        .expect("payload present");
    assert!(
        marker < first_z,
        "the marker stays above its wrapped payload"
    );

    // The post-resize payload wrapped to the new width: 24 'z' -> 3 rows of <=10,
    // and no committed row exceeds the new width.
    for row in &content {
        assert!(
            row.chars().count() <= 10,
            "no committed row may exceed the new width 10, got {row:?}",
        );
    }
    let z_rows: Vec<&&String> = content
        .iter()
        .filter(|r| r.chars().all(|c| c == 'z') && !r.is_empty())
        .collect();
    assert_eq!(z_rows.len(), 3, "24 'z' wraps to 3 rows at width 10");
    let reassembled: String = z_rows.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        reassembled, wide_payload,
        "no loss across the mid-stream resize",
    );
}

// ============================================================================
// A pure height change over the inline viewport never touches scrollback
// (VAL-CORE-011)
// ============================================================================

#[test]
fn height_shrink_alone_leaves_scrollback_untouched() {
    // Draw the bottom area, shrink the pane height (no width change, no commit),
    // redraw: the fixed viewport repaints its whole buffer, so nothing spills
    // into scrollback.
    let mut terminal = inline_terminal(30, MAX_VIEWPORT_ROWS + 4);

    let paint = |terminal: &mut Terminal<TestBackend>, rows: u16| {
        let width = terminal.get_frame().area().width;
        let g = bottom_area_geometry(MIN_INPUT_ROWS, false, width, rows);
        terminal
            .draw(|frame| {
                frame.render_widget(ratatui::widgets::Block::bordered(), g.active);
            })
            .expect("draw");
    };

    paint(&mut terminal, MAX_VIEWPORT_ROWS + 4);
    terminal.backend().assert_scrollback_empty();

    // Shrink the pane below the fixed viewport height and redraw.
    terminal
        .backend_mut()
        .resize(30, MAX_VIEWPORT_ROWS.saturating_sub(2));
    paint(&mut terminal, MAX_VIEWPORT_ROWS.saturating_sub(2));

    terminal.backend().assert_scrollback_empty();
}

// ============================================================================
// Storm coalescing through the live scheduler (VAL-CORE-021)
// ============================================================================

#[tokio::test(start_paused = true)]
async fn resize_storm_coalesces_to_a_handful_of_draws() {
    // A resize storm feeds the scheduler through the *same* `request_frame()`
    // path every other producer uses: the input loop folds each resize into the
    // tracked size and requests a frame. Firing a burst inside one frame window
    // must collapse to a single draw — the runtime never re-lays-out once per
    // resize event.
    let draws = Arc::new(AtomicUsize::new(0));
    let counter = draws.clone();
    let (requester, handle) = FrameScheduler::spawn(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    // A tracked size the "input loop" folds each resize into, exactly as the demo
    // does — proving the storm updates state cheaply and only requests frames.
    let mut size = TerminalSize::new(100, 40);
    for width in (40..=100).rev() {
        // Each synthetic resize event: fold, then request a coalesced frame.
        let _ = size.apply_resize(width, 40);
        requester.request_frame();
    }

    // Let the actor drain: the first request draws; the rest coalesce.
    tokio::task::yield_now().await;
    tokio::time::sleep(MIN_FRAME_INTERVAL * 2).await;

    drop(requester);
    handle.await.unwrap().unwrap();

    let n = draws.load(Ordering::SeqCst);
    assert!(
        (1..=3).contains(&n),
        "a 61-event resize storm must coalesce to a handful of draws, got {n}",
    );
    // The storm settled at the final width — one correct layout, not 61.
    assert_eq!(size, TerminalSize::new(40, 40));
}
