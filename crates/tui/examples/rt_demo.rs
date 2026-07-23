//! Minimal inline terminal-session demo for the ratatui runtime.
//!
//! Launches an inline viewport (never the alternate screen) so the shell
//! content above stays visible, then draws a bordered input line at the bottom
//! that echoes what you type. It exercises the full session lifecycle:
//! raw mode, bracketed paste, optional kitty keyboard flags, and terminal
//! restoration on every exit path (quit, Ctrl+C, EOF, panic).
//!
//! Keys:
//!   - printable chars / Backspace: edit the input line
//!   - Ctrl+D or Ctrl+C: quit cleanly (terminal fully restored)
//!   - Ctrl+Z: ignored (no suspend; the UI does not move)
//!   - F12: DELIBERATE PANIC — crashes on purpose so you can confirm the panic
//!     path still leaves a readable, usable terminal (VAL-CORE-018)
//!
//! Run it:
//!   cargo run -p hand-tui --example rt_demo
//!   HAND_TUI_FORCE_KITTY_KEYBOARD=1 cargo run -p hand-tui --example rt_demo
//!
//! On a non-TTY (piped) stdin/stdout it prints a diagnostic to stderr and
//! exits non-zero without ever touching the parent shell's terminal mode.

use std::io;
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use hand_tui::rt::session::{SessionError, SessionGuard};
use ratatui::layout::Alignment;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

/// The key that intentionally panics, for exercising the panic-restore path.
const PANIC_KEY_HELP: &str = "F12 = deliberate panic";

fn main() -> ExitCode {
    if wants_help() {
        print_help();
        return ExitCode::SUCCESS;
    }

    match run() {
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
        "rt_demo — inline terminal session demo\n\
         \n\
         Inline viewport (no alternate screen); prior shell content stays visible.\n\
         \n\
         Keys:\n\
         \x20 printable / Backspace : edit the input line\n\
         \x20 Ctrl+D or Ctrl+C      : quit cleanly\n\
         \x20 Ctrl+Z                : ignored (no suspend)\n\
         \x20 {PANIC_KEY_HELP} (crashes on purpose; terminal stays readable)"
    );
}

fn run() -> Result<(), SessionError> {
    // Establishing the guard verifies stdin/stdout are TTYs *before* toggling
    // raw mode, so a non-interactive launch leaves the shell untouched.
    let mut guard = SessionGuard::enter()?;
    let mut terminal = guard.terminal()?;

    let mut input = String::new();

    loop {
        terminal
            .draw(|frame| draw(frame, &input))
            .map_err(SessionError::Io)?;

        // Poll so a paint can happen even if input is idle; a plain blocking
        // read would also work for this demo.
        if !event::poll(Duration::from_millis(200)).map_err(SessionError::Io)? {
            continue;
        }

        match event::read() {
            Ok(Event::Key(key)) => {
                // Kitty mode reports Press/Release/Repeat; only act on Press so
                // keys do not double-fire.
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    // Ctrl+D / Ctrl+C: clean quit (guard restores on Drop).
                    KeyCode::Char('d' | 'c') if ctrl => break,
                    // Deliberate panic to exercise the panic-restore path.
                    KeyCode::F(12) => panic!("rt_demo: deliberate panic (F12) for VAL-CORE-018"),
                    // Ctrl+Z is intentionally a no-op: no SIGTSTP, UI unchanged.
                    KeyCode::Char('z') if ctrl => {}
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) if !ctrl => input.push(c),
                    _ => {}
                }
            }
            // EOF on the input stream: quit cleanly.
            Ok(Event::Resize(_, _)) => {}
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(SessionError::Io(err)),
        }
    }

    // Explicit restore before returning; Drop would also do it, but doing it
    // here keeps the teardown ordering obvious.
    guard.restore();
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, input: &str) {
    let title = Line::from(format!(" rt_demo — Ctrl+D/Ctrl+C quit · {PANIC_KEY_HELP} "))
        .style(Style::default());
    let block = Block::bordered()
        .title(title)
        .title_alignment(Alignment::Left);
    let body = Line::from(vec!["> ".dim(), input.into()]);
    let paragraph = Paragraph::new(body).block(block);
    frame.render_widget(paragraph, frame.area());
}
