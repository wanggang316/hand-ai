//! Bottom-area rendering and the frame scheduler for the rt interactive driver.
//!
//! The frame scheduler owns the terminal and is the single place the UI paints
//! (wrapped in synchronized-output markers). The draw closure:
//!
//! 1. drains finalized chat blocks into native scrollback via the
//!    [`HistorySink`] — the `insert_before`-between-draws ordering — *before* it
//!    redraws the viewport;
//! 2. wipes the old-width viewport on a backend size change so a resize never
//!    spills a stale-width fragment into scrollback (M1 resize invariant);
//! 3. lays the fixed-max inline bottom area out with
//!    [`bottom_area_geometry`]`.offset_y(frame.area().y)` (M1 FIX-2: the viewport
//!    origin drifts down as scrollback fills), then renders the bordered box, the
//!    optional working-loader row (M2 [`Loader`]), the editor (borderless — the
//!    box is the driver's), and the two-line footer view-model;
//! 4. drives the hardware cursor from the editor's reported caret.
//!
//! This mirrors the rt demo's scheduler/draw split exactly; the only differences
//! are the concrete components (M2 [`Editor`] + [`Loader`] + the footer view-model
//! instead of the demo's input row + status block) and the chat commits coming
//! from the agent driver rather than a synthetic stream.

use std::io;
use std::sync::{Arc, Mutex};

use hand_tui::rt::components::{DEFAULT_SPINNER_FRAMES, Editor, Loader};
use hand_tui::rt::history::{HistorySink, wrap_lines};
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionTerminal, clear_viewport_region};
use hand_tui::rt::view::{RtComponent, bottom_area_geometry};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::footer::{FooterViewModel, render_footer_lines};
use super::overlay::SharedOverlay;
use super::state::{DriverState, SharedEditor, SharedFooter, lock_editor, lock_footer, lock_state};
use crate::modes::interactive::theme::ThemePalette;

/// The static message the working-loader shows while a turn streams.
const LOADER_MESSAGE: &str = "Working…";

/// Rows the footer view-model occupies inside the bottom box (the cwd line + the
/// stats line). Reserved from the input body so the editor keeps its full desired
/// height above the footer.
const FOOTER_ROWS: u16 = 2;

/// Spawn the frame scheduler over the session terminal.
///
/// The returned closure is the one and only painter. The terminal is wrapped in
/// [`EraseOnDrop`] so, when the scheduler task ends (all requesters dropped on
/// quit, or a panic unwinding through it), the inline viewport region is wiped
/// *before* the guard restores — the shell prompt lands on a fresh line below the
/// transcript with no ghost bottom-UI box, deterministically and without relying
/// on a final scheduler frame.
pub fn spawn_scheduler(
    terminal: SessionTerminal,
    state: Arc<Mutex<DriverState>>,
    editor: SharedEditor,
    footer: SharedFooter,
    overlay: SharedOverlay,
) -> (FrameRequester, tokio::task::JoinHandle<io::Result<()>>) {
    let mut terminal = EraseOnDrop::new(terminal);
    let mut history = HistorySink::new();
    // The backend size the last draw painted against, so a resize is detected in
    // the draw path itself (independent of when the crossterm Resize event
    // reaches the input loop) and the old-width viewport is erased before
    // anything autoresizes and spills.
    let mut last_size: Option<ratatui::layout::Size> = None;

    // The working-loader shown in the bordered slot while a turn streams. Owned by
    // the draw closure (not shared): it is toggled active/idle from the streaming
    // flag and ticked once per frame while streaming, so the spinner animates and
    // the static "Working…" message is present only mid-turn. When the turn ends
    // the geometry drops the loader row entirely and the fixed-viewport blank
    // repaint wipes it — no ghost "Working…" or border fragment left in the
    // active area or in scrollback (M1 shrink-erase invariant).
    let mut loader = Loader::new(LOADER_MESSAGE).frames(
        DEFAULT_SPINNER_FRAMES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );

    FrameScheduler::spawn(move || {
        // Snapshot state under the lock, then release it before touching the
        // terminal. Finished chat blocks and raw sequences are taken out (not
        // cloned) so they can only ever be committed / written once.
        let (snapshot, commits, raw) = {
            let mut guard = lock_state(&state);
            let commits = guard.take_commits();
            let raw = guard.take_raw();
            let preview = guard.streaming_preview.clone().unwrap_or_default();
            // Snapshot the mounted selector's render lines (Send `Vec<Line>`) so the
            // draw closure can paint the overlay without holding the (?Send) M1
            // stack across the task boundary — the rt-demo pattern. Measured against
            // the overlay interior width for the current viewport.
            let palette = guard.palette();
            let overlay_lines =
                overlay.render_lines(overlay_interior_width(guard.size.cols), &palette);
            let snapshot = StateSnapshot {
                size: guard.size,
                loader: guard.streaming,
                loader_message: guard.loader_message.clone(),
                preview,
                // A modal selector owns focus; the editor caret is hidden while an
                // overlay is up so the hardware cursor does not blink under the
                // dimmed dialog.
                overlay_open: overlay_lines.is_some(),
                overlay_lines,
                palette,
            };
            (snapshot, commits, raw)
        };

        // Drive the working-loader from the streaming flag: active only mid-turn,
        // ticked once per frame while active so the spinner animates. An idle
        // loader paints nothing, and the geometry drops its row entirely — the
        // dismissal leaves no residue. The message is overridable so a
        // long-running operation (`/compact`) can name itself.
        loader.set_active(snapshot.loader);
        loader.set_message(
            snapshot
                .loader_message
                .clone()
                .unwrap_or_else(|| LOADER_MESSAGE.to_string()),
        );
        if snapshot.loader {
            loader.tick();
        }

        // Resize erase: detect a backend size change and wipe the old-width
        // viewport *before* any autoresize scrolls its stale cells into
        // scrollback. Best-effort — a failed wipe must not abort the frame.
        let current_size = terminal.size().ok();
        if let Some(current) = current_size
            && last_size.is_some_and(|prev| prev != current)
        {
            let _ = clear_viewport_region(&mut *terminal);
        }
        last_size = current_size;

        // Commit finished chat blocks into native scrollback BEFORE the draw.
        // Each block is one `insert_before`; the sink autoresizes then pre-wraps
        // to the current width, so a block committed right after a resize wraps
        // to the new width and a tall block lands complete and ordered.
        for block in commits {
            history.commit_lines(&mut *terminal, block)?;
        }

        let footer_view = lock_footer(&footer).clone();

        // Wrap the whole paint in BSU/ESU: the closing `?2026l` is emitted even
        // if `terminal.draw` errors, so an interrupt mid-draw never leaves an
        // open synchronized block.
        let mut stdout = io::stdout();
        let editor = &editor;
        let loader = &loader;
        draw_synchronized(&mut stdout, |w| {
            terminal.draw(|frame| {
                draw(frame, &snapshot, editor, loader, &footer_view);
                // Layer the mounted selector over the base viewport, full-frame, so a
                // centered modal dialog dims the whole transcript + bottom UI beneath
                // it. Built as a throwaway local M1 OverlayStack each frame (the
                // ?Send stack never crosses the task boundary) so the dim + border +
                // clear + anchor placement is pixel-identical to the M1 contract. The
                // whole viewport repaints each frame, so closing the overlay leaves no
                // dim residue or ghost border (VAL-OVERLAY-001 / -008).
                if let Some(lines) = snapshot.overlay_lines.clone() {
                    let area = frame.area();
                    draw_overlay(frame.buffer_mut(), area, lines);
                }
            })?;
            // Flush any raw terminal control sequences (OSC 133 prompt marks,
            // OSC 9;4 progress) AFTER the viewport draw but INSIDE the sync
            // block, on this terminal-owning task — the same raw-emission
            // discipline the M2 image / OSC 8 channel uses. These are
            // terminal-global escapes (not cell content), so they are written
            // between a cursor save/restore so they never disturb the caret
            // ratatui just positioned.
            flush_raw(w, &raw)?;
            Ok(())
        })
    })
}

