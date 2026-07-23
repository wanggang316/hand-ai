//! Minimal inline terminal-session demo for the ratatui runtime.
//!
//! Launches an inline viewport (never the alternate screen) so the shell
//! content above stays visible, then draws a bordered input line at the bottom
//! that echoes what you type. It exercises the full session lifecycle
//! (raw mode, bracketed paste, optional kitty keyboard flags, restoration on
//! every exit path), the rt input pipeline (crossterm events → `RtInputEvent`
//! over an mpsc channel), and the **frame scheduler**: input events never draw
//! directly, they only mutate state and call `request_frame()`. All painting
//! happens in one place — the scheduler's draw closure — wrapped in
//! synchronized-output markers.
//!
//! Keys:
//!   - printable chars / Backspace: edit the input line
//!   - Enter: clear the input line (submit placeholder)
//!   - `f`: start/stop the token flood — a background task that calls
//!     `request_frame()` hundreds of times per second and pushes lines into a
//!     shared buffer, so the scheduler's coalescing and rate-limiting can be
//!     probed (VAL-CORE-004). The flood also starts automatically when the
//!     `HAND_TUI_RT_DEMO_FLOOD` env var is set to `1`.
//!   - `s`: start a **streaming block** — a styled block grows line-by-line in
//!     the live viewport (simulating a token stream); when it finishes it is
//!     committed **exactly once** into native scrollback, seeded with a
//!     `⟦commit N⟧` boundary marker so a validator can diff scrollback
//!     deterministically (VAL-CORE-002/006/034).
//!   - `b`: commit an **oversized block** — a single block ~2× the terminal
//!     height, to confirm a tall block lands complete and ordered in scrollback
//!     (VAL-CORE-033). Also seeded with a `⟦commit N⟧` marker.
//!   - `g`: **grow** the input body by one line (auto-grow simulation), from 1
//!     row up to the 8-row ceiling. The inline viewport is *fixed* at its max
//!     height (ratatui#984 strategy B), so growing only enlarges the active area
//!     inside the viewport — it never enlarges the viewport and never eats the
//!     history above it (VAL-CORE-007).
//!   - `c`: **collapse** the input body back to a single row and hide the loader.
//!     Because every draw repaints the whole fixed viewport, the freed rows come
//!     back blank — no border/spinner ghost on screen or in scrollback
//!     (VAL-CORE-008).
//!   - `l`: toggle the **loader** row (a spinner above the input). Loading and
//!     unloading it changes the bottom-area height without moving the viewport.
//!   - `G`: **grow + collapse during streaming** — start a stream, grow to 8
//!     rows with the loader on, then collapse mid-stream, so the height changes
//!     while content is being committed to scrollback (VAL-CORE-035).
//!   - Ctrl+D or Ctrl+C: quit cleanly (terminal fully restored)
//!   - Ctrl+Z: ignored (no suspend; the UI does not move)
//!   - F12: DELIBERATE PANIC — crashes on purpose so you can confirm the panic
//!     path still leaves a readable, usable terminal (VAL-CORE-018)
//!
//! Paste (bracketed paste): a multi-line paste lands as a single event and is
//! inserted whole — it never fires per-character key actions.
//!
//! Resize: a `RtInputEvent::Resize` only folds the new `(cols, rows)` into the
//! tracked size and requests a frame — it never re-lays-out synchronously. The
//! scheduler coalesces a resize storm into one re-anchoring draw, and the next
//! draw autoresizes the terminal so the bottom area re-wraps to the new width and
//! any block committed afterwards wraps to the new width too (VAL-CORE-009/010/
//! 011/021). Set `HAND_TUI_RT_DEMO_STREAM=1` to auto-run a continuous stream so a
//! probe can resize while content is perpetually committing to scrollback.
//!
//! Run it:
//!   cargo run -p hand-tui --example rt_demo
//!   HAND_TUI_RT_DEMO_FLOOD=1 cargo run -p hand-tui --example rt_demo
//!   HAND_TUI_RT_DEMO_STREAM=1 cargo run -p hand-tui --example rt_demo
//!   HAND_TUI_FORCE_KITTY_KEYBOARD=1 cargo run -p hand-tui --example rt_demo
//!
//! On a non-TTY (piped) stdin/stdout it prints a diagnostic to stderr and
//! exits non-zero without ever touching the parent shell's terminal mode.

