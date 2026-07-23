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
//!   - Ctrl+D or Ctrl+C: quit cleanly (terminal fully restored)
//!   - Ctrl+Z: ignored (no suspend; the UI does not move)
//!   - F12: DELIBERATE PANIC — crashes on purpose so you can confirm the panic
//!     path still leaves a readable, usable terminal (VAL-CORE-018)
//!
//! Paste (bracketed paste): a multi-line paste lands as a single event and is
//! inserted whole — it never fires per-character key actions.
//!
//! Run it:
//!   cargo run -p hand-tui --example rt_demo
//!   HAND_TUI_RT_DEMO_FLOOD=1 cargo run -p hand-tui --example rt_demo
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
    /// The current input-line contents.
    input: String,
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
         \x20 Ctrl+D or Ctrl+C      : quit cleanly\n\
         \x20 Ctrl+Z                : ignored (no suspend)\n\
         \x20 paste                 : multi-line paste inserted as one event\n\
         \x20 {PANIC_KEY_HELP} (crashes on purpose; terminal stays readable)\n\
         \n\
         Env:\n\
         \x20 {FLOOD_ENV}=1 : start the flood automatically at launch"
    );
}

async fn run() -> Result<(), SessionError> {
    // Establishing the guard verifies stdin/stdout are TTYs *before* toggling
    // raw mode, so a non-interactive launch leaves the shell untouched.
    let mut guard = SessionGuard::enter()?;
    let terminal = guard.terminal()?;

    let state = Arc::new(Mutex::new(DemoState::default()));

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
            // Resize / focus: just repaint.
            RtInputEvent::Resize { .. } | RtInputEvent::FocusGained | RtInputEvent::FocusLost => {}
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
            let snapshot = StateSnapshot {
                input: guard.input.clone(),
                flood_ticks: guard.flood_ticks,
                flooding: guard.flooding,
                stream_block: guard.stream_block.clone(),
                streaming: guard.streaming,
            };
            (snapshot, commits)
        };

        // Commit finished blocks into native scrollback *before* the draw. Each
        // block becomes exactly one `insert_before`; the sink pre-wraps to the
        // current width so a tall or wide block lands complete and ordered.
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
    input: String,
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

fn draw(frame: &mut ratatui::Frame, state: &StateSnapshot) {
    let flood = if state.flooding {
        format!(" · FLOOD #{}", state.flood_ticks)
    } else {
        String::new()
    };
    let title = Line::from(format!(
        " rt_demo — Ctrl+D/Ctrl+C quit · f=flood · s=stream · b=big · {PANIC_KEY_HELP}{flood} "
    ))
    .style(Style::default());
    let block = Block::bordered()
        .title(title)
        .title_alignment(Alignment::Left);

    // While a block is streaming, show its progress live in the viewport body —
    // this is the content a probe captures *before* the block commits into
    // scrollback. Otherwise show the input line.
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
    let paragraph = Paragraph::new(body).block(block);
    frame.render_widget(paragraph, frame.area());
}
