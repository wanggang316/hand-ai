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
//! 3. rebuilds the inline viewport taller while a modal overlay is mounted (the
//!    M6 revision of the fixed-height decision: grow at mount so panel + box +
//!    footer fit, shrink back at unmount — both through the erase-first
//!    [`set_session_viewport_height`] primitive, so the height change is
//!    ghost-free and leak-free);
//! 4. lays the inline bottom area out with
//!    [`bottom_area_geometry_within`]`.offset_y(frame.area().y)` (M1 FIX-2: the
//!    viewport origin drifts down as scrollback fills), then renders the
//!    bordered box around the editor only (borderless — the box is the
//!    driver's), the optional working-loader row in an unbordered row directly
//!    *above* the box (M2 [`Loader`]), the two-line footer view-model in an
//!    unbordered band *below* it, and — while a selector is mounted — the
//!    bordered overlay panel glued directly above the box (M6);
//! 5. drives the hardware cursor from the editor's reported caret.
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
use hand_tui::rt::session::{
    EraseOnDrop, SessionTerminal, clear_viewport_region, set_session_viewport_height,
};
use hand_tui::rt::view::{
    BORDER_ROWS, LOADER_ROWS, MAX_VIEWPORT_ROWS, RtComponent, bottom_area_geometry_within,
};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::chrome;
use super::footer::{FooterViewModel, render_footer_lines};
use super::overlay::SharedOverlay;
use super::state::{DriverState, SharedEditor, SharedFooter, lock_editor, lock_footer, lock_state};
use crate::modes::interactive::theme::ThemePalette;

/// The static message the working-loader shows while a turn streams.
const LOADER_MESSAGE: &str = "Working…";

/// Rows the footer view-model occupies in the unbordered band *below* the
/// bordered box (the cwd line + the stats line). Reserved from the fixed
/// viewport budget before the box is laid out, so the footer sits glued under
/// the box's bottom border and the box wraps only the editor.
const FOOTER_ROWS: u16 = 2;

/// Rows the persistent key-hint line occupies in the bottom chrome, directly
/// below the box's bottom border and above the footer. Guidance chrome, so it
/// yields first on a short pane (shown only when box + footer already fit with a
/// spare row) and is hidden under a modal overlay, whose panel carries its own
/// hint and owns input.
const HINT_ROWS: u16 = 1;

/// The smallest useful bordered box: the top + bottom border rows plus one
/// editor row for the caret. On a tiny pane the footer band collapses before
/// the box is ever squeezed below this.
const MIN_BOX_ROWS: u16 = 3;

/// The smallest useful overlay panel: the top + bottom border rows plus one
/// content row. With less band than this above the box, the panel is dropped
/// for the frame rather than squeezing the box or the footer (the
/// footer-collapse degradation spirit).
const MIN_PANEL_ROWS: u16 = 3;

/// Rows of transcript kept visible above a viewport grown for the overlay
/// panel: a mounted selector may claim most of the terminal, never the whole
/// screen.
const OVERLAY_TRANSCRIPT_MARGIN: u16 = 2;

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
    // The height the inline viewport is currently built at. The default is the
    // fixed-max bottom-UI ceiling the session terminal was constructed with;
    // while a modal overlay is mounted the viewport is rebuilt taller so
    // panel + box + footer fit (the M6 revision of the fixed-height decision)
    // and rebuilt back at unmount — see `desired_viewport_rows`.
    let mut viewport_rows: u16 = hand_tui::rt::session::INLINE_VIEWPORT_ROWS;

    // The working-loader shown in the unbordered row directly above the box while
    // a turn streams. Owned by the draw closure (not shared): it is toggled
    // active/idle from the streaming flag and ticked once per frame while
    // streaming, so the spinner animates and the static "Working…" message is
    // present only mid-turn. When the turn ends the draw path drops the loader
    // row entirely and the fixed-viewport blank repaint wipes it — no ghost
    // "Working…" or border fragment left in the active area or in scrollback
    // (M1 shrink-erase invariant).
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
                // overlay is up so the hardware cursor does not blink inside the
                // collapsed box under the panel.
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

        // M6 overlay panel: rebuild the inline viewport when the mounted
        // selector needs a taller bottom area, when it just closed, or when a
        // resize invalidated the grown height. The primitive erases first, so
        // the height change never ghosts or leaks; the ratchet inside
        // `desired_viewport_rows` keeps mid-overlay filtering from thrashing
        // rebuilds. The real backend height is preferred over the tracked one
        // so a resize storm cannot momentarily over-grow.
        let terminal_rows = current_size.map_or(snapshot.size.rows, |size| size.height);
        let target = desired_viewport_rows(
            viewport_rows,
            snapshot
                .overlay_lines
                .as_ref()
                .map(|lines| lines.len().min(u16::MAX as usize) as u16),
            terminal_rows,
        );
        if target != viewport_rows {
            set_session_viewport_height(&mut terminal, target)?;
            viewport_rows = target;
        }

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
            // `draw` paints the whole frame, including the mounted selector's
            // bordered panel glued above the box (the panel placement needs the
            // box geometry, so it lives inside `draw` rather than as a separate
            // layer). The whole viewport repaints each frame, so closing the
            // overlay leaves no ghost border (VAL-OVERLAY-008).
            terminal.draw(|frame| draw(frame, &snapshot, editor, loader, &footer_view))?;
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
    /// layout (whether the row above the box is reserved) and the loader's
    /// active state.
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
    /// editor caret is suppressed so the hardware cursor is not stranded in the
    /// collapsed box under the panel.
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