use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use hand_tui::rt::events::{RtInputEvent, spawn_event_pump};
use hand_tui::rt::history::HistorySink;
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{SessionError, SessionGuard, SessionTerminal};
use hand_tui::rt::view::{MAX_INPUT_ROWS, MIN_INPUT_ROWS, TerminalSize, bottom_area_geometry};
use ratatui::layout::Alignment;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

/// The key that intentionally panics, for exercising the panic-restore path.
const PANIC_KEY_HELP: &str = "F12 = deliberate panic";

/// Bound on the event channel: a small buffer is plenty for interactive typing;
/// backpressure just makes the pump await, which is fine.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Environment variable that auto-starts the token flood at launch, so a probe
/// harness need not synthesize an `f` keypress.
const FLOOD_ENV: &str = "HAND_TUI_RT_DEMO_FLOOD";

/// Environment variable that auto-runs a *continuous* streaming loop at launch:
/// stream a block, commit it, immediately start the next. It lets a resize probe
/// drive `tmux resize-window` while content is perpetually committing to
/// scrollback (mid-stream resize / storm-while-streaming) without synthesizing
/// `s` keypresses. Set to `1` to enable.
const STREAM_ENV: &str = "HAND_TUI_RT_DEMO_STREAM";

/// How often the flood task requests a frame: ~500/s, deliberately far above the
/// scheduler's ~60fps ceiling so coalescing and rate-limiting are exercised.
const FLOOD_REQUEST_INTERVAL: Duration = Duration::from_micros(2_000);

/// Cadence of the streaming-block simulator: one new line every ~80ms, slow
/// enough that a tmux probe can capture the block mid-stream (in the viewport,
/// not yet in scrollback) and then again after it commits.
const STREAM_LINE_INTERVAL: Duration = Duration::from_millis(80);

/// Number of lines in a streaming block. Includes CJK/emoji content wider than a
/// narrow pane so the width-aware wrap is exercised on commit.
const STREAM_BLOCK_LINES: usize = 6;

/// Multiplier for the oversized block: this many times the terminal height, so
/// the committed block is guaranteed to be much taller than the viewport
/// (VAL-CORE-033).
const OVERSIZED_HEIGHT_MULTIPLIER: usize = 2;

/// Mutable demo state, shared between the input loop (which mutates it) and the
/// scheduler's draw closure (which reads it). A plain `std::sync::Mutex`: the
/// draw closure the scheduler runs is synchronous, and every critical section
/// here is a tiny, non-awaiting field access — so a blocking mutex is both
/// correct (no `blocking_lock` inside the async runtime) and simplest.
#[derive(Debug, Default)]
struct DemoState {
    /// The current terminal geometry, tracked from `RtInputEvent::Resize`.
    ///
    /// Seeded at launch from the real terminal size and overwritten whole on
    /// every resize event. The draw closure lays the fixed bottom-area viewport
    /// out against it, so a narrow/widen re-anchors and re-wraps to the new size
    /// on the next coalesced frame — the input side never re-lays-out
    /// synchronously, it just updates this and requests a frame.
    size: TerminalSize,
    /// The current input-line contents.
    input: String,
    /// How many rows the input body currently occupies (auto-grow simulation).
    /// Starts at one, grows one row per `g` up to the 8-row ceiling, and
    /// collapses back to one on `c`. Drives the fixed-viewport bottom-area
    /// geometry: the viewport never changes, only this interior size does.
    input_rows: u16,
    /// Whether the loader/spinner row is showing above the input body.
    loader: bool,
    /// Spinner animation phase, advanced each frame while the loader shows.
    spinner_phase: u64,
    /// A monotonically increasing counter the flood task advances, shown so a
    /// probe can confirm content keeps moving while draws stay rate-limited.
    flood_ticks: u64,
    /// Whether the flood task is currently running.
    flooding: bool,
    /// The lines of the in-progress streaming block, as they arrive. Rendered
    /// live in the viewport while the block grows; moved to `pending_commits`
    /// (and cleared) the moment the block finishes, so the block is committed to
    /// scrollback **exactly once**.
    stream_block: Vec<Line<'static>>,
    /// Whether a streaming block is currently growing.
    streaming: bool,
    /// How many blocks have been committed so far, used to number the
    /// `⟦commit N⟧` boundary marker each commit seeds.
    commit_count: u64,
    /// Blocks finished and awaiting a single `insert_before`. The scheduler's
    /// draw path drains this (calling the [`HistorySink`]) *before* it redraws
    /// the viewport, honouring the "insert_before between draws" ordering.
    pending_commits: Vec<Vec<Line<'static>>>,
}

