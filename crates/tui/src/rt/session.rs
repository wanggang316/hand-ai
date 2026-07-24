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

/// Height of the inline viewport, in rows.
///
/// Under the fixed-max-viewport strategy (ratatui#984 workaround B), the inline
/// viewport is reserved once at the *tallest* the bottom area can ever be —
/// [`crate::rt::view::MAX_VIEWPORT_ROWS`] — and the active content (loader +
/// auto-growing input, from 1 up to 8 rows) is laid out inside it. Fixing it at
/// the max means a runtime grow never has to enlarge the viewport (which ratatui
/// cannot do without rebuilding the `Terminal`) and a shrink never moves it: only
/// the interior layout changes and the freed rows repaint blank, so there is no
/// scrollback leak or ghost row. See [`crate::rt::view`] for the geometry core
/// and the rejected alternatives.
pub const INLINE_VIEWPORT_ROWS: u16 = crate::rt::view::MAX_VIEWPORT_ROWS;

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
    /// A session guard is already live in this process. Only one guard may own
    /// the terminal's raw/interactive state at a time — a second [`enter`]
    /// would double-install the panic hook and race the shared restore flag, so
    /// it is refused *before* any terminal state is touched.
    ///
    /// [`enter`]: SessionGuard::enter
    AlreadyActive,
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
            SessionError::AlreadyActive => write!(
                f,
                "a terminal session is already active in this process; only one may run at a time"
            ),
            SessionError::Io(err) => write!(f, "terminal I/O error: {err}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::NotATty | SessionError::AlreadyActive => None,
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

/// Erase the inline viewport's rows, leaving the transcript above it untouched.
///
/// This is the single "wipe the fixed bottom-UI band clean" primitive shared by
/// two teardown/reflow paths that both otherwise leak the viewport's current
/// content:
///
/// - **Exit.** The rt runtime reserves a fixed inline viewport
///   ([`INLINE_VIEWPORT_ROWS`]) for the bottom UI. Neither ratatui's
///   `Terminal::Drop` (which only shows the cursor) nor [`SessionGuard::restore`]
///   (paste-disable + kitty-pop + cursor-show) erases those rows, so on quit the
///   bordered box is left on screen as a ghost and the shell prompt overwrites
///   *inside* it. Calling this before restoring wipes the band so the prompt
///   lands on a fresh line directly below the transcript, with no ghost border.
/// - **Resize.** ratatui recomputes the inline viewport lazily, on the next
///   `draw`/`autoresize` after a backend size change. That recompute
///   (`compute_inline_size` → `append_lines`) scrolls the viewport's *current*
///   cells — an old-width border box, or overlay rows — into native scrollback
///   *before* it re-anchors and clears. Wiping the viewport to blank *first*
///   means only blank rows can spill through *this* recompute path: on a
///   non-reflowing terminal no stale old-width fragment reaches scrollback. This
///   fixes only ratatui's own `compute_inline_size` → `append_lines` reflow; it
///   cannot stop a terminal that reflows its scrollback on resize (e.g. tmux)
///   from re-wrapping content that already landed in history — that reflow is the
///   terminal's own behaviour, outside rt's control.
///
/// Both uses map to the same terminal operation: for an inline viewport,
/// [`Terminal::clear`] moves the backend cursor to the viewport's top row and
/// issues an erase-in-display from there to the end of the screen, leaving every
/// row *above* the viewport (the committed transcript) intact and resetting the
/// back buffer so the next draw repaints in full. It preserves the cursor's
/// pre-clear column/row on backends that report it.
///
/// # Errors
///
/// Propagates any backend error from the clear (e.g. a failed write to the
/// underlying terminal). On a teardown path a caller typically ignores it —
/// there is nothing useful to do while tearing down — but the result is surfaced
/// so a live caller (the resize path) can react.
pub fn clear_viewport_region<B>(terminal: &mut Terminal<B>) -> Result<(), B::Error>
where
    B: Backend,
{
    terminal.clear()
}

/// A [`Terminal`] wrapper that erases the inline viewport region exactly once,
/// when it is dropped.
///
/// This is how the exit erase is made *deterministic without a scheduler-shutdown
/// hook*. The frame scheduler owns the terminal inside a spawned task, so the
/// terminal is dropped when that task ends (on a clean quit, all requesters
/// dropped; or on a panic unwinding through the task). Wrapping it here means the
/// [`clear_viewport_region`] wipe fires on that drop — while stdout is still
/// valid and the terminal still knows its viewport origin — so the ghost
/// bottom-UI box is gone *before* [`SessionGuard::restore`] runs and the shell
/// prompt lands on a fresh line below the transcript. There is no reliance on the
/// scheduler drawing one more frame at shutdown.
///
/// It [`Deref`](std::ops::Deref)s to the inner terminal, so a holder draws,
/// commits, and resizes through it exactly as through a bare [`Terminal`]. The
/// erase is best-effort (a teardown path must not panic): any backend error from
/// the wipe is swallowed.
///
/// M3's hand reuses the same scheduler-owns-the-terminal shape, so wrapping the
/// terminal here (rather than hacking the erase into the demo's shutdown) is what
/// makes the exit erase carry over unchanged.
#[derive(Debug)]
pub struct EraseOnDrop<B: Backend> {
    terminal: Terminal<B>,
}

impl<B: Backend> EraseOnDrop<B> {
    /// Wrap `terminal` so its inline viewport region is erased on drop.
    pub const fn new(terminal: Terminal<B>) -> Self {
        Self { terminal }
    }

    /// Consume the wrapper and return the inner terminal *without* erasing.
    ///
    /// The escape hatch for a caller that wants to keep the terminal alive past
    /// the wrapper (and take over the erase timing itself).
    pub fn into_inner(self) -> Terminal<B> {
        // Move the terminal out without running the erase Drop.
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is a `ManuallyDrop`, so its `Drop` never runs and the
        // field is not dropped twice; we move the terminal out exactly once.
        unsafe { std::ptr::read(&this.terminal) }
    }
}

impl<B: Backend> std::ops::Deref for EraseOnDrop<B> {
    type Target = Terminal<B>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl<B: Backend> std::ops::DerefMut for EraseOnDrop<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl<B: Backend> Drop for EraseOnDrop<B> {
    fn drop(&mut self) {
        // Best-effort: a teardown path must not panic. If the wipe fails there is
        // nothing useful to do while tearing down.
        let _ = clear_viewport_region(&mut self.terminal);
    }
}

/// Tracks whether a session guard is currently active, so the panic hook only
/// restores the terminal when a session actually owns it.
///
/// It is also the **single-session arbiter**: [`SessionGuard::enter`] claims it
/// with a `compare_exchange` (false → true) *before* touching any terminal
/// state, so a second concurrent (or re-entrant) `enter` observes the flag as
/// already set and is refused with [`SessionError::AlreadyActive`] rather than
/// double-installing the panic hook or racing the shared restore.
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Installs the terminal-restoring panic hook exactly once for the lifetime of
/// the process.
///
/// A fresh hook is chained onto the previous one only on the first live
/// session; subsequent sessions reuse it. The hook is keyed on the shared
/// [`SESSION_ACTIVE`] flag (via [`restore_once`]), so once a session ends and
/// clears the flag the hook is inert until the next `enter` re-arms it — there
/// is no need to (and no safe way to) *uninstall* a hook, and re-installing per
/// session would stack an unbounded chain of hooks across a resumed/re-entered
/// run. Kitty state is read from the flag-owning guard at restore time via the
/// captured `kitty` value of whichever session installed the hook; every live
/// session pushes the same kitty flags, so a single captured value is correct.
static PANIC_HOOK_INSTALLED: std::sync::Once = std::sync::Once::new();

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

        // Single-session guard: claim the shared flag *before* toggling raw mode
        // or installing the panic hook. If another guard already owns it the
        // claim fails and we return without having touched any terminal state,
        // so a re-entrant `enter` can never double-install the hook or race the
        // restore.
        if !claim_session(&SESSION_ACTIVE) {
            return Err(SessionError::AlreadyActive);
        }

        let kitty = should_use_kitty();

        if let Err(err) = enable_raw_mode() {
            // Release the claim we just took so a later `enter` can retry.
            SESSION_ACTIVE.store(false, Ordering::SeqCst);
            return Err(SessionError::Io(err));
        }

        let mut stdout = io::stdout();
        if let Err(err) = write_enter_sequences(&mut stdout, kitty) {
            // Roll back the raw-mode toggle *and* the claim so a partial failure
            // does not leave the shell wedged or the flag stuck set.
            let _ = disable_raw_mode();
            SESSION_ACTIVE.store(false, Ordering::SeqCst);
            return Err(SessionError::Io(err));
        }

        install_panic_hook(kitty);

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

    /// Temporarily yield the terminal to a foreground child process (an external
    /// `$VISUAL` / `$EDITOR` opened with Ctrl+G), then re-claim it with
    /// [`resume`](SessionGuard::resume).
    ///
    /// Unlike [`restore`](SessionGuard::restore) this does **not** touch the
    /// single-session `SESSION_ACTIVE` flag or the panic hook: the guard stays
    /// live and its one-shot teardown contract (explicit restore / `Drop` / panic
    /// hook, emitted exactly once) is unchanged. It only pops the interactive
    /// escapes (paste-disable, kitty-pop, show-cursor) and leaves raw mode so the
    /// spawned editor gets a clean cooked TTY. Best-effort — any escape-write or
    /// raw-toggle error is swallowed, matching the teardown discipline (a failed
    /// suspend must not panic the run loop).
    ///
    /// Pairs strictly with [`resume`](SessionGuard::resume): the caller suspends,
    /// runs the child to completion, then resumes. This is the M3 Ctrl+G
    /// terminal-handoff seam (VAL-EDITOR-020) — the smallest surface that lets the
    /// interactive driver hand the terminal to an external editor and take it back
    /// without tearing down the session.
    pub fn suspend(&self) {
        restore_terminal(self.kitty);
    }

    /// Re-claim the terminal after a [`suspend`](SessionGuard::suspend): re-enter
    /// raw mode and re-write the interactive enter escapes (bracketed paste, and —
    /// when this session pushed them — the kitty keyboard flags).
    ///
    /// Best-effort like [`suspend`](SessionGuard::suspend): errors are swallowed so
    /// a partial resume never panics the run loop. The caller requests a full
    /// repaint afterwards (the scheduler's per-frame viewport wipe redraws the
    /// bottom UI cleanly), so no residue from the editor's screen survives.
    pub fn resume(&self) {
        let _ = enable_raw_mode();
        let mut stdout = io::stdout();
        let _ = write_enter_sequences(&mut stdout, self.kitty);
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Claim exclusive ownership of `active`, returning whether the claim succeeded.
///
/// The single-session arbiter: a `compare_exchange` from `false` to `true`
/// succeeds for exactly one caller while the flag is clear, and fails for every
/// caller that observes it already set. Kept as a free function over the flag so
/// the exclusion can be unit-tested against a local `AtomicBool` without
/// touching the real terminal — the mirror of [`restore_once`], which releases
/// the same flag.
fn claim_session(active: &AtomicBool) -> bool {
    active
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
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
///
/// Installed **exactly once** per process via [`PANIC_HOOK_INSTALLED`]: chaining
/// a fresh hook on every `enter` would stack an unbounded chain across a
/// resumed / re-entered session. The hook keys on the shared [`SESSION_ACTIVE`]
/// flag, so it is inert between sessions and re-armed by the next `enter`.
fn install_panic_hook(kitty: bool) {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_once(&SESSION_ACTIVE, || restore_terminal(kitty));
            previous(info);
        }));
    });
}