/// Paint one frame: the bordered bottom box (wrapping only the editor), the
/// working-loader row in the unbordered row directly above it while streaming,
/// and the two-line footer in the unbordered band below it — laid out inside
/// the fixed inline viewport.
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
    // 1..=8 input-row band by the geometry helper. The popup's wanted rows are
    // read under the same lock — the loader-row arbitration below needs them.
    let (editor_rows, popup_wanted) = {
        let ed = lock_editor(editor);
        // desired_height on a borderless editor returns interior rows only; probe
        // against a representative interior rect (border insets applied below).
        let probe = Rect::new(
            0,
            0,
            area.width.saturating_sub(2).max(1),
            area.height.max(1),
        );
        (ed.desired_height(probe).max(1), ed.popup_row_count())
    };

    // The footer renders BELOW the box, so its rows come out of the viewport
    // budget before the box is laid out: the geometry sees a height reduced by
    // the footer band and bottom-anchors the box against it, leaving the band
    // glued under the box's bottom border. On a tiny pane the footer collapses
    // (partially, then fully) before the box shrinks below border + one editor
    // row, so the caret always keeps a home. The budget is the frame's own
    // height: normally the fixed max clamped to the terminal, and — while a
    // modal overlay is mounted — the taller viewport the scheduler rebuilt for
    // the panel.
    let viewport_budget = area.height.max(1);
    let footer_rows = FOOTER_ROWS.min(viewport_budget.saturating_sub(MIN_BOX_ROWS));
    // The key-hint line sits between the box and the footer. It is guidance, so
    // it yields first: shown only when the box (at its minimum) and the full
    // footer already fit with a spare row, and never while a modal overlay is
    // mounted (the panel carries its own hint and owns input, so the main input
    // hint would be misleading). Its row comes out of the box budget just like
    // the footer's, so the box bottom-anchors above hint + footer.
    let hint_rows = if !state.overlay_open
        && viewport_budget >= MIN_BOX_ROWS.saturating_add(footer_rows).saturating_add(HINT_ROWS)
    {
        HINT_ROWS
    } else {
        0
    };
    let box_budget = viewport_budget
        .saturating_sub(footer_rows)
        .saturating_sub(hint_rows);

    // While a modal overlay is mounted, its bordered panel replaces the whole
    // band above the box for the duration:
    // - the selector owns every key (LIFO modal capture), so the autocomplete
    //   popup can never open mid-overlay — the popup band is simply skipped;
    // - the loader row and the streaming preview yield the band to the panel
    //   (the turn keeps streaming: commits keep settling into scrollback
    //   underneath) and both resume the moment the overlay closes;
    // - the box collapses to a single editor row (the editor is frozen and its
    //   caret hidden anyway), which keeps the panel budget deterministic —
    //   panel + collapsed box + footer is exactly what the grown viewport
    //   reserved.
    let overlay_open = state.overlay_open;

    // While streaming, the loader occupies one unbordered row directly ABOVE the
    // box's top border (the box wraps only the editor). Reserving that row here
    // caps the editor's growth one row earlier, so at max growth the box top
    // stops one row short of the viewport origin and the loader always has a
    // home. Idle frames reserve nothing — the layout is identical to the
    // loader-free one. The `.max(1)` keeps the caret's single editor row on a
    // tiny pane; when even the reserved row then has no room, the loader yields
    // below rather than squeezing the editor.
    let loader_reserve = if state.loader && !overlay_open {
        LOADER_ROWS
    } else {
        0
    };
    let editor_rows = if overlay_open {
        1
    } else {
        editor_rows.min(
            box_budget
                .saturating_sub(BORDER_ROWS)
                .saturating_sub(loader_reserve)
                .max(1),
        )
    };

    // Lay the bottom area out inside the viewport, then translate every rect
    // down by `area.y` so the bottom UI paints at the viewport's real rows —
    // not absolute row 0, which drifts off-screen once `insert_before` moves the
    // viewport down (the "box vanishes after a big block" bug). This is the M1
    // FIX-2 offset_y invariant. `loader_visible` is hard-false: the geometry's
    // interior loader slot is retired — the loader row lives above the box now.
    let geometry =
        bottom_area_geometry_within(editor_rows, false, area.width, box_budget).offset_y(area.y);

    // Arbitrate the blank band above the box (viewport origin → box top): while
    // streaming the loader claims the band's bottom row, glued to the top border
    // — unless the popup needs the space. The popup wins and the loader yields
    // for the frame (the same degradation spirit as the footer's collapse), so
    // the two never overlap and the order stays popup < loader < box. A mounted
    // overlay panel supersedes both (see the band rule above).
    let band = geometry.active.y.saturating_sub(area.y);
    let popup_leaves_room = popup_wanted == 0 || band >= popup_wanted.saturating_add(LOADER_ROWS);
    let loader_rows = if state.loader && !overlay_open && band >= LOADER_ROWS && popup_leaves_room {
        LOADER_ROWS
    } else {
        0
    };

    // The live streaming preview paints in the blank band ABOVE the loader row
    // and the active box (between the viewport origin and whichever is higher),
    // so the in-flight assistant partial grows in place without touching
    // scrollback. When the preview is taller than the band, its tail is shown so
    // the newest tokens stay visible. The panel owns the band while an overlay
    // is mounted.
    if !overlay_open {
        draw_stream_preview(frame, &state.preview, area, geometry.active, loader_rows);
    }

    // The bordered box occupies only the active area; rows above it (freed by a
    // collapse) stay blank and repaint clear each frame. The border colours from
    // the active palette (the default palette keeps the historical dark grey).
    let block = Block::bordered().border_style(Style::default().fg(state.palette.border));
    frame.render_widget(block, geometry.active);

    // The working-loader row, while streaming, sits directly ABOVE the box's top
    // border, unbordered — inset to the box's interior columns like the footer,
    // so the spinner + message column-align with the editor text. The M2 Loader
    // paints nothing when inactive, and the row is claimed only while streaming —
    // so the dismissal at AgentEnd leaves no "Working…" or spinner residue (the
    // fixed-viewport blank repaint wipes the freed row).
    if loader_rows > 0 {
        let loader_rect = Rect::new(
            area.x,
            geometry.active.y.saturating_sub(loader_rows),
            area.width,
            loader_rows,
        );
        loader.render(inset(loader_rect), frame.buffer_mut());
    }

    // The box interior is the editor's alone — neither the loader nor the footer
    // shares it.
    let editor_rect = inset(geometry.input);

    // Render the editor and drive the hardware cursor from its reported caret.
    // When a modal overlay is open it owns focus, so the editor caret is suppressed
    // — the hardware cursor is not placed under the dimmed dialog.
    //
    // The editor self-renders its `@`/`/` suggestion popup in the band *below* its
    // box, but the driver's inline geometry gives the editor a rect sized to
    // exactly the box height — so that band is zero rows and the popup never
    // paints. The driver owns the surrounding box, so it paints the popup itself,
    // in a reserved band *above* the box and the loader row (below the box sit
    // the footer band and the viewport's bottom edge; above it is the blank band
    // the box is anchored over). This keeps the popup clear of the loader
    // (directly above the box), the footer (below the box), and scrollback
    // (above the viewport).
    {
        let ed = lock_editor(editor);
        ed.render(editor_rect, frame.buffer_mut());
        // The popup band is skipped outright while a modal overlay is mounted:
        // the selector consumes every key, so no completable context can even
        // arise, and the band belongs to the panel.
        if !overlay_open {
            draw_autocomplete_popup(frame.buffer_mut(), &ed, area, geometry.active, loader_rows);
        }
        // Disambiguate: the `RtComponent::cursor` (viewport-local `Option<Position>`)
        // over the inherent `Editor::cursor` (the `(line, col)` accessor). The
        // component caret is already anchored at `editor_rect` (its render area).
        if !state.overlay_open
            && let Some(caret) = RtComponent::cursor(&*ed)
        {
            frame.set_cursor_position(caret);
        }
    }

    // The persistent key-hint line, in the unbordered row directly below the
    // box's bottom border and above the footer. Inset to the box's interior
    // columns like the footer so the glyphs column-align with the editor text.
    // Skipped when `hint_rows` is 0 (short pane or a mounted overlay), where the
    // footer simply moves up to the box.
    if hint_rows > 0 {
        let hint_rect = inset(Rect::new(
            area.x,
            geometry.active.bottom(),
            area.width,
            hint_rows,
        ))
        .intersection(area);
        if hint_rect.height > 0 {
            Paragraph::new(Text::from(chrome::key_hint_line(&state.palette)))
                .render(hint_rect, frame.buffer_mut());
        }
    }

    // The two-line footer view-model, rendered into the unbordered band directly
    // below the key-hint row (or the box's bottom border when the hint is
    // hidden). Inset to the box's interior columns so the footer text lines up
    // with the editor text above it, and intersected with the frame so a size
    // mismatch can never paint past the viewport. `Paragraph` clips a line wider
    // than the rect, so a narrow pane never spills.
    let footer_rect = inset(Rect::new(
        area.x,
        geometry.active.bottom().saturating_add(hint_rows),
        area.width,
        footer_rows,
    ))
    .intersection(area);
    if footer_rect.height > 0 {
        let lines = render_footer_lines(footer, footer_rect.width, &state.palette);
        Paragraph::new(Text::from(lines)).render(footer_rect, frame.buffer_mut());
    }

    // The mounted selector's bordered panel, glued directly above the box's top
    // border — painted last so it owns its band outright. The stack order while
    // an overlay is up is fixed: transcript (above the viewport) → panel → box
    // → footer, with no overlap and no floating (the M6 layout).
    if let Some(lines) = state.overlay_lines.clone() {
        draw_overlay_panel(
            frame.buffer_mut(),
            area,
            geometry.active.y,
            lines,
            &state.palette,
        );
    }
}