/// Save-cursor escape (`ESC 7`) — parks the caret before a raw OSC write.
const SAVE_CURSOR: &[u8] = b"\x1b7";
/// Restore-cursor escape (`ESC 8`) — returns the caret after a raw OSC write.
const RESTORE_CURSOR: &[u8] = b"\x1b8";

/// Write queued raw control sequences to the terminal, bracketed by a cursor
/// save/restore so the escapes never disturb the caret ratatui positioned.
///
/// A no-op for an empty queue: it emits nothing, not even the save/restore, so a
/// steady-state repaint pays no bytes. The OSC 133 / OSC 9;4 escapes are
/// terminal-global (they carry no position), so this is a plain sequential write
/// rather than the row-addressed flush the image channel needs.
fn flush_raw<W: io::Write>(out: &mut W, raw: &[&'static str]) -> io::Result<()> {
    if raw.is_empty() {
        return Ok(());
    }
    out.write_all(SAVE_CURSOR)?;
    for sequence in raw {
        out.write_all(sequence.as_bytes())?;
    }
    out.write_all(RESTORE_CURSOR)?;
    Ok(())
}

/// An immutable per-frame view of the state the draw path reads.
struct StateSnapshot {
    /// The tracked terminal geometry this frame lays out against.
    size: hand_tui::rt::view::TerminalSize,
    /// Whether the loader row shows (streaming turn in flight). Drives both the
    /// geometry (whether a loader row is reserved) and the loader's active state.
    loader: bool,
    /// An override for the loader message this frame, or `None` for the default
    /// `Working…`. Set by a long-running operation (`/compact`) so the loader
    /// names it.
    loader_message: Option<String>,
    /// The live streaming-preview lines to paint in the blank band above the
    /// active bottom box, or empty when no turn is streaming. The tail is shown
    /// when the band is shorter than the preview so the most recent tokens are
    /// always visible.
    preview: Vec<Line<'static>>,
    /// Whether a modal overlay (a selector) is currently mounted. While it is, the
    /// editor caret is suppressed so the hardware cursor is not stranded under the
    /// dimmed dialog.
    overlay_open: bool,
    /// The mounted selector's interior render lines this frame, or `None` when no
    /// overlay is open. Captured as a `Send` `Vec<Line>` so the draw closure paints
    /// the overlay without holding the `?Send` M1 stack across the task boundary.
    overlay_lines: Option<Vec<Line<'static>>>,
    /// The active theme palette this frame, so the footer's context-percentage
    /// segment colours from the theme (the default palette keeps the historical
    /// red/yellow thresholds).
    palette: ThemePalette,
}

/// Paint one frame: the bordered bottom box, the optional working-loader row, the
/// editor, and the two-line footer — laid out inside the fixed inline viewport.
fn draw(
    frame: &mut ratatui::Frame,
    state: &StateSnapshot,
    editor: &SharedEditor,
    loader: &Loader,
    footer: &FooterViewModel,
) {
    let area = frame.area();

    // The editor's desired interior rows drive the auto-grow. Measure it against
    // the interior width the box will give it (2 border columns), clamped to the
    // 1..=8 input-row band by the geometry helper.
    let editor_rows = {
        let ed = lock_editor(editor);
        // desired_height on a borderless editor returns interior rows only; probe
        // against a representative interior rect (border insets applied below).
        let probe = Rect::new(
            0,
            0,
            area.width.saturating_sub(2).max(1),
            area.height.max(1),
        );
        ed.desired_height(probe).max(1)
    };

    // The geometry's input body must hold the editor's desired rows *plus* the
    // footer rows, so the footer sits below a full-height editor rather than
    // stealing from it. (The geometry clamps the total to the 1..=8 band and to
    // the pane height, so a tiny pane trims the footer/editor gracefully.)
    let input_rows = editor_rows.saturating_add(FOOTER_ROWS);

    // Lay the fixed-max bottom area out inside the viewport, then translate every
    // rect down by `area.y` so the bottom UI paints at the viewport's real rows —
    // not absolute row 0, which drifts off-screen once `insert_before` moves the
    // viewport down (the "box vanishes after a big block" bug). This is the M1
    // FIX-2 offset_y invariant.
    let geometry = bottom_area_geometry(input_rows, state.loader, area.width, state.size.rows)
        .offset_y(area.y);

    // The live streaming preview paints in the blank band ABOVE the active box
    // (between the viewport origin and the box's top), so the in-flight assistant
    // partial grows in place without touching scrollback. When the preview is
    // taller than the band, its tail is shown so the newest tokens stay visible.
    draw_stream_preview(frame, &state.preview, area, geometry.active);

    // The bordered box occupies only the active area; rows above it (freed by a
    // collapse) stay blank and repaint clear each frame. The border colours from
    // the active palette (the default palette keeps the historical dark grey).
    let block = Block::bordered().border_style(Style::default().fg(state.palette.border));
    frame.render_widget(block, geometry.active);

    // The working-loader row, when streaming, sits just below the top border,
    // inside the box. The M2 Loader paints nothing when inactive, and the
    // geometry reserves this row only while streaming — so the dismissal at
    // AgentEnd leaves no "Working…" or spinner residue.
    if let Some(loader_rect) = geometry.loader {
        loader.render(inset(loader_rect), frame.buffer_mut());
    }

    // Split the input interior into the editor rows (the top) and the footer rows
    // (the bottom `FOOTER_ROWS`).
    let (editor_rect, footer_rect) = split_editor_footer(inset(geometry.input));

    // Render the editor and drive the hardware cursor from its reported caret.
    // When a modal overlay is open it owns focus, so the editor caret is suppressed
    // — the hardware cursor is not placed under the dimmed dialog.
    //
    // The editor self-renders its `@`/`/` suggestion popup in the band *below* its
    // box, but the driver's inline geometry gives the editor a rect sized to
    // exactly the box height — so that band is zero rows and the popup never
    // paints. The driver owns the surrounding box, so it paints the popup itself,
    // in a reserved band *above* the box (space below is the viewport's bottom
    // edge; above it is the blank band the box is anchored over). This keeps the
    // popup clear of the footer (inside the box) and of scrollback (below it).
    {
        let ed = lock_editor(editor);
        ed.render(editor_rect, frame.buffer_mut());
        draw_autocomplete_popup(frame.buffer_mut(), &ed, area, geometry.active);
        // Disambiguate: the `RtComponent::cursor` (viewport-local `Option<Position>`)
        // over the inherent `Editor::cursor` (the `(line, col)` accessor). The
        // component caret is already anchored at `editor_rect` (its render area).
        if !state.overlay_open
            && let Some(caret) = RtComponent::cursor(&*ed)
        {
            frame.set_cursor_position(caret);
        }
    }

    // The two-line footer view-model, rendered into the reserved footer rows.
    // `Paragraph` clips a line wider than the rect, so a narrow pane never spills.
    if footer_rect.height > 0 {
        let lines = render_footer_lines(footer, footer_rect.width, &state.palette);
        Paragraph::new(Text::from(lines)).render(footer_rect, frame.buffer_mut());
    }
}

/// The interior width an overlay dialog gives its content, for the current
/// viewport columns: a centered dialog spans ~60% of the width (bounded to a sane
/// minimum), minus the two border columns. The scheduler measures the mounted
/// selector's render lines against this so wrapping matches the box it paints into.
pub(crate) fn overlay_interior_width(cols: u16) -> u16 {
    dialog_outer_width(cols).saturating_sub(2).max(1)
}

/// The outer width of a centered dialog overlay for `cols` viewport columns: ~60%,
/// floored at a readable minimum, clamped to the viewport.
fn dialog_outer_width(cols: u16) -> u16 {
    let sixty = (cols as u32 * 3 / 5) as u16;
    sixty.max(40).min(cols.max(1))
}

/// Paint the mounted selector's `lines` as a centered, dimmed, bordered modal
/// dialog over the already-drawn base buffer (VAL-OVERLAY-001).
///
/// Placement reuses the M1 pure geometry
/// [`anchor_rect`](hand_tui::rt::overlay::anchor_rect): the dialog is sized to hold
/// its content (~60% wide, tall enough for the lines plus the two border rows) and
/// Center-anchored, and `anchor_rect` clamps an oversized box into the viewport — so
/// a tiny pane (40×10) keeps the border on-frame without wrapping (VAL-OVERLAY-020).
/// The dim + bordered-clear passes mirror the M1 [`OverlayStack::render`] contract
/// (dim every base cell outside the box, `Clear` the footprint, draw the border,
/// paint the content inside). A selector's list can be taller than the M1 stack's
/// fixed 7-row dialog, so the box is sized here rather than through the stack's
/// private heuristic — the only reason this does not call `OverlayStack::render`
/// directly. Because the whole viewport repaints each frame, closing the overlay
/// later leaves no dim residue or ghost border (VAL-OVERLAY-008).
pub(crate) fn draw_overlay(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    lines: Vec<Line<'static>>,
) {
    use hand_tui::rt::overlay::{OverlayAnchor, OverlayMargin, anchor_rect};
    use ratatui::layout::Size;
    use ratatui::style::Modifier;
    use ratatui::widgets::{Block, Clear};

    if area.width == 0 || area.height == 0 {
        return;
    }

    // Desired outer box: ~60% wide (min 40, clamped), tall enough for the content
    // plus the two border rows. `anchor_rect` size-clamps both to the viewport.
    let outer_w = dialog_outer_width(area.width);
    let outer_h = (lines.len() as u16).saturating_add(2).max(3);
    let rect = anchor_rect(
        Size::new(outer_w, outer_h),
        area,
        OverlayAnchor::Center,
        OverlayMargin::uniform(0),
        true,
    );

    // Dim every base cell outside the dialog so the background recedes but stays
    // legible (mirrors the M1 `dim_outside` pass).
    let dim = Style::default().add_modifier(Modifier::DIM);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let inside =
                x >= rect.left() && x < rect.right() && y >= rect.top() && y < rect.bottom();
            if !inside && let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(dim);
            }
        }
    }

    // Clear the footprint so no base content bleeds through, draw the border, and
    // paint the content into the interior. `Paragraph` clips content wider/taller
    // than the interior, so the border never overflows.
    Clear.render(rect, buf);
    let block = Block::bordered().border_style(Style::default().fg(Color::DarkGray));
    let interior = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(Text::from(lines)).render(interior, buf);
}