/// A SIGHUP listener for the runtime's main loop.
///
/// The only way a TTY's stdin can *close* under us is the controlling PTY
/// master going away, which the kernel signals with **SIGHUP**. Under the
/// default disposition SIGHUP terminates the process outright (exit-by-signal
/// 1), so neither [`SessionGuard::Drop`] nor [`SessionGuard::restore`] ever
/// runs and the terminal is left in raw mode — the stdin-close gap.
///
/// Rather than install a bare signal handler and touch the terminal from inside
/// it (escape writes and `disable_raw_mode` are **not** async-signal-safe), the
/// runtime registers this listener and `select!`s on it in its normal event
/// loop. Delivery of a SIGHUP wakes [`Hangup::recv`]; the loop then takes the
/// *same* clean-exit path a Ctrl+D takes, so [`SessionGuard`]'s ordinary
/// teardown restores cooked mode, shows the cursor, pops kitty flags, and
/// disables paste — all from safe, ordinary control flow.
///
/// Registering a listener also installs tokio's process-wide handler, which
/// supersedes the default terminate-on-SIGHUP disposition: the signal no longer
/// kills the process before the loop can react.
///
/// This is also the probe seam for the stdin-close assertion: a probe closes
/// the PTY master (or sends `kill -HUP <pid>`) and observes the terminal come
/// back cooked with the kitty-pop / `?2004l` restore tail, then a clean exit.
///
/// # Errors
///
/// Propagates the [`io::Error`] from tokio's signal registration (e.g. the
/// signal handler slot is already taken by foreign code). Must be called from
/// within a tokio runtime.
#[cfg(unix)]
pub fn hangup_listener() -> io::Result<Hangup> {
    use tokio::signal::unix::{SignalKind, signal};
    Ok(Hangup {
        signal: signal(SignalKind::hangup())?,
    })
}