/// The interior width the overlay panel gives the mounted selector's lines, for
/// the current viewport columns: the panel spans the full frame width — its
/// edges aligned with the input box below it — minus the two border columns.
/// The scheduler measures the mounted selector's render lines against this so
/// wrapping matches the panel it paints into.
pub(crate) fn overlay_interior_width(cols: u16) -> u16 {
    cols.saturating_sub(2).max(1)
}

/// The inline-viewport height the scheduler wants this frame.
///
/// With no overlay mounted this is always the fixed-max default
/// ([`MAX_VIEWPORT_ROWS`]) — closing a selector shrinks the viewport back. With
/// an overlay mounted, the panel needs its content plus two border rows, on top
/// of the collapsed box and the footer band; that total is clamped between the
/// default (never shrink below it — a short selector simply fits inside) and
/// the terminal height minus a couple of transcript rows
/// ([`OVERLAY_TRANSCRIPT_MARGIN`]) so the panel can never claim the whole
/// screen. On a pane too short to grow at all, the cap floors at the default
/// and the panel clamps *itself* into the band instead (the small-terminal
/// rule: the panel shrinks first, the box and footer never move out).
///
/// The `.max(current)` is a mid-overlay ratchet: filtering a selector narrows
/// its list every keystroke, and shrinking the viewport to chase it would
/// rebuild the terminal per key. Growing is immediate (a backspace that
/// re-widens the list gets its rows back); shrinking waits for the unmount —
/// except when the terminal itself shrank, where the cap re-clamps downward.
fn desired_viewport_rows(
    current: u16,
    overlay_content_rows: Option<u16>,
    terminal_rows: u16,
) -> u16 {
    let Some(content) = overlay_content_rows else {
        return MAX_VIEWPORT_ROWS;
    };
    let cap = terminal_rows
        .saturating_sub(OVERLAY_TRANSCRIPT_MARGIN)
        .max(MAX_VIEWPORT_ROWS);
    let needed = content
        .saturating_add(BORDER_ROWS)
        .saturating_add(MIN_BOX_ROWS)
        .saturating_add(FOOTER_ROWS)
        .clamp(MAX_VIEWPORT_ROWS, cap);
    needed.max(current).min(cap)
}

/// Paint the mounted selector's `lines` as a bordered panel glued directly
/// above the box's top border (`box_top`), spanning the full frame width so its
/// edges align with the input box below (the M6 overlay layout).
///
/// The panel's height is its content plus the two border rows, clamped to the
/// band between the viewport origin and the box top — it can never overlap the
/// box or the footer, and it never floats. The scheduler normally grows the
/// viewport so the band fits the whole list (see [`desired_viewport_rows`]); on
/// a pane too short for that the panel clamps into whatever band exists and the
/// selector's own list window scrolls inside it. With less band than
/// [`MIN_PANEL_ROWS`] the panel is dropped for the frame rather than squeezing
/// the box. The transcript above is left untouched — no background dim: the
/// panel covers no content, so there is nothing to recede. Because the whole
/// viewport repaints each frame, closing the overlay leaves no ghost border
/// (VAL-OVERLAY-008).
pub(crate) fn draw_overlay_panel(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    box_top: u16,
    lines: Vec<Line<'static>>,
    palette: &ThemePalette,
) {
    use ratatui::widgets::Clear;

    let band = box_top.saturating_sub(area.y);
    if area.width == 0 || band < MIN_PANEL_ROWS {
        return;
    }
    let height = (lines.len().min(u16::MAX as usize) as u16)
        .saturating_add(BORDER_ROWS)
        .clamp(MIN_PANEL_ROWS, band);
    let rect = Rect::new(area.x, box_top.saturating_sub(height), area.width, height);

    // Clear the footprint so nothing bleeds through, draw the border (coloured
    // from the palette like the box below, so the two read as one chrome), and
    // paint the content into the interior. `Paragraph` clips content
    // wider/taller than the interior, so the border never overflows.
    Clear.render(rect, buf);
    let block = Block::bordered().border_style(Style::default().fg(palette.border));
    let interior = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(Text::from(lines)).render(interior, buf);
}