fn main() -> ExitCode {
    if wants_help() {
        print_help();
        return ExitCode::SUCCESS;
    }

    // The rt input pipeline drives crossterm's async EventStream and the frame
    // scheduler is an async actor, so the demo needs a tokio runtime.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("rt_demo: failed to start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(SessionError::NotATty) => {
            eprintln!(
                "rt_demo: standard input/output is not a terminal (TTY).\n\
                 Run this from an interactive terminal, e.g. `cargo run -p hand-tui --example rt_demo`."
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("rt_demo: {err}");
            ExitCode::FAILURE
        }
    }
}

fn wants_help() -> bool {
    std::env::args()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "-h")
}

fn print_help() {
    println!(
        "rt_demo — inline terminal session demo (rt input + frame scheduler)\n\
         \n\
         Inline viewport (no alternate screen); prior shell content stays visible.\n\
         Input events only request frames; all drawing is coalesced + rate-limited.\n\
         \n\
         Keys:\n\
         \x20 printable / Backspace : edit the input line\n\
         \x20 Enter                 : clear the input line\n\
         \x20 f                     : toggle the token flood (probe rate-limiting)\n\
         \x20 s                     : stream a block, then commit it once to scrollback\n\
         \x20 b                     : commit an oversized (~2x height) block to scrollback\n\
         \x20 g                     : grow the input body by one row (up to 8)\n\
         \x20 c                     : collapse the input to one row and hide the loader\n\
         \x20 l                     : toggle the loader/spinner row\n\
         \x20 G (shift+g)           : grow + collapse during streaming\n\
         \x20 Ctrl+D or Ctrl+C      : quit cleanly\n\
         \x20 Ctrl+Z                : ignored (no suspend)\n\
         \x20 paste                 : multi-line paste inserted as one event\n\
         \x20 {PANIC_KEY_HELP} (crashes on purpose; terminal stays readable)\n\
         \n\
         Env:\n\
         \x20 {FLOOD_ENV}=1 : start the flood automatically at launch\n\
         \x20 {STREAM_ENV}=1 : run a continuous stream (probe resize while committing)"
    );
}