/// Paint the streaming preview into the blank band above the active box.
///
/// The band runs from the viewport origin (`area.y`) down to the top of the
/// active box. The preview is wrapped to the width and, when it is taller than
/// the band, its **tail** is shown so the newest tokens are always visible (the
/// preview grows upward off the top of the band, matching how a streamed reply
/// reads). An empty band or empty preview paints nothing.
fn draw_stream_preview(
    frame: &mut ratatui::Frame,
    preview: &[Line<'static>],
    area: Rect,
    active: Rect,
) {
    if preview.is_empty() {
        return;
    }
    let band_height = active.y.saturating_sub(area.y);
    if band_height == 0 || area.width == 0 {
        return;
    }
    let band = Rect::new(area.x, area.y, area.width, band_height);

    // Wrap to the band width, then keep only the last `band_height` visual rows
    // so the newest content sits flush against the box.
    let wrapped = wrap_lines(preview, band.width);
    let start = wrapped.len().saturating_sub(band_height as usize);
    let visible: Vec<Line<'static>> = wrapped[start..].to_vec();

    Paragraph::new(Text::from(visible)).render(band, frame.buffer_mut());
}

/// Paint the editor's `@`/`/` suggestion popup into a band immediately above the
/// active box.
///
/// The editor's own `render` paints the popup only in the band *below* its box,
/// which the driver's exact-height editor rect leaves empty — so the driver
/// paints it here instead, via the editor's public
/// [`autocomplete`](Editor::autocomplete) accessor. The band hangs off the top
/// border of the active box, `popup_row_count` rows tall, clamped to the space
/// between the box top and the viewport origin (`area.y`) so it never overwrites
/// scrollback above nor the box below. A closed popup (zero rows) or a band with
/// no room paints nothing.
fn draw_autocomplete_popup(
    buf: &mut ratatui::buffer::Buffer,
    editor: &Editor,
    area: Rect,
    active: Rect,
) {
    if !editor.autocomplete_visible() {
        return;
    }
    let wanted = editor.popup_row_count();
    // The blank band above the box, from the viewport origin down to the box top.
    let above = active.y.saturating_sub(area.y);
    let rows = wanted.min(above);
    if rows == 0 || active.width == 0 {
        return;
    }
    // Hang the popup off the top border of the box, growing upward: its bottom row
    // sits just above the box, so the newest completions stay flush with the input.
    let popup = Rect::new(active.x, active.y.saturating_sub(rows), active.width, rows);
    editor.autocomplete().render(popup, buf);
}

/// Split a bottom-area body rect into the editor (the top rows) and the footer
/// (the bottom [`FOOTER_ROWS`] rows).
///
/// The editor always keeps at least one row for its caret: the footer only claims
/// the rows left over above a single editor row. On a body of height `h`, the
/// footer takes `min(FOOTER_ROWS, h - 1)` rows from the bottom (0 when `h <= 1`),
/// so a tiny pane collapses the footer before it ever starves the editor.
fn split_editor_footer(body: Rect) -> (Rect, Rect) {
    let footer_rows = FOOTER_ROWS.min(body.height.saturating_sub(1));
    let editor_rows = body.height.saturating_sub(footer_rows);
    let editor = Rect::new(body.x, body.y, body.width, editor_rows);
    let footer = Rect::new(
        body.x,
        body.y.saturating_add(editor_rows),
        body.width,
        footer_rows,
    );
    (editor, footer)
}

/// Inset a rect by one column on each side so content sits inside the box's
/// left/right border rather than overwriting it. Height is left unchanged.
fn inset(rect: Rect) -> Rect {
    Rect::new(
        rect.x + 1,
        rect.y,
        rect.width.saturating_sub(2),
        rect.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_editor_footer_reserves_footer_rows_at_the_bottom() {
        let body = Rect::new(1, 5, 40, 5);
        let (editor, footer) = split_editor_footer(body);
        // The bottom FOOTER_ROWS go to the footer; the rest to the editor.
        assert_eq!(editor, Rect::new(1, 5, 40, 3));
        assert_eq!(footer, Rect::new(1, 8, 40, FOOTER_ROWS));
    }

    #[test]
    fn split_editor_footer_single_row_gives_editor_all_and_empty_footer() {
        let body = Rect::new(1, 5, 40, 1);
        let (editor, footer) = split_editor_footer(body);
        assert_eq!(editor, body, "editor keeps its one row");
        assert_eq!(
            footer.height, 0,
            "footer collapses before starving the editor"
        );
    }

    #[test]
    fn split_editor_footer_two_rows_leaves_editor_one_and_footer_one() {
        // With only two rows, the editor keeps one and the footer gets the other,
        // never taking both FOOTER_ROWS at the editor's expense.
        let body = Rect::new(1, 5, 40, 2);
        let (editor, footer) = split_editor_footer(body);
        assert_eq!(editor.height, 1, "editor never starved below one row");
        assert_eq!(footer.height, 1);
    }

    #[test]
    fn inset_trims_two_columns_and_keeps_height() {
        let r = Rect::new(0, 3, 20, 2);
        assert_eq!(inset(r), Rect::new(1, 3, 18, 2));
    }

    #[test]
    fn inset_narrow_rect_saturates_to_zero_width() {
        let r = Rect::new(0, 0, 1, 1);
        assert_eq!(inset(r).width, 0);
    }

    // --- Loader slot lifecycle (VAL-CHAT-003 / VAL-COMPAT-008) --------------

    use hand_tui::rt::components::Editor;
    use hand_tui::rt::view::{MAX_VIEWPORT_ROWS, TerminalSize};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::{TerminalOptions, Viewport};

    /// Build the fixed-max inline viewport terminal over a `TestBackend`, matching
    /// the runtime's `Viewport::Inline(MAX_VIEWPORT_ROWS)`.
    fn fixed_viewport(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::with_options(
            TestBackend::new(width, height),
            TerminalOptions {
                viewport: Viewport::Inline(MAX_VIEWPORT_ROWS),
            },
        )
        .expect("build fixed-max inline test terminal")
    }

    /// Every non-blank cell of the current viewport buffer, joined into one string
    /// so a residue check is a simple `contains`.
    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    /// Drive one real `draw` frame with the given streaming flag and loader state.
    fn draw_frame(
        terminal: &mut Terminal<TestBackend>,
        editor: &SharedEditor,
        loader: &Loader,
        streaming: bool,
    ) {
        let width = terminal.get_frame().area().width;
        let height = terminal.get_frame().area().height;
        let snapshot = StateSnapshot {
            size: TerminalSize::new(width, height),
            loader: streaming,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: false,
            overlay_lines: None,
            palette: ThemePalette::default(),
        };
        let footer = FooterViewModel::default();
        terminal
            .draw(|frame| draw(frame, &snapshot, editor, loader, &footer))
            .expect("draw one frame");
    }

    /// Type `s` into a shared editor, one char per keystroke, so the autocomplete
    /// popup refreshes off the caret context exactly as it does under real input.
    fn type_into(editor: &SharedEditor, s: &str) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hand_tui::rt::events::RtKey;
        let mut ed = lock_editor(editor);
        for c in s.chars() {
            ed.handle_key(&RtKey {
                key_id: Some(c.to_string()),
                raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            });
        }
    }

    #[test]
    fn autocomplete_popup_paints_above_the_box_when_visible() {
        use hand_tui::rt::components::{EditorBorder, PathEntry, PathProvider};
        use std::sync::Arc as StdArc;

        // A data-injected `@`-mention provider (no filesystem) so the popup opens
        // deterministically on `@RE`.
        let provider = StdArc::new(PathProvider::new(vec![
            PathEntry::file("README.md"),
            PathEntry::file("main.rs"),
        ]));
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new()
                .border(EditorBorder::None)
                .with_autocomplete_provider(provider),
        ));
        type_into(&editor, "@RE");
        assert!(
            lock_editor(&editor).autocomplete_visible(),
            "the @-context must open the popup"
        );

        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        draw_frame(&mut terminal, &editor, &loader, false);

        // The driver reserves a band above the box and paints the candidate there —
        // the seam the editor's below-box self-render leaves empty.
        let painted = buffer_text(&terminal);
        assert!(
            painted.contains("README.md"),
            "the popup candidate must paint above the box, got:\n{painted}"
        );
    }

    #[test]
    fn no_autocomplete_popup_paints_when_closed() {
        // With no completable context, the popup is closed and the driver paints no
        // candidate band — the box + footer are the only chrome.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        lock_editor(&editor).set_text("plain text");
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        draw_frame(&mut terminal, &editor, &loader, false);
        assert!(
            !lock_editor(&editor).autocomplete_visible(),
            "no context, popup closed"
        );
    }

    #[test]
    fn loader_slot_shows_while_streaming_and_leaves_no_residue_after() {
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let mut loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);

        // While streaming, the loader is active and its static message is painted
        // inside the bordered slot.
        loader.set_active(true);
        draw_frame(&mut terminal, &editor, &loader, true);
        assert!(
            buffer_text(&terminal).contains(LOADER_MESSAGE),
            "loader message must be visible while streaming"
        );

        // After the turn ends, the loader is dismissed: it paints nothing and the
        // fixed-viewport blank repaint wipes the shrunk row — no "Working…" or
        // border fragment is left behind (the shrink-leak regression).
        loader.set_active(false);
        draw_frame(&mut terminal, &editor, &loader, false);
        let after = buffer_text(&terminal);
        assert!(
            !after.contains(LOADER_MESSAGE),
            "loader message must not linger after dismissal, got:\n{after}"
        );
    }

    #[test]
    fn footer_fields_render_in_the_active_box() {
        // The footer view-model's fields paint into the reserved footer rows so
        // they are visible from the first frame, before any turn.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(80, MAX_VIEWPORT_ROWS);

        let width = terminal.get_frame().area().width;
        let height = terminal.get_frame().area().height;
        let snapshot = StateSnapshot {
            size: TerminalSize::new(width, height),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: false,
            overlay_lines: None,
            palette: ThemePalette::default(),
        };
        let footer = FooterViewModel {
            cwd: "/tmp/proj".to_string(),
            git_branch: Some("tmp".to_string()),
            model_id: "test-model".to_string(),
            context_window: 100_000,
            context_percent: Some(1.0),
            ..FooterViewModel::default()
        };
        terminal
            .draw(|frame| draw(frame, &snapshot, &editor, &loader, &footer))
            .expect("draw one frame");

        let text = buffer_text(&terminal);
        assert!(text.contains("/tmp/proj"), "cwd missing: {text}");
        assert!(text.contains("(tmp)"), "branch missing: {text}");
        assert!(text.contains("test-model"), "model id missing: {text}");
    }

    // --- Overlay dialog rendering (VAL-OVERLAY-001 / -020) ----------------

    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;

    /// Whether any cell of `buf` inside `rect` carries a box-drawing border glyph —
    /// the crude "is there a bordered box here" probe.
    fn has_border(buf: &Buffer) -> bool {
        let area = buf.area;
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    let s = cell.symbol();
                    if s == "─" || s == "│" || s == "┌" || s == "┐" || s == "└" || s == "┘"
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn overlay_renders_a_centered_bordered_dimmed_dialog() {
        // VAL-OVERLAY-001: the mounted selector paints a bordered box, centered, with
        // the surrounding base cells dimmed and its own content crisp.
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new("."));
        let lines = vec![
            Line::from("Search: "),
            Line::from("→ claude-sonnet [anthropic]"),
            Line::from("  gpt-4o [openai]"),
        ];

        draw_overlay(&mut buf, area, lines);

        // The dialog drew a border.
        assert!(has_border(&buf), "a bordered dialog box must be painted");

        // A base cell in the far corner (outside the centered box) is dimmed; the
        // box interior is not.
        let corner = buf.cell((0, 0)).unwrap();
        assert!(
            corner.modifier.contains(Modifier::DIM),
            "cells outside the dialog are dimmed"
        );
        // The dialog content is present and crisp.
        let text: String = {
            let mut s = String::new();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if let Some(c) = buf.cell((x, y)) {
                        s.push_str(c.symbol());
                    }
                }
                s.push('\n');
            }
            s
        };
        assert!(text.contains("claude-sonnet"), "content painted: {text}");
    }

    #[test]
    fn overlay_clamps_into_a_tiny_40x10_pane_without_overflowing() {
        // VAL-OVERLAY-020: on a small 40×10 pane the box is size-clamped to the
        // viewport, so the border stays on-frame and nothing writes past the edges.
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new(" "));
        // A tall content list (more rows than the pane) forces the height clamp.
        let lines: Vec<Line<'static>> = (0..20).map(|i| Line::from(format!("model-{i}"))).collect();

        draw_overlay(&mut buf, area, lines);

        assert!(has_border(&buf), "the clamped dialog still has a border");
        // The buffer is exactly 40×10 — the render never panicked or wrote out of
        // bounds (TestBackend/Buffer would panic on an out-of-range cell write).
        assert_eq!(buf.area, area, "no overflow past the tiny pane");
    }

    #[test]
    fn overlay_on_a_zero_sized_area_is_a_silent_noop() {
        // VAL-OVERLAY-020 (0×0 dialog layer): a degenerate viewport renders nothing
        // rather than panicking.
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        draw_overlay(&mut buf, area, vec![Line::from("x")]);
        assert_eq!(buf.area.width, 0);
    }

    #[test]
    fn a_full_frame_layers_the_overlay_over_the_base_which_keeps_rendering() {
        // VAL-OVERLAY-009: an open overlay is layered over the base viewport, and the
        // base (footer, and any streaming preview) keeps rendering underneath the
        // dimmed dialog — the overlay is a draw-layer concern only.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(80, MAX_VIEWPORT_ROWS);

        let width = terminal.get_frame().area().width;
        let height = terminal.get_frame().area().height;
        let snapshot = StateSnapshot {
            size: TerminalSize::new(width, height),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: true,
            overlay_lines: Some(vec![Line::from("→ claude-sonnet [anthropic]")]),
            palette: ThemePalette::default(),
        };
        let footer = FooterViewModel {
            model_id: "base-model".to_string(),
            ..FooterViewModel::default()
        };
        terminal
            .draw(|frame| {
                draw(frame, &snapshot, &editor, &loader, &footer);
                if let Some(lines) = snapshot.overlay_lines.clone() {
                    let area = frame.area();
                    draw_overlay(frame.buffer_mut(), area, lines);
                }
            })
            .expect("draw one frame");

        let text = buffer_text(&terminal);
        // The overlay content is on top.
        assert!(text.contains("claude-sonnet"), "overlay content: {text}");
        // The base footer still rendered underneath (it was drawn before the overlay
        // and only dimmed, not erased).
        assert!(text.contains("base-model"), "base keeps rendering: {text}");
    }

    // --- Active-area box geometry (M3 polish: box border alignment) --------

    /// The columns of the left and right box edges on a given buffer row: any
    /// cell carrying a vertical-bar or a corner glyph. A well-formed bordered box
    /// row has exactly two — the left and right border columns.
    fn edge_cols(terminal: &Terminal<TestBackend>, y: u16) -> Vec<u16> {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut cols = Vec::new();
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, y))
                && matches!(cell.symbol(), "│" | "┌" | "┐" | "└" | "┘")
            {
                cols.push(x);
            }
        }
        cols
    }

    /// The top and bottom border rows of the active box: the first and last rows
    /// that carry a horizontal-bar or corner glyph.
    fn border_rows(terminal: &Terminal<TestBackend>) -> (u16, u16) {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut top = None;
        let mut bottom = None;
        for y in area.y..area.y + area.height {
            let mut has_h = false;
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y))
                    && matches!(cell.symbol(), "─" | "┌" | "┐" | "└" | "┘")
                {
                    has_h = true;
                    break;
                }
            }
            if has_h {
                if top.is_none() {
                    top = Some(y);
                }
                bottom = Some(y);
            }
        }
        (
            top.expect("a top border row"),
            bottom.expect("a bottom border row"),
        )
    }

    /// The M3 polish invariant: the active-area box is a true rectangle — its top
    /// and bottom borders span exactly the same columns as the left/right side
    /// borders on every interior row. A regression where the top border is drawn
    /// a few columns wider than the content rows (or vice versa) leaves the box
    /// corners misaligned; this pins the alignment across widths, even/odd column
    /// counts, the tiny 40-col pane, and both the idle and streaming states so the
    /// loader row never widens or narrows the box either.
    #[test]
    fn active_box_border_is_a_true_rectangle_across_widths_and_states() {
        use hand_tui::rt::components::EditorBorder;

        for &width in &[40u16, 41, 60, 80, 81, 100] {
            for &streaming in &[false, true] {
                let editor: SharedEditor =
                    Arc::new(Mutex::new(Editor::new().border(EditorBorder::None)));
                {
                    let mut ed = lock_editor(&editor);
                    // Long enough to wrap and fully fill the interior rows, so the
                    // editor's own painting would expose any content/border overlap.
                    ed.set_text(
                        "the quick brown fox jumps over the lazy dog while the box border stays square",
                    );
                }
                let mut loader = Loader::new(LOADER_MESSAGE);
                loader.set_active(streaming);
                let mut terminal = fixed_viewport(width, MAX_VIEWPORT_ROWS);
                let height = terminal.get_frame().area().height;
                let snapshot = StateSnapshot {
                    size: TerminalSize::new(width, height),
                    loader: streaming,
                    loader_message: None,
                    preview: Vec::new(),
                    overlay_open: false,
                    overlay_lines: None,
                    palette: ThemePalette::default(),
                };
                let footer = FooterViewModel {
                    model_id: "test-model".to_string(),
                    ..FooterViewModel::default()
                };
                terminal
                    .draw(|frame| draw(frame, &snapshot, &editor, &loader, &footer))
                    .expect("draw one frame");

                let (top, bottom) = border_rows(&terminal);
                let top_edges = edge_cols(&terminal, top);
                assert_eq!(
                    top_edges.len(),
                    2,
                    "top border must have exactly two corners at width {width} streaming={streaming}, got {top_edges:?}",
                );
                let (left, right) = (top_edges[0], top_edges[1]);

                // Every row from the top border to the bottom border (inclusive)
                // must carry its left/right edge at exactly the same columns — the
                // box is square, its top border no wider than its content rows.
                for y in top..=bottom {
                    let edges = edge_cols(&terminal, y);
                    assert_eq!(
                        edges,
                        vec![left, right],
                        "row {y} edges misaligned at width {width} streaming={streaming}: \
                         expected [{left}, {right}], got {edges:?}",
                    );
                }

                // The box spans the full viewport width (the active rect is `width`
                // columns), so the right edge sits on the last column.
                assert_eq!(left, 0, "box hugs the left edge at width {width}");
                assert_eq!(
                    right,
                    width - 1,
                    "box top border must reach the last column at width {width}, \
                     not fall short of the content rows",
                );
            }
        }
    }

    #[test]
    fn flush_raw_empty_writes_nothing() {
        let mut buf: Vec<u8> = Vec::new();
        flush_raw(&mut buf, &[]).unwrap();
        assert!(buf.is_empty(), "empty queue emits no bytes");
    }

    #[test]
    fn flush_raw_brackets_sequences_in_cursor_save_restore() {
        let mut buf: Vec<u8> = Vec::new();
        flush_raw(&mut buf, &["\x1b]133;A\x07", "\x1b]133;B\x07"]).unwrap();
        // Save first, both escapes in order, restore last — the caret is parked
        // around the raw writes so it is not disturbed.
        let expected = [
            SAVE_CURSOR,
            b"\x1b]133;A\x07",
            b"\x1b]133;B\x07",
            RESTORE_CURSOR,
        ]
        .concat();
        assert_eq!(buf, expected);
    }

    // --- Cross-region journeys (VAL-CROSS-001 / -002 / -003) ---------------
    //
    // These exercise the *driver-level slice* of each M3 cross-region journey:
    // the pieces that flow through this module's `draw` + `HistorySink` commit
    // path against a `TestBackend`, which is the leak-free / no-scrollback-leak
    // ground truth (a tmux `capture-pane -S -` would fold in tmux's OWN resize
    // reflow — see docs/user-test-patterns.md — so scrollback-leak correctness is
    // asserted here on a raw `TestBackend`, never through tmux history).

    use super::super::state::{DriverState, lock_state};
    use hand_tui::rt::history::HistorySink;
    use std::sync::Mutex as StdMutex;

    /// A minimal stand-in for the frame scheduler: owns the fixed-max inline
    /// `TestBackend` terminal and a `HistorySink`, and replays the scheduler's
    /// per-frame contract — drain the state's queued scrollback commits through
    /// the sink *before* drawing the viewport — so a test drives compose / stream
    /// / settle / resize exactly as the live loop does.
    struct JourneyHarness {
        terminal: Terminal<TestBackend>,
        history: HistorySink,
        editor: SharedEditor,
        /// The backend size the last frame drew against — mirrors the real
        /// scheduler's `last_size`, so a size change is detected in the frame path
        /// and the old-width viewport is wiped before autoresize can spill it.
        last_size: Option<ratatui::layout::Size>,
    }

    impl JourneyHarness {
        fn new(width: u16, height: u16) -> Self {
            use hand_tui::rt::components::EditorBorder;
            Self {
                terminal: fixed_viewport(width, height),
                history: HistorySink::new(),
                editor: Arc::new(Mutex::new(Editor::new().border(EditorBorder::None))),
                last_size: None,
            }
        }

        /// Resize the backend the way a `RtInputEvent::Resize` does: the draw
        /// path's own size-change detection then re-anchors the fixed viewport
        /// on the next frame.
        fn resize(&mut self, width: u16, height: u16) {
            self.terminal.backend_mut().resize(width, height);
        }

        /// One scheduler frame: detect a backend resize and wipe the old-width
        /// viewport *before* committing (so a stale-width fragment can never spill
        /// into scrollback — the M1 resize-erase invariant), drain queued commits
        /// into scrollback, then paint the viewport from a snapshot of the state.
        fn frame(&mut self, state: &Arc<StdMutex<DriverState>>, footer: &FooterViewModel) {
            // Resize-erase: exactly the scheduler's pre-commit wipe on a detected
            // size change (see `spawn_scheduler`), the step that keeps the live
            // region out of scrollback across a resize.
            let current_size = self.terminal.size().ok();
            if let Some(current) = current_size
                && self.last_size.is_some_and(|prev| prev != current)
            {
                use hand_tui::rt::session::clear_viewport_region;
                let _ = clear_viewport_region(&mut self.terminal);
            }
            self.last_size = current_size;

            let commits = lock_state(state).take_commits();
            for block in commits {
                self.history
                    .commit_lines(&mut self.terminal, block)
                    .expect("commit block into scrollback");
            }
            let (size, streaming, preview) = {
                let guard = lock_state(state);
                (
                    guard.size,
                    guard.streaming,
                    guard.streaming_preview.clone().unwrap_or_default(),
                )
            };
            let mut loader = Loader::new(LOADER_MESSAGE);
            loader.set_active(streaming);
            let snapshot = StateSnapshot {
                size,
                loader: streaming,
                loader_message: None,
                preview,
                overlay_open: false,
                overlay_lines: None,
                palette: ThemePalette::default(),
            };
            let editor = &self.editor;
            let footer = footer.clone();
            self.terminal
                .draw(|frame| draw(frame, &snapshot, editor, &loader, &footer))
                .expect("draw one journey frame");
        }

        /// Every scrollback row (committed history, oldest first) with trailing
        /// blanks trimmed — the live viewport is deliberately excluded so a probe
        /// asserts what actually *settled* into scrollback.
        fn scrollback_rows(&self) -> Vec<String> {
            let buf = self.terminal.backend().scrollback();
            let area = buf.area;
            let mut out = Vec::new();
            for y in area.y..area.y + area.height {
                let mut row = String::new();
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell((x, y)) {
                        row.push_str(cell.symbol());
                    }
                }
                let trimmed = row.trim_end().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
            }
            out
        }

        /// The live viewport text (the active box + preview band), joined per row.
        fn viewport_text(&self) -> String {
            let buf = self.terminal.backend().buffer();
            let area = buf.area;
            let mut out = String::new();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell((x, y)) {
                        out.push_str(cell.symbol());
                    }
                }
                out.push('\n');
            }
            out
        }
    }

    #[test]
    fn compose_to_reply_settles_the_stream_into_scrollback_without_residue() {
        // VAL-CROSS-001: an echoed user bubble commits to scrollback, a reply
        // streams live in the preview band, then settles into scrollback on
        // finalize — leaving the live band clean (no half-streamed residue) and
        // the loader dismissed.
        let mut h = JourneyHarness::new(60, MAX_VIEWPORT_ROWS);
        let state = Arc::new(StdMutex::new(DriverState::new(TerminalSize::new(
            60,
            MAX_VIEWPORT_ROWS,
        ))));
        let footer = FooterViewModel::default();

        // 1. Compose + submit: the user bubble is queued as a scrollback commit,
        //    the way `AppendUser` does after a submit.
        lock_state(&state).queue_commit(vec![Line::from("> tell me a joke")]);
        h.frame(&state, &footer);
        assert!(
            h.scrollback_rows()
                .iter()
                .any(|r| r.contains("tell me a joke")),
            "the echoed user bubble must settle into scrollback"
        );

        // 2. The turn streams: the loader shows and the partial reply renders in
        //    the live preview band above the box (never in scrollback yet).
        {
            let mut guard = lock_state(&state);
            guard.streaming = true;
            guard.set_streaming_preview(Some(vec![Line::from("Why did the")]));
        }
        h.frame(&state, &footer);
        let mid = h.viewport_text();
        assert!(mid.contains("Working"), "loader visible mid-stream: {mid}");
        assert!(
            mid.contains("Why did the"),
            "the live partial shows in the preview band: {mid}"
        );
        assert!(
            !h.scrollback_rows()
                .iter()
                .any(|r| r.contains("Why did the")),
            "an in-flight partial must NOT be in scrollback yet"
        );

        // 3. Finalize: clear the preview, drop the loader, and commit the final
        //    reply to scrollback — the compose→reply→settle round trip.
        {
            let mut guard = lock_state(&state);
            guard.set_streaming_preview(None);
            guard.streaming = false;
            guard.queue_commit(vec![Line::from("Why did the chicken cross the road?")]);
        }
        h.frame(&state, &footer);

        assert!(
            h.scrollback_rows()
                .iter()
                .any(|r| r.contains("chicken cross the road")),
            "the finalized reply must settle into scrollback"
        );
        let after = h.viewport_text();
        assert!(
            !after.contains("Working"),
            "the loader must be dismissed after settle: {after}"
        );
        assert!(
            !after.contains("Why did the"),
            "no half-streamed preview residue may linger in the live band: {after}"
        );
    }

    #[test]
    fn overlay_over_stream_dims_base_while_the_stream_keeps_rendering() {
        // VAL-CROSS-002 (draw-layer slice): while a turn streams, opening an
        // overlay dims the base but the streaming loader + preview keep rendering
        // underneath it — the overlay is a pure draw layer, it never pauses the
        // turn. (The Esc-closes-overlay / Esc-cancels-turn key routing is covered
        // by the input-loop cancel path; here we pin the render invariant.)
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new().border(hand_tui::rt::components::EditorBorder::None),
        ));
        let mut loader = Loader::new(LOADER_MESSAGE);
        loader.set_active(true);
        let mut terminal = fixed_viewport(80, MAX_VIEWPORT_ROWS);
        let snapshot = StateSnapshot {
            size: TerminalSize::new(80, MAX_VIEWPORT_ROWS),
            loader: true,
            loader_message: None,
            preview: vec![Line::from("streaming reply in flight")],
            overlay_open: true,
            overlay_lines: Some(vec![Line::from("→ claude-sonnet [anthropic]")]),
            palette: ThemePalette::default(),
        };
        let footer = FooterViewModel::default();
        terminal
            .draw(|frame| {
                draw(frame, &snapshot, &editor, &loader, &footer);
                if let Some(lines) = snapshot.overlay_lines.clone() {
                    let area = frame.area();
                    draw_overlay(frame.buffer_mut(), area, lines);
                }
            })
            .expect("draw one frame");

        let buf = terminal.backend().buffer();
        // The overlay is layered on top.
        let text = buffer_text(&terminal);
        assert!(
            text.contains("claude-sonnet"),
            "overlay content on top: {text}"
        );
        // A corner base cell (outside the centered dialog) is dimmed.
        let corner = buf.cell((0, buf.area.height - 1)).unwrap();
        assert!(
            corner.modifier.contains(Modifier::DIM),
            "base cells outside the overlay dim while the stream runs underneath"
        );
    }

    #[test]
    fn resize_under_load_relays_out_leak_free_across_narrow_then_short() {
        // VAL-CROSS-003: with the editor grown, the loader on, and a reply
        // streaming, resize twice (narrow, then short). The bottom area re-lays
        // to each new width/height every time, and the live region never leaks
        // into scrollback — asserted on a raw `TestBackend`, the ground truth for
        // this class (tmux `capture-pane -S -` would fold in tmux's own reflow).
        let mut h = JourneyHarness::new(80, MAX_VIEWPORT_ROWS);
        let state = Arc::new(StdMutex::new(DriverState::new(TerminalSize::new(
            80,
            MAX_VIEWPORT_ROWS,
        ))));
        let footer = FooterViewModel::default();

        // Grow the editor (multi-line), turn the loader on, and stream a preview.
        {
            let mut ed = lock_editor(&h.editor);
            ed.set_text("line one\nline two\nline three\nline four");
        }
        {
            let mut guard = lock_state(&state);
            guard.streaming = true;
            guard.set_streaming_preview(Some(vec![
                Line::from("streaming reply row A"),
                Line::from("streaming reply row B"),
            ]));
        }
        // Commit one settled block so scrollback is non-empty going in.
        lock_state(&state).queue_commit(vec![Line::from("SETTLED-MARKER earlier reply")]);
        h.frame(&state, &footer);

        let baseline_scrollback = h.scrollback_rows();
        assert!(
            baseline_scrollback
                .iter()
                .any(|r| r.contains("SETTLED-MARKER")),
            "the earlier reply is in scrollback before any resize"
        );

        // Resize #1 — narrow to 40 columns.
        h.resize(40, MAX_VIEWPORT_ROWS);
        lock_state(&state).size = TerminalSize::new(40, MAX_VIEWPORT_ROWS);
        h.frame(&state, &footer);
        // The active box re-lays out to the narrow width: its border reaches the
        // last visible column, so the whole bottom UI followed the resize.
        let (top_n, _) = border_rows(&h.terminal);
        let edges_n = edge_cols(&h.terminal, top_n);
        assert_eq!(
            edges_n,
            vec![0, 39],
            "the box re-lays out to width 40 after the narrow resize"
        );

        // Resize #2 — short to 8 rows.
        h.resize(40, 8);
        lock_state(&state).size = TerminalSize::new(40, 8);
        h.frame(&state, &footer);
        // The active box is trimmed to fit the short pane (it never draws past
        // the bottom edge): its bottom border sits within the 8-row viewport.
        let (_, bottom_s) = border_rows(&h.terminal);
        assert!(
            bottom_s < 8,
            "the box bottom border stays inside the 8-row short pane, at row {bottom_s}"
        );

        // Leak-free: no live-region content (the loader message or the streaming
        // preview) ever leaked into scrollback across either resize. Only settled
        // history is there.
        let final_scrollback = h.scrollback_rows();
        for row in &final_scrollback {
            assert!(
                !row.contains(LOADER_MESSAGE),
                "the loader must never leak into scrollback on resize, found: {row:?}"
            );
            assert!(
                !row.contains("streaming reply row"),
                "the streaming preview must never leak into scrollback on resize, found: {row:?}"
            );
        }
        assert!(
            final_scrollback
                .iter()
                .any(|r| r.contains("SETTLED-MARKER")),
            "the settled reply survives the resizes in scrollback"
        );
    }

    // --- 0x0 PTY operability (VAL-COMPAT-011) ------------------------------

    #[test]
    fn driver_draw_is_operable_on_a_zero_sized_pty_via_fallback_geometry() {
        // VAL-COMPAT-011: the interactive driver's own draw path stays operable
        // when the terminal reports a degenerate 0x0 size. At runtime the rt
        // session wraps the backend in a `FallbackSizeBackend`, whose geometry is
        // exactly `effective_size(cols, rows)` — 80x24 for a 0x0 PTY. This drives
        // the driver's `draw` at that resolved fallback geometry, the
        // coding-agent-side counterpart to the rt-layer 0x0 render test, and pins
        // that the box + footer render rather than the frame collapsing to
        // nothing. (`FallbackSizeBackend` requires `Backend<Error = io::Error>`,
        // which `TestBackend` is not, so the fallback size is resolved directly.)
        use hand_tui::rt::session::{FALLBACK_COLS, FALLBACK_ROWS, effective_size};

        // A 0x0 PTY resolves to the 80x24 fallback the driver actually renders at.
        let (fallback_cols, fallback_rows) = effective_size(0, 0);
        assert_eq!(
            (fallback_cols, fallback_rows),
            (FALLBACK_COLS, FALLBACK_ROWS)
        );

        let mut terminal = fixed_viewport(fallback_cols, MAX_VIEWPORT_ROWS);
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new().border(hand_tui::rt::components::EditorBorder::None),
        ));
        let loader = Loader::new(LOADER_MESSAGE);
        let snapshot = StateSnapshot {
            size: TerminalSize::new(fallback_cols, MAX_VIEWPORT_ROWS),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: false,
            overlay_lines: None,
            palette: ThemePalette::default(),
        };
        let footer = FooterViewModel {
            model_id: "mock-model".to_string(),
            ..FooterViewModel::default()
        };

        // The draw must succeed and paint the active-area box at the fallback
        // width (a panic or an empty frame would be the 0x0 regression).
        terminal
            .draw(|frame| {
                assert_eq!(
                    frame.area().width,
                    FALLBACK_COLS,
                    "the frame renders at the 80-col fallback, not the 0-col PTY size"
                );
                draw(frame, &snapshot, &editor, &loader, &footer);
            })
            .expect("draw must succeed at fallback geometry on a 0x0 PTY");

        assert!(
            has_border(terminal.backend().buffer()),
            "the active-area box paints at fallback geometry"
        );
        assert!(
            buffer_text(&terminal).contains("mock-model"),
            "the footer stays legible on a 0x0 PTY"
        );
    }
}