/// Paint the streaming preview into the blank band above the active box.
///
/// The band runs from the viewport origin (`area.y`) down to the top of the
/// active box, minus the `reserved_below` rows claimed at the band's bottom by
/// the loader row — the preview never overwrites the spinner glued to the box.
/// The preview is wrapped to the width and, when it is taller than the band,
/// its **tail** is shown so the newest tokens are always visible (the preview
/// grows upward off the top of the band, matching how a streamed reply reads).
/// An empty band or empty preview paints nothing.
fn draw_stream_preview(
    frame: &mut ratatui::Frame,
    preview: &[Line<'static>],
    area: Rect,
    active: Rect,
    reserved_below: u16,
) {
    if preview.is_empty() {
        return;
    }
    let band_height = active
        .y
        .saturating_sub(area.y)
        .saturating_sub(reserved_below);
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
/// active box (and above the loader row, when one is glued to the box top).
///
/// The editor's own `render` paints the popup only in the band *below* its box,
/// which the driver's exact-height editor rect leaves empty — so the driver
/// paints it here instead, via the editor's public
/// [`autocomplete`](Editor::autocomplete) accessor. The band hangs off the top
/// of the loader row (`reserved_below` rows above the box top; zero when idle or
/// when the loader yielded), `popup_row_count` rows tall, clamped to the space
/// up to the viewport origin (`area.y`) so it never overwrites scrollback above
/// nor the loader/box below. A closed popup (zero rows) or a band with no room
/// paints nothing.
fn draw_autocomplete_popup(
    buf: &mut ratatui::buffer::Buffer,
    editor: &Editor,
    area: Rect,
    active: Rect,
    reserved_below: u16,
) {
    if !editor.autocomplete_visible() {
        return;
    }
    let wanted = editor.popup_row_count();
    // The blank band above the loader row (or the box top when no loader row),
    // from the viewport origin down.
    let bottom = active.y.saturating_sub(reserved_below);
    let above = bottom.saturating_sub(area.y);
    let rows = wanted.min(above);
    if rows == 0 || active.width == 0 {
        return;
    }
    // Hang the popup off its band bottom, growing upward: its last row sits just
    // above the loader/box, so the newest completions stay flush with the input.
    let popup = Rect::new(active.x, bottom.saturating_sub(rows), active.width, rows);
    editor.autocomplete().render(popup, buf);
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
    fn inset_trims_two_columns_and_keeps_height() {
        let r = Rect::new(0, 3, 20, 2);
        assert_eq!(inset(r), Rect::new(1, 3, 18, 2));
    }

    #[test]
    fn inset_narrow_rect_saturates_to_zero_width() {
        let r = Rect::new(0, 0, 1, 1);
        assert_eq!(inset(r).width, 0);
    }

    // --- Loader row lifecycle (VAL-CHAT-003 / VAL-COMPAT-008) ---------------

    use hand_tui::rt::components::Editor;
    use hand_tui::rt::view::TerminalSize;
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
        draw_frame_with_footer(
            terminal,
            editor,
            loader,
            streaming,
            &FooterViewModel::default(),
        );
    }

    /// Like [`draw_frame`], but with a caller-supplied footer view-model so a
    /// layout test can locate recognizable footer text in the buffer.
    fn draw_frame_with_footer(
        terminal: &mut Terminal<TestBackend>,
        editor: &SharedEditor,
        loader: &Loader,
        streaming: bool,
        footer: &FooterViewModel,
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
        terminal
            .draw(|frame| draw(frame, &snapshot, editor, loader, footer))
            .expect("draw one frame");
    }

    /// The first buffer row whose text contains `needle`, for row-order
    /// assertions (top border < editor < bottom border < footer).
    fn row_of(terminal: &Terminal<TestBackend>, needle: &str) -> Option<u16> {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        for y in area.y..area.y + area.height {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            if row.contains(needle) {
                return Some(y);
            }
        }
        None
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

        // Row order: the popup stays above the box's top border while the footer
        // sits below its bottom border — the two can never collide.
        let (top, bottom) = border_rows(&terminal);
        let popup_row = row_of(&terminal, "README.md").expect("popup candidate row");
        assert!(
            popup_row < top,
            "popup paints above the box top border (row {popup_row} < {top})"
        );
        let footer_row = row_of(&terminal, "no-model").expect("footer stats row");
        assert!(
            footer_row > bottom,
            "footer sits below the box bottom border (row {footer_row} > {bottom}), \
             clear of the popup"
        );
    }

    #[test]
    fn tight_band_popup_follows_a_deep_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hand_tui::rt::components::{EditorBorder, PathEntry, PathProvider};
        use hand_tui::rt::events::RtKey;
        use std::sync::Arc as StdArc;

        // Ten candidates against a band shorter than the popup's desired rows:
        // the idle layout leaves 6 rows above the box (viewport 11 − active 5),
        // so the 8-row window is clamped to 6. A selection between the band
        // height and the desired cap used to be clipped out of the drawn rows
        // (invisible highlight, lagging scroll) — the drawn window must follow
        // the selection at the band's real height.
        let provider = StdArc::new(PathProvider::new(
            (0..10)
                .map(|i| PathEntry::file(format!("f{i}.txt")))
                .collect(),
        ));
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new()
                .border(EditorBorder::None)
                .with_autocomplete_provider(provider),
        ));
        type_into(&editor, "@f");
        {
            let mut ed = lock_editor(&editor);
            for _ in 0..7 {
                ed.handle_key(&RtKey {
                    key_id: Some("down".to_string()),
                    raw: KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                });
            }
            assert_eq!(ed.autocomplete().selected_index(), 7, "walked to row 7");
        }

        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        draw_frame(&mut terminal, &editor, &loader, false);

        let painted = buffer_text(&terminal);
        assert!(
            painted.contains("▸ f7.txt"),
            "the highlighted candidate must be drawn in the tight band, got:\n{painted}"
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
    fn popup_paints_above_the_loader_row_when_both_are_visible() {
        use hand_tui::rt::components::{EditorBorder, PathEntry, PathProvider};
        use std::sync::Arc as StdArc;

        // Typing an @-mention mid-stream: with room in the band the loader keeps
        // its row glued to the box top and the popup band hangs off the loader's
        // top — popup, loader, box, footer from top to bottom, never overlapping.
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
        let mut loader = Loader::new(LOADER_MESSAGE);
        loader.set_active(true);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        draw_frame(&mut terminal, &editor, &loader, true);

        let (top, bottom) = border_rows(&terminal);
        let loader_row = row_of(&terminal, "Working").expect("loader row painted");
        let popup_row = row_of(&terminal, "README.md").expect("popup row painted");
        assert_eq!(
            loader_row,
            top - 1,
            "the loader row stays glued directly above the box top border"
        );
        assert!(
            popup_row < loader_row,
            "the popup paints above the loader row ({popup_row} < {loader_row})"
        );
        let footer_row = row_of(&terminal, "no-model").expect("footer stats row");
        assert!(
            footer_row > bottom,
            "the footer stays below the box, clear of popup and loader"
        );
    }

    #[test]
    fn popup_wins_the_band_and_the_loader_yields_when_the_budget_is_tight() {
        use hand_tui::rt::components::{EditorBorder, PathEntry, PathProvider};
        use std::sync::Arc as StdArc;

        // At streaming max growth the band above the box is exactly the loader's
        // reserved row. An open popup needs that row too — the popup wins and the
        // loader yields for the frame (the footer-collapse degradation spirit),
        // so nothing overlaps.
        let provider = StdArc::new(PathProvider::new(vec![
            PathEntry::file("README.md"),
            PathEntry::file("main.rs"),
        ]));
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new()
                .border(EditorBorder::None)
                .with_autocomplete_provider(provider),
        ));
        // Nine full lines plus a trailing empty one, then the typed @-mention on
        // the last line: 10 content rows pin the editor at its clamp while the
        // caret keeps a completable @RE context.
        lock_editor(&editor).set_text("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
        type_into(&editor, "@RE");
        assert!(
            lock_editor(&editor).autocomplete_visible(),
            "the @-context must open the popup"
        );
        let mut loader = Loader::new(LOADER_MESSAGE);
        loader.set_active(true);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        draw_frame(&mut terminal, &editor, &loader, true);

        let (top, _bottom) = border_rows(&terminal);
        assert_eq!(top, 1, "streaming max growth leaves a one-row band above");
        assert_eq!(
            row_of(&terminal, "README.md"),
            Some(0),
            "the popup wins the single band row"
        );
        assert_eq!(
            row_of(&terminal, "Working"),
            None,
            "the loader yields to the popup for the frame"
        );
    }

    #[test]
    fn loader_row_shows_above_the_box_while_streaming_and_leaves_no_residue_after() {
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let mut loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);

        // While streaming, the loader is active and its static message is painted
        // in the unbordered row directly ABOVE the box's top border.
        loader.set_active(true);
        draw_frame(&mut terminal, &editor, &loader, true);
        assert!(
            buffer_text(&terminal).contains(LOADER_MESSAGE),
            "loader message must be visible while streaming"
        );
        let (top, _bottom) = border_rows(&terminal);
        assert_eq!(
            row_of(&terminal, "Working").expect("loader row painted"),
            top - 1,
            "the loader row sits directly above the box's top border"
        );

        // After the turn ends, the loader is dismissed: it paints nothing and the
        // fixed-viewport blank repaint wipes the freed row — no "Working…" or
        // border fragment is left behind (the shrink-leak regression).
        loader.set_active(false);
        draw_frame(&mut terminal, &editor, &loader, false);
        let after = buffer_text(&terminal);
        assert!(
            !after.contains(LOADER_MESSAGE),
            "loader message must not linger after dismissal, got:\n{after}"
        );
    }

    /// A footer view-model with recognizable cwd / branch / model text for the
    /// bottom-layout tests.
    fn probe_footer() -> FooterViewModel {
        FooterViewModel {
            cwd: "/tmp/proj".to_string(),
            git_branch: Some("tmp".to_string()),
            model_id: "test-model".to_string(),
            context_window: 100_000,
            context_percent: Some(1.0),
            ..FooterViewModel::default()
        }
    }

    #[test]
    fn footer_fields_render_below_the_box_bottom_border() {
        // The footer view-model's fields paint into the unbordered band BELOW
        // the box's bottom border (the cwd line first, the stats line beneath),
        // visible from the first frame, before any turn.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(80, MAX_VIEWPORT_ROWS);
        draw_frame_with_footer(&mut terminal, &editor, &loader, false, &probe_footer());

        let text = buffer_text(&terminal);
        assert!(text.contains("/tmp/proj"), "cwd missing: {text}");
        assert!(text.contains("(tmp)"), "branch missing: {text}");
        assert!(text.contains("test-model"), "model id missing: {text}");

        // Row order: top border < bottom border < key-hint line < cwd line <
        // stats line — the key-hint row and footer live outside the box, glued
        // directly under it in that order.
        let (top, bottom) = border_rows(&terminal);
        let hint_row = row_of(&terminal, "send").expect("hint row painted");
        let cwd_row = row_of(&terminal, "/tmp/proj").expect("cwd row painted");
        let stats_row = row_of(&terminal, "test-model").expect("stats row painted");
        assert!(top < bottom, "a bordered box is painted");
        assert_eq!(
            hint_row,
            bottom + 1,
            "the key-hint line sits directly below the box's bottom border"
        );
        assert_eq!(
            cwd_row,
            bottom + 2,
            "the cwd line sits below the key-hint line"
        );
        assert_eq!(
            stats_row,
            bottom + 3,
            "the stats line sits below the cwd line"
        );
    }

    #[test]
    fn the_box_wraps_only_the_editor_and_the_footer_stays_glued_below() {
        use hand_tui::rt::components::EditorBorder;

        // The bordered box is exactly the editor rows + the two border rows; it
        // grows and shrinks with the editor while the key-hint row and footer band
        // stay glued below the bottom border (the box bottom stays anchored above
        // them). The cwd line sits two rows below the box bottom: the key-hint row
        // is between them.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new().border(EditorBorder::None)));
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
        let footer = probe_footer();

        // One-row editor: box height 3 (1 editor row + 2 border rows).
        draw_frame_with_footer(&mut terminal, &editor, &loader, false, &footer);
        let (top1, bottom1) = border_rows(&terminal);
        assert_eq!(
            bottom1 - top1,
            2,
            "empty editor: box is 1 editor + 2 border rows"
        );
        assert_eq!(
            row_of(&terminal, "send").expect("hint row"),
            bottom1 + 1,
            "key-hint glued directly below the box"
        );
        assert_eq!(
            row_of(&terminal, "/tmp/proj").expect("cwd row"),
            bottom1 + 2,
            "footer glued below the key-hint row"
        );

        // Three-row editor: the box grows upward to 5 rows, bottom anchored.
        lock_editor(&editor).set_text("alpha\nbravo\ncharlie");
        draw_frame_with_footer(&mut terminal, &editor, &loader, false, &footer);
        let (top2, bottom2) = border_rows(&terminal);
        assert_eq!(
            bottom2 - top2,
            4,
            "3-row editor: box is 3 editor + 2 border rows"
        );
        assert_eq!(
            bottom2, bottom1,
            "the box bottom stays anchored above the hint + footer"
        );
        let alpha_row = row_of(&terminal, "alpha").expect("editor content row");
        assert!(
            top2 < alpha_row && alpha_row < bottom2,
            "editor content renders inside the box (row {alpha_row} in {top2}..{bottom2})"
        );
        assert_eq!(
            row_of(&terminal, "/tmp/proj").expect("cwd row"),
            bottom2 + 2,
            "footer still glued below the hint row after the grow"
        );

        // Shrink back: the box collapses with the editor, hint + footer unmoved.
        lock_editor(&editor).set_text("");
        draw_frame_with_footer(&mut terminal, &editor, &loader, false, &footer);
        let (top3, bottom3) = border_rows(&terminal);
        assert_eq!(bottom3 - top3, 2, "box shrinks back with the editor");
        assert_eq!(
            row_of(&terminal, "/tmp/proj").expect("cwd row"),
            bottom3 + 2,
            "footer still glued below the hint row after the shrink"
        );
    }

    #[test]
    fn max_editor_growth_keeps_box_plus_footer_within_the_viewport_budget() {
        use hand_tui::rt::components::EditorBorder;

        // At the editor's maximum auto-grow the box is trimmed so loader + box +
        // hint + footer never exceed the fixed viewport height — the stats (last)
        // row stays inside MAX_VIEWPORT_ROWS, and while streaming the loader row
        // still has its home directly above the box top.
        for &streaming in &[false, true] {
            let editor: SharedEditor =
                Arc::new(Mutex::new(Editor::new().border(EditorBorder::None)));
            let tall: String = (0..10)
                .map(|i| format!("line-{i}"))
                .collect::<Vec<_>>()
                .join("\n");
            lock_editor(&editor).set_text(&tall);
            let mut loader = Loader::new(LOADER_MESSAGE);
            loader.set_active(streaming);
            let mut terminal = fixed_viewport(60, MAX_VIEWPORT_ROWS);
            draw_frame_with_footer(&mut terminal, &editor, &loader, streaming, &probe_footer());

            let (top, bottom) = border_rows(&terminal);
            if streaming {
                assert_eq!(
                    top, 1,
                    "streaming max growth stops one row short of the origin for the loader"
                );
                assert_eq!(
                    row_of(&terminal, "Working"),
                    Some(0),
                    "the loader row paints in the reserved row above the box top"
                );
            } else {
                assert_eq!(top, 0, "idle max growth reaches the viewport origin");
            }
            let hint_row = row_of(&terminal, "send").expect("hint row painted");
            let cwd_row = row_of(&terminal, "/tmp/proj").expect("cwd row painted");
            let stats_row = row_of(&terminal, "test-model").expect("stats row painted");
            assert_eq!(
                hint_row,
                bottom + 1,
                "key-hint directly below the box (streaming={streaming})"
            );
            assert_eq!(
                cwd_row,
                bottom + 2,
                "footer below the hint row (streaming={streaming})"
            );
            assert_eq!(stats_row, bottom + 3);
            assert!(
                stats_row < MAX_VIEWPORT_ROWS,
                "the whole bottom area fits the fixed viewport budget \
                 (stats row {stats_row} < {MAX_VIEWPORT_ROWS}, streaming={streaming})"
            );
        }
    }

    // --- Overlay panel rendering (M6: panel above the input box) -----------

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

    /// The buffer rows whose cells include the given glyph, in row order — used
    /// to tell the panel's border rows from the box's.
    fn rows_with(terminal: &Terminal<TestBackend>, glyph: &str) -> Vec<u16> {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut rows = Vec::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y))
                    && cell.symbol() == glyph
                {
                    rows.push(y);
                    break;
                }
            }
        }
        rows
    }

    #[test]
    fn overlay_panel_renders_bordered_content_glued_to_its_anchor() {
        // The mounted selector paints as a full-width bordered panel whose
        // bottom edge touches the anchor row (the box top in the driver), with
        // crisp content inside and nothing at or below the anchor touched —
        // and no background dim: the panel covers no transcript.
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new("."));
        let lines = vec![
            Line::from("Search: "),
            Line::from("→ claude-sonnet [anthropic]"),
            Line::from("  gpt-4o [openai]"),
        ];
        let box_top = 18;

        draw_overlay_panel(&mut buf, area, box_top, lines, &ThemePalette::default());

        assert!(has_border(&buf), "a bordered panel must be painted");
        // Panel = 3 content rows + 2 border rows, bottom glued to the anchor:
        // top border row 13, bottom border row 17, full frame width.
        assert_eq!(buf.cell((0, 13)).unwrap().symbol(), "┌");
        assert_eq!(buf.cell((79, 13)).unwrap().symbol(), "┐");
        assert_eq!(
            buf.cell((0, 17)).unwrap().symbol(),
            "└",
            "the panel's bottom border sits directly above the box top"
        );
        // The content is crisp inside the interior.
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
        // Nothing at/below the anchor (the box's rows) is touched, and the rows
        // above the panel keep their base cells undimmed — the transcript dim
        // is gone with the centered dialog.
        assert_eq!(buf.cell((0, 18)).unwrap().symbol(), ".");
        let above = buf.cell((0, 0)).unwrap();
        assert_eq!(above.symbol(), ".");
        assert!(
            !above.modifier.contains(Modifier::DIM),
            "no dim pass over the base"
        );
    }

    #[test]
    fn overlay_panel_clamps_into_a_tiny_band_without_overflowing() {
        // VAL-OVERLAY-020 spirit: with a band shorter than the content, the
        // panel clamps to the band — the border stays on-frame, nothing writes
        // at or past the anchor, and the selector's own list window handles the
        // reduced interior.
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new(" "));
        // A tall content list (more rows than the band) forces the height clamp.
        let lines: Vec<Line<'static>> = (0..20).map(|i| Line::from(format!("model-{i}"))).collect();
        let box_top = 5;

        draw_overlay_panel(&mut buf, area, box_top, lines, &ThemePalette::default());

        assert!(has_border(&buf), "the clamped panel still has a border");
        assert_eq!(
            buf.cell((0, 0)).unwrap().symbol(),
            "┌",
            "the clamped panel takes the whole band"
        );
        assert_eq!(
            buf.cell((0, 4)).unwrap().symbol(),
            "└",
            "the bottom border stays glued to the box top"
        );
        assert_eq!(buf.area, area, "no overflow past the tiny pane");
    }

    #[test]
    fn overlay_panel_on_a_degenerate_band_is_a_silent_noop() {
        // A zero-sized viewport renders nothing rather than panicking.
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        draw_overlay_panel(
            &mut buf,
            area,
            0,
            vec![Line::from("x")],
            &ThemePalette::default(),
        );
        assert_eq!(buf.area.width, 0);

        // A band shorter than MIN_PANEL_ROWS drops the panel for the frame —
        // the box and footer are never squeezed to make room.
        let area = Rect::new(0, 0, 20, 8);
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new("."));
        draw_overlay_panel(
            &mut buf,
            area,
            MIN_PANEL_ROWS - 1,
            vec![Line::from("x")],
            &ThemePalette::default(),
        );
        assert!(!has_border(&buf), "no panel in a band below the minimum");
    }

    #[test]
    fn a_full_frame_glues_the_panel_above_the_box_with_the_footer_clear() {
        // VAL-OVERLAY-009: the panel is part of the frame paint. It sits glued
        // above the box top, spans the frame width, and the footer below stays
        // fully legible — never covered, never dimmed.
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
            .draw(|frame| draw(frame, &snapshot, &editor, &loader, &footer))
            .expect("draw one frame");

        let text = buffer_text(&terminal);
        assert!(text.contains("claude-sonnet"), "overlay content: {text}");
        assert!(text.contains("base-model"), "footer legible: {text}");

        // Row order: panel top < content < panel bottom, panel bottom + 1 ==
        // box top (glued, no overlap), footer below the box bottom.
        let opens = rows_with(&terminal, "┌");
        let closes = rows_with(&terminal, "└");
        assert_eq!(opens.len(), 2, "panel + box top borders: {opens:?}");
        assert_eq!(closes.len(), 2, "panel + box bottom borders: {closes:?}");
        let (panel_top, box_top) = (opens[0], opens[1]);
        let (panel_bottom, box_bottom) = (closes[0], closes[1]);
        let content_row = row_of(&terminal, "claude-sonnet").expect("panel content row");
        assert!(panel_top < content_row && content_row < panel_bottom);
        assert_eq!(
            panel_bottom + 1,
            box_top,
            "the panel is glued directly above the box"
        );
        let footer_row = row_of(&terminal, "base-model").expect("footer stats row");
        assert!(footer_row > box_bottom, "footer below the box, uncovered");
        // Full width: the panel's corners land on the frame's first and last
        // columns, aligned with the box below.
        assert_eq!(edge_cols(&terminal, panel_top), vec![0, width - 1]);
        assert_eq!(edge_cols(&terminal, box_top), vec![0, width - 1]);
    }

    #[test]
    fn small_pane_panel_clamps_while_box_and_footer_stay_intact() {
        // 40x10 pane: too short to grow (the desired height floors at the
        // default), so the panel clamps into the band above the box while the
        // box and footer keep every row they had.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let loader = Loader::new(LOADER_MESSAGE);
        let mut terminal = fixed_viewport(40, 10);
        let lines: Vec<Line<'static>> = (0..8).map(|i| Line::from(format!("item-{i}"))).collect();
        let snapshot = StateSnapshot {
            size: TerminalSize::new(40, 10),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: true,
            overlay_lines: Some(lines),
            palette: ThemePalette::default(),
        };
        terminal
            .draw(|frame| draw(frame, &snapshot, &editor, &loader, &probe_footer()))
            .expect("draw one frame");

        let opens = rows_with(&terminal, "┌");
        let closes = rows_with(&terminal, "└");
        assert_eq!(opens.len(), 2, "panel + box painted: {opens:?}");
        assert_eq!(
            closes[0] + 1,
            opens[1],
            "the clamped panel stays glued to the box top"
        );
        // The clamped interior still shows the head of the list.
        assert!(
            buffer_text(&terminal).contains("item-0"),
            "the clamped panel shows list rows"
        );
        // Box + footer intact inside the 10-row pane: the footer's two rows sit
        // below the box bottom and inside the frame.
        let cwd_row = row_of(&terminal, "/tmp/proj").expect("cwd row painted");
        let stats_row = row_of(&terminal, "test-model").expect("stats row painted");
        assert_eq!(cwd_row, closes[1] + 1, "footer glued below the box");
        assert!(stats_row < 10, "everything fits the 10-row pane");
    }

    #[test]
    fn desired_viewport_rows_budgets_panel_box_footer_with_cap_and_ratchet() {
        // Closed → always the fixed default (closing a selector shrinks back).
        assert_eq!(desired_viewport_rows(21, None, 24), MAX_VIEWPORT_ROWS);
        // Open: content + panel borders + collapsed box + footer.
        assert_eq!(desired_viewport_rows(MAX_VIEWPORT_ROWS, Some(14), 24), 21);
        // Capped at the terminal height minus the transcript margin.
        assert_eq!(desired_viewport_rows(MAX_VIEWPORT_ROWS, Some(30), 24), 22);
        // A short selector never shrinks the viewport below the default.
        assert_eq!(
            desired_viewport_rows(MAX_VIEWPORT_ROWS, Some(1), 24),
            MAX_VIEWPORT_ROWS
        );
        // On a pane too short to grow, the default holds — the panel clamps
        // itself into the band instead (the small-terminal rule).
        assert_eq!(
            desired_viewport_rows(MAX_VIEWPORT_ROWS, Some(14), 10),
            MAX_VIEWPORT_ROWS
        );
        // Mid-overlay ratchet: filtering the list down never shrinks (no
        // rebuild churn per keystroke)…
        assert_eq!(desired_viewport_rows(21, Some(2), 24), 21);
        // …but a terminal shrink re-clamps a grown viewport downward.
        assert_eq!(desired_viewport_rows(21, Some(14), 16), 14);
    }

    #[test]
    fn overlay_mount_grows_the_viewport_and_unmount_shrinks_it_back_ghost_free() {
        use hand_tui::rt::session::set_inline_viewport_height;

        // The scheduler's rebuild sequence against a TestBackend: mount grows
        // the inline viewport to the desired height, the panel then has room
        // for the selector's whole window; unmount shrinks it back with the
        // bottom edge pinned and leaves no panel residue anywhere on screen.
        let editor: SharedEditor = Arc::new(Mutex::new(Editor::new()));
        let loader = Loader::new(LOADER_MESSAGE);
        let footer = probe_footer();
        let mut terminal = fixed_viewport(60, 24);
        assert_eq!(terminal.get_frame().area().height, MAX_VIEWPORT_ROWS);

        // Mount: 8 content rows → panel 10 → panel + box + footer = 15.
        let lines: Vec<Line<'static>> = (0..8).map(|i| Line::from(format!("item-{i}"))).collect();
        let target = desired_viewport_rows(MAX_VIEWPORT_ROWS, Some(lines.len() as u16), 24);
        assert_eq!(target, 15, "panel(10) + collapsed box(3) + footer(2)");
        set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), target)
            .expect("grow the viewport at mount");
        assert_eq!(terminal.get_frame().area().height, target);

        let snapshot = StateSnapshot {
            size: TerminalSize::new(60, 24),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: true,
            overlay_lines: Some(lines),
            palette: ThemePalette::default(),
        };
        terminal
            .draw(|frame| draw(frame, &snapshot, &editor, &loader, &footer))
            .expect("draw the open frame");

        // The grown band fits the whole list — every row is visible at once.
        let text = buffer_text(&terminal);
        for i in 0..8 {
            assert!(
                text.contains(&format!("item-{i}")),
                "row {i} visible: {text}"
            );
        }
        let opens = rows_with(&terminal, "┌");
        let closes = rows_with(&terminal, "└");
        assert_eq!(
            closes[0] + 1,
            opens[1],
            "panel glued above the box in the grown viewport"
        );

        // Unmount: shrink back to the default, bottom edge pinned.
        let shrink = desired_viewport_rows(target, None, 24);
        assert_eq!(shrink, MAX_VIEWPORT_ROWS);
        set_inline_viewport_height(&mut terminal, TestBackend::new(1, 1), shrink)
            .expect("shrink the viewport at unmount");
        let area = terminal.get_frame().area();
        assert_eq!(
            area.y,
            target - MAX_VIEWPORT_ROWS,
            "the shrunk viewport keeps its bottom edge pinned"
        );

        let closed = StateSnapshot {
            size: TerminalSize::new(60, 24),
            loader: false,
            loader_message: None,
            preview: Vec::new(),
            overlay_open: false,
            overlay_lines: None,
            palette: ThemePalette::default(),
        };
        terminal
            .draw(|frame| draw(frame, &closed, &editor, &loader, &footer))
            .expect("draw the closed frame");

        // Ghost-free close: no panel content or extra border anywhere on the
        // whole screen — the erase-first shrink wiped the grown band.
        let after = buffer_text(&terminal);
        assert!(!after.contains("item-"), "no panel residue: {after}");
        assert_eq!(
            rows_with(&terminal, "┌").len(),
            1,
            "only the box border remains after close"
        );
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
    fn overlay_replaces_the_loader_band_and_streaming_chrome_resumes_on_close() {
        // VAL-CROSS-002 (draw-layer slice): while a turn streams, a mounted
        // overlay's panel replaces the loader/preview band entirely — the turn
        // itself never pauses (its commits keep settling into scrollback, a
        // path independent of this draw layout) — and the loader + preview
        // chrome resume the moment the overlay closes. There is no background
        // dim anymore: the panel covers no content.
        let editor: SharedEditor = Arc::new(Mutex::new(
            Editor::new().border(hand_tui::rt::components::EditorBorder::None),
        ));
        let mut loader = Loader::new(LOADER_MESSAGE);
        loader.set_active(true);
        let mut terminal = fixed_viewport(80, MAX_VIEWPORT_ROWS);
        let open = StateSnapshot {
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
            .draw(|frame| draw(frame, &open, &editor, &loader, &footer))
            .expect("draw the open frame");

        let text = buffer_text(&terminal);
        assert!(text.contains("claude-sonnet"), "panel on top: {text}");
        assert!(
            !text.contains(LOADER_MESSAGE),
            "the loader yields its band to the panel: {text}"
        );
        assert!(
            !text.contains("streaming reply"),
            "the preview yields its band to the panel: {text}"
        );
        let buf = terminal.backend().buffer();
        let corner = buf.cell((0, buf.area.height - 1)).unwrap();
        assert!(
            !corner.modifier.contains(Modifier::DIM),
            "no base dim under the panel layout"
        );

        // Close the overlay mid-stream: the loader row and preview return on
        // the very next frame.
        let closed = StateSnapshot {
            overlay_open: false,
            overlay_lines: None,
            ..open
        };
        terminal
            .draw(|frame| draw(frame, &closed, &editor, &loader, &footer))
            .expect("draw the closed frame");
        let after = buffer_text(&terminal);
        assert!(
            after.contains(LOADER_MESSAGE),
            "the loader resumes when the overlay closes: {after}"
        );
        assert!(
            after.contains("streaming reply"),
            "the preview resumes when the overlay closes: {after}"
        );
        assert!(
            !after.contains("claude-sonnet"),
            "no panel residue after close: {after}"
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
