//! Terminal session lifecycle for the ratatui runtime.
//!
//! Owns the crossterm terminal state around the ratatui `Terminal`: entering
//! and leaving raw mode, configuring the inline viewport, enabling bracketed
//! paste, pushing/popping kitty keyboard-enhancement flags, and restoring the
//! terminal on both the normal exit and panic paths.
//!
//! The session is deliberately inline: it never enters the alternate screen,
//! so prior shell output stays visible above the viewport. It also never
//! enables mouse capture.

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::KeyboardEnhancementFlags;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement};
use crossterm::tty::IsTty;
use crossterm::{Command, queue};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Fallback terminal width used when the real width reports as zero or cannot
/// be queried (e.g. a headless 0x0 PTY).
pub const FALLBACK_COLS: u16 = 80;

/// Fallback terminal height used when the real height reports as zero or cannot
/// be queried.
pub const FALLBACK_ROWS: u16 = 24;

/// Height of the inline viewport, in rows (bordered input line: 3 rows).
pub const INLINE_VIEWPORT_ROWS: u16 = 3;

/// Environment variable that forces the kitty keyboard-enhancement push even
/// when `supports_keyboard_enhancement()` cannot confirm support (e.g. a dumb
/// PTY that never answers the query). Set to `1` to force it on.
pub const FORCE_KITTY_ENV: &str = "HAND_TUI_FORCE_KITTY_KEYBOARD";

/// Kitty keyboard flags we push: disambiguate escape codes so plain keys and
/// escape sequences are distinguishable, and report event types so we can
/// filter key-release/repeat and avoid double-firing.
fn kitty_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
}

/// Resolve the terminal geometry, substituting the 80x24 fallback whenever
/// either dimension is zero. A zero dimension means the terminal geometry is
/// unknown (headless PTY), in which case a sane default keeps rendering alive.
#[must_use]
pub fn effective_size(cols: u16, rows: u16) -> (u16, u16) {
    if cols == 0 || rows == 0 {
        (FALLBACK_COLS, FALLBACK_ROWS)
    } else {
        (cols, rows)
    }
}

/// A [`Backend`] wrapper that keeps rendering alive on degenerate terminals.
///
/// It substitutes the 80x24 fallback geometry whenever the wrapped backend
/// reports a zero-sized or unqueryable size/window-size, and treats a failed
/// cursor-position query as the origin. Every other operation is forwarded
/// unchanged.
#[derive(Debug)]
pub struct FallbackSizeBackend<B> {
    inner: B,
}

impl<B> FallbackSizeBackend<B> {
    /// Wrap a backend so its size and cursor queries degrade gracefully.
    pub const fn new(inner: B) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and return the inner backend.
    pub fn into_inner(self) -> B {
        self.inner
    }

    /// Borrow the inner backend.
    pub const fn inner(&self) -> &B {
        &self.inner
    }

    /// Mutably borrow the inner backend.
    pub const fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }
}