async fn run() -> Result<(), SessionError> {
    // Establishing the guard verifies stdin/stdout are TTYs *before* toggling
    // raw mode, so a non-interactive launch leaves the shell untouched.
    let mut guard = SessionGuard::enter()?;
    let terminal = guard.terminal()?;

    // Seed the tracked size from the real terminal so the first frame lays out
    // against the actual geometry; resize events overwrite it thereafter.
    let (init_cols, init_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let state = Arc::new(Mutex::new(DemoState {
        input_rows: MIN_INPUT_ROWS,
        size: TerminalSize::new(init_cols, init_rows),
        ..DemoState::default()
    }));

    // Spawn the frame scheduler: it owns the terminal and is the *single* place
    // the UI is painted. Every draw is wrapped in synchronized-output markers,
    // so an exit mid-flood can never leave an unterminated `?2026h`.
    let (requester, scheduler) = spawn_scheduler(terminal, state.clone());

    // Spawn the rt input pump: it reads crossterm's EventStream, translates each
    // event, and delivers RtInputEvents over the channel.
    let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

    // The streaming-block simulator task, when one is running.
    let mut stream_handle: Option<tokio::task::JoinHandle<()>> = None;

    // Optional auto-flood so a probe harness need not synthesize a keypress.
    let flood_stop = Arc::new(AtomicBool::new(false));
    let mut flood_handle: Option<tokio::task::JoinHandle<()>> = None;
    if flood_requested_by_env() {
        set_flooding(&state, true);
        flood_handle = Some(spawn_flood(
            requester.clone(),
            state.clone(),
            flood_stop.clone(),
        ));
    }

    // Optional continuous streaming loop so a resize probe can drive
    // `tmux resize-window` while content is perpetually committing to scrollback
    // (mid-stream resize / storm-while-streaming) without synthesizing keys.
    let continuous_stop = Arc::new(AtomicBool::new(false));
    let mut continuous_handle: Option<tokio::task::JoinHandle<()>> = None;
    if continuous_stream_requested_by_env() {
        continuous_handle = Some(spawn_continuous_stream(
            requester.clone(),
            state.clone(),
            continuous_stop.clone(),
        ));
    }

    // Initial paint.
    requester.request_frame();

    while let Some(event) = events.recv().await {
        let mut quit = false;
        match event {
            RtInputEvent::Key(key) => match key.key_id.as_deref() {
                // Ctrl+D / Ctrl+C: clean quit (guard restores on Drop).
                Some("ctrl+d" | "ctrl+c") => quit = true,
                // Deliberate panic to exercise the panic-restore path.
                Some("f12") => panic!("rt_demo: deliberate panic (F12) for VAL-CORE-018"),
                // Ctrl+Z is intentionally a no-op: no SIGTSTP, UI unchanged.
                Some("ctrl+z") => {}
                // Toggle the token flood.
                Some("f") => {
                    toggle_flood(&requester, &state, &flood_stop, &mut flood_handle);
                }
                // Start a streaming block (ignored if one is already growing).
                Some("s") => {
                    start_stream(&requester, &state, &mut stream_handle);
                }
                // Queue an oversized (~2× terminal height) block for one commit.
                Some("b") => {
                    queue_oversized_block(&requester, &state);
                }
                // Grow the input body by one row (auto-grow simulation), capped
                // at the 8-row ceiling.
                Some("g") => {
                    let mut guard = lock(&state);
                    guard.input_rows = (guard.input_rows + 1).min(MAX_INPUT_ROWS);
                }
                // Collapse the input body back to one row and hide the loader.
                Some("c") => {
                    let mut guard = lock(&state);
                    guard.input_rows = MIN_INPUT_ROWS;
                    guard.loader = false;
                }
                // Toggle the loader/spinner row.
                Some("l") => {
                    let mut guard = lock(&state);
                    guard.loader = !guard.loader;
                }
                // Grow + collapse *during* streaming: kick off a stream (if none
                // is running) and a task that grows to 8 rows with the loader,
                // then collapses mid-stream, so the bottom-area height changes
                // while content is being committed to scrollback (VAL-CORE-035).
                Some("shift+g") => {
                    start_stream(&requester, &state, &mut stream_handle);
                    spawn_grow_collapse(&requester, &state);
                }
                Some("backspace") => {
                    lock(&state).input.pop();
                }
                Some("enter") => lock(&state).input.clear(),
                Some("space") => lock(&state).input.push(' '),
                // A printable character with no ctrl/alt/super modifier.
                _ => {
                    if let Some(ch) = printable_char(&key) {
                        lock(&state).input.push(ch);
                    }
                }
            },
            // Bracketed paste: insert the whole payload as one action.
            RtInputEvent::Paste(payload) => lock(&state).input.push_str(&payload),
            // Resize: fold the whole new geometry into the tracked size. The
            // work stops here — we do *not* re-lay-out synchronously. The shared
            // `request_frame()` below routes through the scheduler, so a resize
            // storm coalesces into a single re-anchoring draw (VAL-CORE-021),
            // and the draw closure re-derives geometry from the updated size.
            RtInputEvent::Resize { cols, rows } => {
                // The changed-flag is informational here: the shared
                // `request_frame()` below runs unconditionally and the scheduler
                // coalesces, so a redundant same-size event costs only one
                // already-coalesced frame, not a reflow.
                let _ = lock(&state).size.apply_resize(cols, rows);
            }
            // Focus changes: just repaint (nothing to track).
            RtInputEvent::FocusGained | RtInputEvent::FocusLost => {}
        }

        // Input side never draws directly: it mutates state and requests a
        // frame. The scheduler decides when (and whether) to actually paint.
        if !quit {
            requester.request_frame();
        }

        if quit {
            break;
        }
    }

    // Stop the flood task, if running, before tearing anything down.
    flood_stop.store(true, Ordering::SeqCst);
    if let Some(handle) = flood_handle.take() {
        handle.abort();
    }
    // Stop the continuous-stream driver, if running.
    continuous_stop.store(true, Ordering::SeqCst);
    if let Some(handle) = continuous_handle.take() {
        handle.abort();
    }
    // Stop the streaming-block task, if running: clear the flag so a task racing
    // to append abandons, then abort it.
    lock(&state).streaming = false;
    if let Some(handle) = stream_handle.take() {
        handle.abort();
    }

    // Drop the last requester so the scheduler drains any final frame and stops;
    // then wait for it so the terminal is released before we restore. This is
    // the interrupt-safety ordering: the scheduler always closes its last
    // synchronized block on the way out.
    drop(requester);
    let _ = scheduler.await;

    // Stop the input pump.
    pump.abort();

    // Explicit restore before returning; Drop would also do it.
    guard.restore();
    Ok(())
}

/// Spawn the frame scheduler over the session terminal.
///
/// The returned closure is the one and only painter: it locks the shared state
/// and draws the viewport, wrapped in balanced synchronized-output markers via
/// [`draw_synchronized`]. Because the scheduler coalesces and rate-limits, a
/// token flood requesting hundreds of frames per second still paints at most
/// ~60fps.
fn spawn_scheduler(
    mut terminal: SessionTerminal,
    state: Arc<Mutex<DemoState>>,
) -> (FrameRequester, tokio::task::JoinHandle<io::Result<()>>) {
    // The scheduler task owns the terminal, so it is the only place `insert_before`
    // may be called — and it must be called *between* viewport draws. We drain the
    // finished-block queue here, commit each block once, then draw the viewport.
    let mut history = HistorySink::new();
    FrameScheduler::spawn(move || {
        // Snapshot the state under the lock, then release it before touching the
        // terminal so the critical section stays tiny. Any finished blocks are
        // taken out (not cloned) so they can only ever be committed once.
        let (snapshot, commits) = {
            let mut guard = state.lock().expect("demo state mutex poisoned");
            let commits = std::mem::take(&mut guard.pending_commits);
            // Advance the spinner while the loader shows, so a probe sees it move.
            if guard.loader {
                guard.spinner_phase = guard.spinner_phase.wrapping_add(1);
            }
            let snapshot = StateSnapshot {
                size: guard.size,
                input: guard.input.clone(),
                input_rows: guard.input_rows,
                loader: guard.loader,
                spinner_phase: guard.spinner_phase,
                flood_ticks: guard.flood_ticks,
                flooding: guard.flooding,
                stream_block: guard.stream_block.clone(),
                streaming: guard.streaming,
            };
            (snapshot, commits)
        };

        // Commit finished blocks into native scrollback *before* the draw. Each
        // block becomes exactly one `insert_before`; the sink autoresizes then
        // pre-wraps to the *current* width, so a block committed right after a
        // resize wraps to the new width (VAL-CORE-009/010), and a tall or wide
        // block lands complete and ordered.
        for block in commits {
            history.commit_lines(&mut terminal, block)?;
        }

        // Wrap the whole paint in BSU/ESU. `draw_synchronized` guarantees the
        // closing `?2026l` is emitted even if `terminal.draw` errors, so an
        // interrupt mid-draw never leaves an open synchronized block.
        let mut stdout = io::stdout();
        draw_synchronized(&mut stdout, |_w| {
            terminal.draw(|frame| draw(frame, &snapshot))?;
            Ok(())
        })
    })
}

/// An immutable view of [`DemoState`] handed to the draw function, taken under
/// the lock so the paint itself holds no lock.
struct StateSnapshot {
    /// The tracked terminal geometry this frame lays out against.
    size: TerminalSize,
    input: String,
    /// The auto-grow input body height in rows (1..8).
    input_rows: u16,
    /// Whether the loader/spinner row shows this frame.
    loader: bool,
    /// Spinner animation phase for the loader glyph.
    spinner_phase: u64,
    flood_ticks: u64,
    flooding: bool,
    /// The streaming block's lines so far, shown live in the viewport while the
    /// block grows.
    stream_block: Vec<Line<'static>>,
    /// Whether a streaming block is currently growing.
    streaming: bool,
}

/// Toggle the token flood on or off in response to the `f` key.
fn toggle_flood(
    requester: &FrameRequester,
    state: &Arc<Mutex<DemoState>>,
    flood_stop: &Arc<AtomicBool>,
    flood_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    let currently = lock(state).flooding;
    if currently {
        // Stop: signal the task, abort it, mark state.
        flood_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = flood_handle.take() {
            handle.abort();
        }
        set_flooding(state, false);
    } else {
        // Start: fresh stop flag, spawn the task, mark state.
        flood_stop.store(false, Ordering::SeqCst);
        set_flooding(state, true);
        *flood_handle = Some(spawn_flood(
            requester.clone(),
            state.clone(),
            flood_stop.clone(),
        ));
    }
}

/// Spawn the flood task: at a high frequency, advance the shared counter and
/// request a frame. This is the load generator the scheduler is meant to tame —
/// it deliberately requests far faster than the ~60fps draw ceiling.
fn spawn_flood(
    requester: FrameRequester,
    state: Arc<Mutex<DemoState>>,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(FLOOD_REQUEST_INTERVAL);
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            interval.tick().await;
            {
                let mut guard = lock(&state);
                if !guard.flooding {
                    break;
                }
                guard.flood_ticks = guard.flood_ticks.wrapping_add(1);
            }
            requester.request_frame();
        }
    })
}

