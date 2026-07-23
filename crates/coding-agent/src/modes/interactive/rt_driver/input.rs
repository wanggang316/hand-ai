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
//!    origin drifts down as scrollback fills), then renders the bordered box, an
//!    optional loader row, the editor (borderless — the box is the driver's), and
//!    the footer placeholder line;
//! 4. drives the hardware cursor from the editor's reported caret.
//!
//! This mirrors the rt demo's scheduler/draw split exactly; the only differences
//! are the concrete components (M2 [`Editor`] + a footer line instead of the
//! demo's input row + status block) and the chat commits coming from the agent
//! driver rather than a synthetic stream.

use std::io;
use std::sync::{Arc, Mutex};

use hand_tui::rt::history::{HistorySink, wrap_lines};
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionTerminal, clear_viewport_region};
use hand_tui::rt::view::bottom_area_geometry;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::state::{DriverState, SharedEditor, SharedFooter, lock_editor, lock_footer, lock_state};

/// Spinner glyph frames cycled while a turn streams.
const SPINNER_FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

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
) -> (FrameRequester, tokio::task::JoinHandle<io::Result<()>>) {
    let mut terminal = EraseOnDrop::new(terminal);
    let mut history = HistorySink::new();
    // The backend size the last draw painted against, so a resize is detected in
    // the draw path itself (independent of when the crossterm Resize event
    // reaches the input loop) and the old-width viewport is erased before
    // anything autoresizes and spills.
    let mut last_size: Option<ratatui::layout::Size> = None;

    FrameScheduler::spawn(move || {
        // Snapshot state under the lock, then release it before touching the
        // terminal. Finished chat blocks and raw sequences are taken out (not
        // cloned) so they can only ever be committed / written once.
        let (snapshot, commits, raw) = {
            let mut guard = lock_state(&state);
            let commits = guard.take_commits();
            let raw = guard.take_raw();
            if guard.streaming {
                guard.spinner_phase = guard.spinner_phase.wrapping_add(1);
            }
            let preview = guard.streaming_preview.clone().unwrap_or_default();
            let snapshot = StateSnapshot {
                size: guard.size,
                loader: guard.streaming,
                spinner_phase: guard.spinner_phase,
                preview,
            };
            (snapshot, commits, raw)
        };

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

        let footer_text = lock_footer(&footer).clone();

        // Wrap the whole paint in BSU/ESU: the closing `?2026l` is emitted even
        // if `terminal.draw` errors, so an interrupt mid-draw never leaves an
        // open synchronized block.
        let mut stdout = io::stdout();
        let editor = &editor;
        draw_synchronized(&mut stdout, |w| {
            terminal.draw(|frame| draw(frame, &snapshot, editor, &footer_text))?;
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
    /// Whether the loader/spinner row shows (streaming turn in flight).
    loader: bool,
    /// Spinner animation phase for the loader glyph.
    spinner_phase: u64,
    /// The live streaming-preview lines to paint in the blank band above the
    /// active bottom box, or empty when no turn is streaming. The tail is shown
    /// when the band is shorter than the preview so the most recent tokens are
    /// always visible.
    preview: Vec<Line<'static>>,
}

/// Paint one frame: the bordered bottom box, an optional loader row, the editor,
/// and the footer placeholder — laid out inside the fixed inline viewport.
fn draw(frame: &mut ratatui::Frame, state: &StateSnapshot, editor: &SharedEditor, footer: &str) {
    let area = frame.area();

    // The editor's desired interior rows drive the auto-grow. Measure it against
    // the interior width the box will give it (2 border columns), clamped to the
    // 1..=8 input-row band by the geometry helper.
    let input_rows = {
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
    // collapse) stay blank and repaint clear each frame.
    let block = Block::bordered().border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(block, geometry.active);

    // The loader/spinner row, when streaming, sits just below the top border.
    if let Some(loader_rect) = geometry.loader {
        let glyph = SPINNER_FRAMES[(state.spinner_phase as usize) % SPINNER_FRAMES.len()];
        let spinner = Line::from(vec![
            format!(" {glyph} ").fg(Color::Yellow),
            "working…".dim(),
        ]);
        frame.render_widget(Paragraph::new(spinner), inset(loader_rect));
    }

    // Split the input interior into the editor rows (all but the last) and the
    // one-row footer placeholder (the last interior row).
    let (editor_rect, footer_rect) = split_editor_footer(inset(geometry.input));

    // Render the editor and drive the hardware cursor from its reported caret.
    {
        use hand_tui::rt::view::RtComponent;
        let ed = lock_editor(editor);
        ed.render(editor_rect, frame.buffer_mut());
        // Disambiguate: the `RtComponent::cursor` (viewport-local `Option<Position>`)
        // over the inherent `Editor::cursor` (the `(line, col)` accessor). The
        // component caret is already anchored at `editor_rect` (its render area).
        if let Some(caret) = RtComponent::cursor(&*ed) {
            frame.set_cursor_position(caret);
        }
    }

    // The footer placeholder line, dim.
    if footer_rect.height > 0 {
        Paragraph::new(Line::from(footer.to_string().dim()))
            .render(footer_rect, frame.buffer_mut());
    }
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

/// Split a bottom-area body rect into the editor (all but the last row) and a
/// one-row footer placeholder (the last row).
///
/// When the body is a single row the editor keeps it and the footer collapses to
/// a zero-height rect below, so the caret always has a home and nothing overlaps.
fn split_editor_footer(body: Rect) -> (Rect, Rect) {
    if body.height <= 1 {
        let footer = Rect::new(body.x, body.y.saturating_add(body.height), body.width, 0);
        return (body, footer);
    }
    let editor = Rect::new(body.x, body.y, body.width, body.height - 1);
    let footer = Rect::new(body.x, body.y + body.height - 1, body.width, 1);
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
    fn split_editor_footer_reserves_last_row_for_footer() {
        let body = Rect::new(1, 5, 40, 4);
        let (editor, footer) = split_editor_footer(body);
        assert_eq!(editor, Rect::new(1, 5, 40, 3));
        assert_eq!(footer, Rect::new(1, 8, 40, 1));
    }

    #[test]
    fn split_editor_footer_single_row_gives_editor_all_and_empty_footer() {
        let body = Rect::new(1, 5, 40, 1);
        let (editor, footer) = split_editor_footer(body);
        assert_eq!(editor, body);
        assert_eq!(footer.height, 0);
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
}