impl<B> Backend for FallbackSizeBackend<B>
where
    B: Backend<Error = io::Error>,
{
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // A cursor-position query can go unanswered on a headless PTY; treat
        // that as the origin rather than propagating the failure.
        Ok(self.inner.get_cursor_position().unwrap_or(Position::ORIGIN))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn size(&self) -> io::Result<Size> {
        let (cols, rows) = self
            .inner
            .size()
            .map(|size| (size.width, size.height))
            .unwrap_or((0, 0));
        let (cols, rows) = effective_size(cols, rows);
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let mut window = self.inner.window_size().unwrap_or(WindowSize {
            columns_rows: Size::new(0, 0),
            pixels: Size::new(0, 0),
        });
        let (cols, rows) = effective_size(window.columns_rows.width, window.columns_rows.height);
        window.columns_rows = Size::new(cols, rows);
        Ok(window)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn scroll_region_up(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_down(region, line_count)
    }
}

/// Errors raised while establishing a terminal session.
#[derive(Debug)]
pub enum SessionError {
    /// The provided handle is not connected to a terminal (TTY). Launching a
    /// full-screen session against a pipe or file would corrupt the parent
    /// shell, so we refuse.
    NotATty,
    /// An underlying I/O operation failed (raw mode toggle, escape write, ...).
    Io(io::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotATty => write!(
                f,
                "standard input/output is not a TTY (terminal); run this from an interactive terminal"
            ),
            SessionError::Io(err) => write!(f, "terminal I/O error: {err}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::NotATty => None,
            SessionError::Io(err) => Some(err),
        }
    }
}

impl From<io::Error> for SessionError {
    fn from(err: io::Error) -> Self {
        SessionError::Io(err)
    }
}

/// Verify that the given handle refers to a terminal.
///
/// Returns [`SessionError::NotATty`] for pipes, regular files, and any other
/// non-terminal file descriptor. A PTY slave counts as a terminal.
pub fn check_tty(handle: &impl AsFd) -> Result<(), SessionError> {
    if handle.as_fd().is_tty() {
        Ok(())
    } else {
        Err(SessionError::NotATty)
    }
}

/// Queue a crossterm command as raw ANSI bytes into `out`.
fn queue_cmd(out: &mut impl Write, command: impl Command) -> io::Result<()> {
    queue!(out, command)
}

/// Write the escape sequences that put the terminal into interactive input
/// mode: always enable bracketed paste, and — when `kitty` is true — push the
/// kitty keyboard-enhancement flags. Never enables mouse capture.
pub fn write_enter_sequences(out: &mut impl Write, kitty: bool) -> io::Result<()> {
    queue_cmd(out, crossterm::event::EnableBracketedPaste)?;
    if kitty {
        queue_cmd(
            out,
            crossterm::event::PushKeyboardEnhancementFlags(kitty_flags()),
        )?;
    }
    out.flush()
}

/// Write the escape sequences that restore the terminal to interactive-shell
/// state: disable bracketed paste, show the cursor, and — when `kitty` is true
/// — pop the kitty keyboard-enhancement flags. Never touches mouse capture.
pub fn write_restore_sequences(out: &mut impl Write, kitty: bool) -> io::Result<()> {
    if kitty {
        queue_cmd(out, crossterm::event::PopKeyboardEnhancementFlags)?;
    }
    queue_cmd(out, crossterm::event::DisableBracketedPaste)?;
    queue_cmd(out, crossterm::cursor::Show)?;
    out.flush()
}

/// Whether the kitty keyboard protocol should be pushed for this session.
///
/// Honours the [`FORCE_KITTY_ENV`] override first (used against dumb PTYs that
/// never answer the enhancement query), then falls back to probing the
/// terminal via crossterm.
#[must_use]
pub fn should_use_kitty() -> bool {
    if force_kitty_requested() {
        return true;
    }
    supports_keyboard_enhancement().unwrap_or(false)
}

fn force_kitty_requested() -> bool {
    std::env::var(FORCE_KITTY_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// The stdout-backed ratatui terminal used by a live session.
pub type SessionTerminal = Terminal<FallbackSizeBackend<CrosstermBackend<io::Stdout>>>;

/// Tracks whether a session guard is currently active, so the panic hook only
/// restores the terminal when a session actually owns it.
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// RAII guard that owns the terminal's raw/interactive state.
///
/// Constructed by [`SessionGuard::enter`], it puts the terminal into raw mode,
/// enables bracketed paste, and (when supported) pushes kitty keyboard flags.
/// On [`Drop`] — whether from a normal exit, an early return, or a panic
/// unwinding through it — it restores cooked mode, shows the cursor, disables
/// bracketed paste, and pops kitty flags.
///
/// A panic hook installed at construction time restores the terminal even when
/// the panic aborts before the guard's `Drop` runs, then chains to the previous
/// hook so the panic message still prints readably.
pub struct SessionGuard {
    kitty: bool,
    restored: bool,
}

impl SessionGuard {
    /// Establish a session on the current stdin/stdout.
    ///
    /// Fails with [`SessionError::NotATty`] if stdout or stdin is not a
    /// terminal, without ever having toggled raw mode — so a non-interactive
    /// invocation leaves the parent shell untouched.
    pub fn enter() -> Result<Self, SessionError> {
        // Refuse before touching terminal state: never leave the parent shell
        // in raw mode when we were not launched interactively.
        check_tty(&io::stdout())?;
        check_tty(&io::stdin())?;

        let kitty = should_use_kitty();

        enable_raw_mode().map_err(SessionError::Io)?;

        let mut stdout = io::stdout();
        if let Err(err) = write_enter_sequences(&mut stdout, kitty) {
            // Roll back the raw-mode toggle so a partial failure does not leave
            // the shell wedged.
            let _ = disable_raw_mode();
            return Err(SessionError::Io(err));
        }

        install_panic_hook(kitty);
        SESSION_ACTIVE.store(true, Ordering::SeqCst);

        Ok(Self {
            kitty,
            restored: false,
        })
    }

    /// Whether this session pushed kitty keyboard-enhancement flags.
    #[must_use]
    pub const fn kitty_enabled(&self) -> bool {
        self.kitty
    }

    /// Build the inline ratatui terminal for this session.
    ///
    /// Uses an inline viewport (never the alternate screen) wrapped in
    /// [`FallbackSizeBackend`] so a 0x0 PTY still renders at 80x24.
    pub fn terminal(&self) -> Result<SessionTerminal, SessionError> {
        let backend = FallbackSizeBackend::new(CrosstermBackend::new(io::stdout()));
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(INLINE_VIEWPORT_ROWS),
            },
        )
        .map_err(SessionError::Io)
    }

    /// Restore the terminal to cooked, interactive-shell state.
    ///
    /// Idempotent across every teardown path: an explicit call, [`Drop`], and
    /// the panic hook all compete for the same one-shot `SESSION_ACTIVE` flag,
    /// so the restore sequences (paste-disable, kitty-pop, show-cursor) are
    /// emitted **exactly once** even when the panic hook runs and then Drop
    /// unwinds through this guard. `restored` is a cheap local short-circuit;
    /// the atomic swap is the shared arbiter that also excludes the panic hook.
    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let kitty = self.kitty;
        restore_once(&SESSION_ACTIVE, || restore_terminal(kitty));
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// One-shot restore arbiter shared by the guard's `restore`/`Drop` and the
/// panic hook.
///
/// Swaps `active` to `false` and runs `emit` only for the single caller that
/// observes the flag as still `true`. This is the whole idempotence guarantee:
/// whichever of {explicit restore, Drop, panic hook} runs first emits the
/// restore sequences; every subsequent caller is a no-op. Kept as a free
/// function taking the flag so it can be unit-tested against a local
/// `AtomicBool` and a counting closure without touching the real terminal.
fn restore_once(active: &AtomicBool, emit: impl FnOnce()) {
    if active.swap(false, Ordering::SeqCst) {
        emit();
    }
}

/// Restore terminal state: pop kitty flags, disable paste, show cursor, leave
/// raw mode. Best-effort — errors are swallowed because there is nothing useful
/// to do while tearing down, and we must not panic from a `Drop`/panic path.
fn restore_terminal(kitty: bool) {
    let mut stdout = io::stdout();
    let _ = write_restore_sequences(&mut stdout, kitty);
    let _ = disable_raw_mode();
}

/// Install a panic hook that restores the terminal before the previous hook
/// prints the panic message, so a crash leaves a readable, usable terminal.
fn install_panic_hook(kitty: bool) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_once(&SESSION_ACTIVE, || restore_terminal(kitty));
        previous(info);
    }));
}

#[cfg(test)]
mod restore_once_tests {
    use super::restore_once;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// The panic-hook-then-Drop sequence: the shared flag is armed once, and
    /// the two competing restore paths emit the restore sequences exactly once
    /// between them — never twice, never zero.
    #[test]
    fn restore_once_emits_exactly_once_across_panic_and_drop() {
        let active = AtomicBool::new(true);
        let count = AtomicUsize::new(0);

        // Panic hook fires first.
        restore_once(&active, || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        // Then Drop unwinds through the guard and calls restore again.
        restore_once(&active, || {
            count.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "restore sequences must be emitted exactly once across panic + Drop"
        );
        assert!(!active.load(Ordering::SeqCst), "flag must be disarmed");
    }

    /// A restore when no session is active (flag already `false`) emits nothing.
    #[test]
    fn restore_once_noop_when_flag_already_disarmed() {
        let active = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        restore_once(&active, || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