/// Set the `flooding` flag under the lock.
fn set_flooding(state: &Arc<Mutex<DemoState>>, on: bool) {
    lock(state).flooding = on;
}

/// Drive a *continuous* streaming loop: stream one block line-by-line, commit it
/// once to scrollback, then immediately start the next. It never stalls, so a
/// resize probe can drive `tmux resize-window` at any moment and observe both
/// the in-flight viewport block and the ordered, new-width-wrapped scrollback
/// commits (VAL-CORE-010 mid-stream resize, VAL-CORE-021 storm-while-streaming).
///
/// Each block reuses the same seed/commit/queue path a single `s` stream takes,
/// so the exactly-once commit and `⟦commit N⟧` ordering guarantees hold
/// unchanged; only the driver differs (a loop instead of one shot).
fn spawn_continuous_stream(
    requester: FrameRequester,
    state: Arc<Mutex<DemoState>>,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(STREAM_LINE_INTERVAL);
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            // Reserve this block's commit number and mark streaming. If a manual
            // `s` stream is already running, back off a tick and retry so the two
            // never interleave into one block.
            let commit_number = {
                let mut guard = lock(&state);
                if guard.streaming {
                    None
                } else {
                    guard.streaming = true;
                    guard.stream_block.clear();
                    guard.commit_count += 1;
                    Some(guard.commit_count)
                }
            };
            let Some(commit_number) = commit_number else {
                interval.tick().await;
                continue;
            };

            let block = streaming_block_lines(commit_number);
            // Stream the block one line at a time, repainting after each so it is
            // capturable mid-stream in the viewport before it commits.
            for line in &block {
                interval.tick().await;
                if stop.load(Ordering::SeqCst) {
                    lock(&state).streaming = false;
                    return;
                }
                lock(&state).stream_block.push(line.clone());
                requester.request_frame();
            }
            // Finished: move the block to the commit queue in one shot so it
            // commits exactly once, clear the live block, drop the flag.
            {
                let mut guard = lock(&state);
                guard.pending_commits.push(block);
                guard.stream_block.clear();
                guard.streaming = false;
            }
            requester.request_frame();
        }
    })
}

