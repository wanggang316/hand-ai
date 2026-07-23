//! Minimal inline terminal-session demo for the ratatui runtime.
//!
//! Launches an inline viewport (never the alternate screen) so the shell
//! content above stays visible, then draws a bordered input line at the bottom
//! that echoes what you type. It exercises the full session lifecycle
//! (raw mode, bracketed paste, optional kitty keyboard flags, restoration on
//! every exit path) and the rt input pipeline: crossterm events are read off an
//! async `EventStream`, translated to `RtInputEvent`s, and delivered over an
//! mpsc channel.
//!
//! Keys:
//!   - printable chars / Backspace: edit the input line
//!   - Enter: clear the input line (submit placeholder)
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
//!   HAND_TUI_FORCE_KITTY_KEYBOARD=1 cargo run -p hand-tui --example rt_demo
//!
//! On a non-TTY (piped) stdin/stdout it prints a diagnostic to stderr and
//! exits non-zero without ever touching the parent shell's terminal mode.

use std::process::ExitCode;

use crossterm::event::{KeyCode, KeyModifiers};
use hand_tui::rt::events::{RtInputEvent, spawn_event_pump};
use hand_tui::rt::session::{SessionError, SessionGuard};
use ratatui::layout::Alignment;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

/// The key that intentionally panics, for exercising the panic-restore path.
const PANIC_KEY_HELP: &str = "F12 = deliberate panic";

/// Bound on the event channel: a small buffer is plenty for interactive typing;
/// backpressure just makes the pump await, which is fine.
const EVENT_CHANNEL_CAPACITY: usize = 64;

fn main() -> ExitCode {
    if wants_help() {
        print_help();
        return ExitCode::SUCCESS;
    }

    // The rt input pipeline drives crossterm's async EventStream, so the demo
    // needs a tokio runtime to poll it.
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
        "rt_demo — inline terminal session demo (rt input pipeline)\n\
         \n\
         Inline viewport (no alternate screen); prior shell content stays visible.\n\
         \n\
         Keys:\n\
         \x20 printable / Backspace : edit the input line\n\
         \x20 Enter                 : clear the input line\n\
         \x20 Ctrl+D or Ctrl+C      : quit cleanly\n\
         \x20 Ctrl+Z                : ignored (no suspend)\n\
         \x20 paste                 : multi-line paste inserted as one event\n\
         \x20 {PANIC_KEY_HELP} (crashes on purpose; terminal stays readable)"
    );
}

async fn run() -> Result<(), SessionError> {
    // Establishing the guard verifies stdin/stdout are TTYs *before* toggling
    // raw mode, so a non-interactive launch leaves the shell untouched.
    let mut guard = SessionGuard::enter()?;
    let mut terminal = guard.terminal()?;

    // Spawn the rt input pump: it reads crossterm's EventStream, translates each
    // event, and delivers RtInputEvents over the channel. Release/repeat are
    // already filtered, Esc and alt-chords are single events, and paste arrives
    // whole.
    let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

    let mut input = String::new();

    // Initial paint (frame scheduling is a later feature; redraw per event).
    terminal
        .draw(|frame| draw(frame, &input))
        .map_err(SessionError::Io)?;

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
                Some("backspace") => {
                    input.pop();
                }
                Some("enter") => input.clear(),
                Some("space") => input.push(' '),
                // A printable character with no ctrl/alt/super modifier. Read
                // from the raw event so case (and shifted symbols) echo
                // verbatim; the canonical id lowercases letters.
                _ => {
                    if let Some(ch) = printable_char(&key) {
                        input.push(ch);
                    }
                }
            },
            // Bracketed paste: insert the whole payload as one action. No
            // per-character key actions fire.
            RtInputEvent::Paste(payload) => input.push_str(&payload),
            // Resize / focus: just repaint (handled by the redraw below).
            RtInputEvent::Resize { .. }
            | RtInputEvent::FocusGained
            | RtInputEvent::FocusLost => {}
        }

        if quit {
            break;
        }

        terminal
            .draw(|frame| draw(frame, &input))
            .map_err(SessionError::Io)?;
    }

    // Stop the pump; the receiver drop makes any in-flight send fail and the
    // loop exit. Aborting is fine — it is a detached reader with no cleanup.
    pump.abort();

    // Explicit restore before returning; Drop would also do it, but doing it
    // here keeps the teardown ordering obvious.
    guard.restore();
    Ok(())
}

/// Extract a verbatim printable character from a key event, if it is a plain
/// character press with no ctrl/alt/super modifier (shift is allowed — it is
/// already baked into the char's case by crossterm). Returns `None` for chords
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

fn draw(frame: &mut ratatui::Frame, input: &str) {
    let title = Line::from(format!(
        " rt_demo — Ctrl+D/Ctrl+C quit · {PANIC_KEY_HELP} "
    ))
    .style(Style::default());
    let block = Block::bordered()
        .title(title)
        .title_alignment(Alignment::Left);
    let body = Line::from(vec!["> ".dim(), input.into()]);
    let paragraph = Paragraph::new(body).block(block);
    frame.render_widget(paragraph, frame.area());
}