/// A registered SIGHUP listener; see [`hangup_listener`].
///
/// Holding one keeps tokio's SIGHUP handler installed, so the default
/// terminate-on-hangup disposition stays superseded for the lifetime of the
/// listener. Awaiting [`recv`](Hangup::recv) resolves when a SIGHUP is
/// delivered.
#[cfg(unix)]
#[derive(Debug)]
pub struct Hangup {
    signal: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Hangup {
    /// Wait for the next SIGHUP.
    ///
    /// Resolves to `Some(())` when a hangup is delivered, or `None` if the
    /// signal stream is closed (which does not happen for a live listener).
    /// Cancellation-safe: dropping the returned future without completing it
    /// loses no already-delivered signal.
    pub async fn recv(&mut self) -> Option<()> {
        self.signal.recv().await
    }
}

#[cfg(test)]
mod claim_session_tests {
    use super::{claim_session, restore_once};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The first claim on a clear flag succeeds and sets it; a second claim,
    /// while the flag is still held, is refused. This is the single-session
    /// exclusion: only one guard may own the terminal at a time.
    #[test]
    fn claim_session_admits_one_and_refuses_the_second() {
        let active = AtomicBool::new(false);

        assert!(claim_session(&active), "first claim must succeed");
        assert!(active.load(Ordering::SeqCst), "flag must be set");
        assert!(
            !claim_session(&active),
            "a second claim while active must be refused"
        );
    }

    /// After the flag is released (the guard's restore path clears it via
    /// `restore_once`), a fresh claim succeeds again — a new session may enter
    /// once the previous one has torn down.
    #[test]
    fn claim_session_succeeds_again_after_release() {
        let active = AtomicBool::new(false);

        assert!(claim_session(&active));
        // Release, as the guard's restore / Drop does.
        restore_once(&active, || {});
        assert!(!active.load(Ordering::SeqCst), "flag must be cleared");

        assert!(
            claim_session(&active),
            "a new session may claim once the previous released"
        );
    }
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