/// Build one streaming block's worth of lines, with a distinctive commit marker
/// as its first line and a mix of content — plain, styled, and wide
/// (CJK/emoji/flag) — so a commit exercises the width-aware wrap and the
/// no-attribute-leak guarantee. `commit_number` seeds the `⟦commit N⟧` marker.
fn streaming_block_lines(commit_number: u64) -> Vec<Line<'static>> {
    let mut lines = vec![commit_marker(commit_number)];
    for i in 0..STREAM_BLOCK_LINES {
        lines.push(
            Line::from(format!("stream line {i} · 你好世界🎉🇨🇳 tail"))
                .style(Style::default().fg(Color::Cyan)),
        );
    }
    lines
}

/// The boundary marker seeded at the top of every committed block. A validator
/// diffs scrollback against these to confirm each block committed exactly once,
/// in order. Deliberately styled so the style-leak probe has a coloured anchor.
fn commit_marker(commit_number: u64) -> Line<'static> {
    Line::from(format!("⟦commit {commit_number}⟧"))
        .style(Style::default().fg(Color::Magenta).bold())
}

/// Start a streaming block: reserve the next commit number, spawn the simulator
/// task that appends one line at a time and, when done, hands the finished block
/// to the commit queue exactly once. A no-op if a block is already streaming.
fn start_stream(
    requester: &FrameRequester,
    state: &Arc<Mutex<DemoState>>,
    stream_handle: &mut Option<tokio::task::JoinHandle<()>>,
) {
    {
        let mut guard = lock(state);
        if guard.streaming {
            return;
        }
        guard.streaming = true;
        guard.stream_block.clear();
    }
    let commit_number = {
        let mut guard = lock(state);
        guard.commit_count += 1;
        guard.commit_count
    };

    let requester = requester.clone();
    let state = state.clone();
    *stream_handle = Some(tokio::spawn(async move {
        let block = streaming_block_lines(commit_number);
        let mut interval = tokio::time::interval(STREAM_LINE_INTERVAL);
        // Grow the block one line at a time, repainting after each so the block
        // is visibly streaming in the viewport (and thus capturable mid-stream).
        for line in &block {
            interval.tick().await;
            {
                let mut guard = lock(&state);
                if !guard.streaming {
                    // Cancelled (e.g. quit): abandon without committing.
                    return;
                }
                guard.stream_block.push(line.clone());
            }
            requester.request_frame();
        }
        // Finished: move the whole block to the commit queue in one shot, clear
        // the live block, and drop the streaming flag — so it commits exactly
        // once and no partial state lingers in the viewport.
        {
            let mut guard = lock(&state);
            guard.pending_commits.push(block);
            guard.stream_block.clear();
            guard.streaming = false;
        }
        requester.request_frame();
    }));
}

/// Interval between grow steps in the grow+collapse-during-streaming task, tuned
/// so the whole grow -> collapse sweep overlaps the ~0.5s stream.
const GROW_STEP_INTERVAL: Duration = Duration::from_millis(60);

/// Drive a grow-to-8-then-collapse sweep of the bottom area *while a stream is in
/// flight*, so the fixed viewport's active height changes as content is being
/// committed to scrollback. This is the VAL-CORE-035 stressor: the stream must
/// keep committing in order with no loss/dup/stall while the height moves.
///
/// It turns the loader on, grows the input one row per tick up to the ceiling,
/// holds briefly, then collapses to a single row and drops the loader — the same
/// shrink path a real loader-unload takes. It never touches the stream's own
/// state, so the two proceed independently over the shared `request_frame`.
fn spawn_grow_collapse(requester: &FrameRequester, state: &Arc<Mutex<DemoState>>) {
    let requester = requester.clone();
    let state = state.clone();
    tokio::spawn(async move {
        {
            let mut guard = lock(&state);
            guard.loader = true;
        }
        requester.request_frame();

        let mut interval = tokio::time::interval(GROW_STEP_INTERVAL);
        // Grow one row per tick up to the ceiling.
        for rows in (MIN_INPUT_ROWS + 1)..=MAX_INPUT_ROWS {
            interval.tick().await;
            lock(&state).input_rows = rows;
            requester.request_frame();
        }
        // Hold at full height for a couple of ticks so a probe can capture the
        // grown state mid-stream, then collapse.
        interval.tick().await;
        interval.tick().await;
        {
            let mut guard = lock(&state);
            guard.input_rows = MIN_INPUT_ROWS;
            guard.loader = false;
        }
        requester.request_frame();
    });
}

/// Queue an oversized block — ~`OVERSIZED_HEIGHT_MULTIPLIER`× the terminal height
/// — for a single commit, so a validator can confirm a block far taller than the
/// viewport lands complete and ordered in scrollback (VAL-CORE-033).
fn queue_oversized_block(requester: &FrameRequester, state: &Arc<Mutex<DemoState>>) {
    let commit_number = {
        let mut guard = lock(state);
        guard.commit_count += 1;
        guard.commit_count
    };

    // Size it off the real terminal height so it is genuinely oversized; fall
    // back to a generous default if the height cannot be read.
    let rows = crossterm::terminal::size()
        .map(|(_, h)| usize::from(h))
        .unwrap_or(24)
        .max(1)
        * OVERSIZED_HEIGHT_MULTIPLIER;

    let mut block = vec![commit_marker(commit_number)];
    for i in 0..rows {
        block.push(Line::from(format!("oversized {commit_number}:{i:03}")));
    }
    lock(state).pending_commits.push(block);
    requester.request_frame();
}

/// Lock the shared state, treating poisoning as fatal — a poisoned lock means a
/// panic already tore through the demo, and continuing would paint garbage.
fn lock(state: &Arc<Mutex<DemoState>>) -> std::sync::MutexGuard<'_, DemoState> {
    state.lock().expect("demo state mutex poisoned")
}

/// Whether the flood should auto-start from the environment.
fn flood_requested_by_env() -> bool {
    std::env::var(FLOOD_ENV).map(|v| v == "1").unwrap_or(false)
}

/// Whether the continuous streaming loop should auto-start from the environment.
fn continuous_stream_requested_by_env() -> bool {
    std::env::var(STREAM_ENV).map(|v| v == "1").unwrap_or(false)
}

/// Extract a verbatim printable character from a key event, if it is a plain
/// character press with no ctrl/alt/super modifier. Returns `None` for chords
/// and named keys, so those never leak into the echoed text.
fn printable_char(key: &hand_tui::rt::events::RtKey) -> Option<char> {
    let mods = key.raw.modifiers;
    let chord = KeyModifiers::CONTROL
        | KeyModifiers::ALT
        | KeyModifiers::SUPER
        | KeyModifiers::HYPER
        | KeyModifiers::META;
    if mods.intersects(chord) {
        return None;
    }
    match key.raw.code {
        KeyCode::Char(c) if c != ' ' => Some(c),
        _ => None,
    }
}

/// Spinner glyph frames cycled while the loader shows.
const SPINNER_FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

fn draw(frame: &mut ratatui::Frame, state: &StateSnapshot) {
    let area = frame.area();

    // The frame area *is* the fixed inline viewport, already autoresized to the
    // current terminal width on this draw. Lay the (possibly grown/collapsed)
    // bottom area out inside it: a grow enlarges `active` but never the viewport,
    // and a collapse shrinks it and bottom-anchors, leaving the freed rows above
    // to repaint blank — no ghost. Width comes from the live viewport (so the
    // border spans the resized pane exactly); the *height* clamp uses the tracked
    // full-terminal rows, so shrinking the pane below the bottom area's wanted
    // height trims the active area to fit rather than overflowing.
    let geometry =
        bottom_area_geometry(state.input_rows, state.loader, area.width, state.size.rows);

    let flood = if state.flooding {
        format!(" · FLOOD #{}", state.flood_ticks)
    } else {
        String::new()
    };
    let title = Line::from(format!(
        " rt_demo — Ctrl+D/Ctrl+C quit · g=grow · c=collapse · l=loader · s=stream{flood} "
    ))
    .style(Style::default());
    let block = Block::bordered()
        .title(title)
        .title_alignment(Alignment::Left);

    // The bordered box occupies only the active area; the rows above it (freed by
    // a collapse) stay blank and repaint clear each frame.
    frame.render_widget(block, geometry.active);

    // The loader/spinner row, when showing, sits just below the top border.
    if let Some(loader_rect) = geometry.loader {
        let glyph = SPINNER_FRAMES[(state.spinner_phase as usize) % SPINNER_FRAMES.len()];
        let spinner = Line::from(vec![
            format!(" {glyph} ").fg(Color::Yellow),
            "working…".dim(),
        ]);
        frame.render_widget(Paragraph::new(spinner), inset(loader_rect));
    }

    // While a block is streaming, show its progress live in the input body — this
    // is the content a probe captures *before* the block commits into scrollback.
    // Otherwise echo the (possibly multi-row) input.
    let body = if state.streaming {
        let latest = state
            .stream_block
            .last()
            .map(|line| line.to_string())
            .unwrap_or_default();
        Line::from(vec![
            format!(
                "streaming {}/{} ",
                state.stream_block.len(),
                STREAM_BLOCK_LINES + 1
            )
            .dim(),
            latest.into(),
        ])
    } else {
        Line::from(vec!["> ".dim(), state.input.clone().into()])
    };
    frame.render_widget(Paragraph::new(body), inset(geometry.input));
}

/// Inset a rect by one column on each side so its content sits inside the block's
/// left/right border rather than overwriting it. Height is left unchanged.
fn inset(rect: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect::new(
        rect.x + 1,
        rect.y,
        rect.width.saturating_sub(2),
        rect.height,
    )
}
