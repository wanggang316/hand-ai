//! Terminal abstraction layer.
//!
//! Provides a trait for terminal I/O operations and a real implementation
//! using crossterm.
//!
//! In addition to the [`Terminal`] trait, this module exposes raw-mode and
//! alternate-screen toggling on [`ProcessTerminal`] (with a Drop-safe
//! restore), and an async stdin reader [`run_stdin_reader`] that the
//! [`Tui`](crate::tui::Tui) run loop drives.

#[cfg(not(test))]
use std::io::Write;

use crossterm::terminal;
#[cfg(not(test))]
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

use crate::error::{TuiError, TuiResult};
use crate::stdin_buffer::{StdinBuffer, StdinBufferEvent};

/// Terminal capabilities detection.
#[derive(Debug, Clone)]
pub struct TerminalCapabilities {
    pub supports_color: bool,
    pub supports_unicode: bool,
    pub supports_images: bool,
    pub supports_mouse: bool,
    pub supports_kitty_protocol: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            supports_color: true,
            supports_unicode: true,
            supports_images: false,
            supports_mouse: true,
            supports_kitty_protocol: false,
        }
    }
}

/// Abstract terminal interface.
pub trait Terminal: Send {
    /// Write data to the terminal.
    fn write(&mut self, data: &str);

    /// Get terminal width in columns.
    fn columns(&self) -> u16;

    /// Get terminal height in rows.
    fn rows(&self) -> u16;

    /// Hide the cursor.
    fn hide_cursor(&mut self);

    /// Show the cursor.
    fn show_cursor(&mut self);

    /// Clear the current line.
    fn clear_line(&mut self);

    /// Clear from cursor to end of screen.
    fn clear_from_cursor(&mut self);

    /// Clear the entire screen.
    fn clear_screen(&mut self);

    /// Move cursor by the given number of lines (positive = down).
    fn move_by(&mut self, lines: i32);

    /// Set the terminal title.
    fn set_title(&mut self, title: &str);

    /// Get terminal capabilities.
    fn capabilities(&self) -> &TerminalCapabilities;
}

/// Real terminal implementation using crossterm.
pub struct ProcessTerminal {
    capabilities: TerminalCapabilities,
    columns: u16,
    rows: u16,
    raw_mode: bool,
    alternate_screen: bool,
}

impl ProcessTerminal {
    pub fn new() -> std::io::Result<Self> {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        Ok(Self {
            capabilities: TerminalCapabilities::default(),
            columns: cols,
            rows,
            raw_mode: false,
            alternate_screen: false,
        })
    }

    /// Refresh terminal size (call after resize).
    pub fn refresh_size(&mut self) {
        if let Ok((cols, rows)) = terminal::size() {
            self.columns = cols;
            self.rows = rows;
        }
    }

    /// Enable raw mode. Idempotent — safe to call when already raw.
    pub fn enter_raw_mode(&mut self) -> std::io::Result<()> {
        if self.raw_mode {
            return Ok(());
        }
        Self::enable_raw_mode_impl()?;
        self.raw_mode = true;
        Ok(())
    }

    /// Restore cooked mode. Idempotent.
    pub fn leave_raw_mode(&mut self) -> std::io::Result<()> {
        if !self.raw_mode {
            return Ok(());
        }
        Self::disable_raw_mode_impl()?;
        self.raw_mode = false;
        Ok(())
    }

    pub fn is_raw_mode(&self) -> bool {
        self.raw_mode
    }

    /// Switch to the alternate screen buffer. Idempotent.
    pub fn enter_alternate_screen(&mut self) -> std::io::Result<()> {
        if self.alternate_screen {
            return Ok(());
        }
        Self::enter_alt_screen_impl()?;
        self.alternate_screen = true;
        Ok(())
    }

    /// Switch back to the main screen buffer. Idempotent.
    pub fn leave_alternate_screen(&mut self) -> std::io::Result<()> {
        if !self.alternate_screen {
            return Ok(());
        }
        Self::leave_alt_screen_impl()?;
        self.alternate_screen = false;
        Ok(())
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    // --- crossterm shims with a test gate -------------------------------------
    //
    // Tests must NOT actually flip the test runner's tty into raw mode (it would
    // wedge the harness on Unix CI). We gate the real crossterm calls behind
    // `#[cfg(not(test))]`; in tests these become no-ops while the bookkeeping
    // (`self.raw_mode`) still flips so the public API can be exercised.

    #[cfg(not(test))]
    fn enable_raw_mode_impl() -> std::io::Result<()> {
        enable_raw_mode()
    }

    #[cfg(test)]
    fn enable_raw_mode_impl() -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(not(test))]
    fn disable_raw_mode_impl() -> std::io::Result<()> {
        disable_raw_mode()
    }

    #[cfg(test)]
    fn disable_raw_mode_impl() -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(not(test))]
    fn enter_alt_screen_impl() -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, EnterAlternateScreen)?;
        stdout.flush()
    }

    #[cfg(test)]
    fn enter_alt_screen_impl() -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(not(test))]
    fn leave_alt_screen_impl() -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, LeaveAlternateScreen)?;
        stdout.flush()
    }

    #[cfg(test)]
    fn leave_alt_screen_impl() -> std::io::Result<()> {
        Ok(())
    }
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new().unwrap_or(Self {
            capabilities: TerminalCapabilities::default(),
            columns: 80,
            rows: 24,
            raw_mode: false,
            alternate_screen: false,
        })
    }
}

impl Drop for ProcessTerminal {
    /// Best-effort restore on drop. Errors are swallowed (we're in `Drop`).
    /// Without this, a panic mid-run would leave the user's shell in raw mode
    /// or stuck on the alternate screen.
    fn drop(&mut self) {
        if self.alternate_screen {
            let _ = self.leave_alternate_screen();
        }
        if self.raw_mode {
            let _ = self.leave_raw_mode();
        }
    }
}

impl Terminal for ProcessTerminal {
    fn write(&mut self, data: &str) {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(data.as_bytes());
        let _ = stdout.flush();
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }

    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[2K\r");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn move_by(&mut self, lines: i32) {
        if lines > 0 {
            self.write(&format!("\x1b[{}B", lines));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{}\x07", title));
    }

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }
}

/// In-memory terminal for testing.
pub struct TestTerminal {
    pub output: Vec<String>,
    pub columns: u16,
    pub rows: u16,
    capabilities: TerminalCapabilities,
}

impl TestTerminal {
    pub fn new(columns: u16, rows: u16) -> Self {
        Self {
            output: Vec::new(),
            columns,
            rows,
            capabilities: TerminalCapabilities::default(),
        }
    }

    pub fn last_output(&self) -> Option<&str> {
        self.output.last().map(|s| s.as_str())
    }
}

impl Terminal for TestTerminal {
    fn write(&mut self, data: &str) {
        self.output.push(data.to_string());
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }

    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[2K\r");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn move_by(&mut self, lines: i32) {
        if lines > 0 {
            self.write(&format!("\x1b[{}B", lines));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{}\x07", title));
    }

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }
}

/// Read raw stdin bytes into a [`StdinBuffer`], emitting events on the
/// returned channel. Cancels cleanly when `shutdown` flips to `true`.
///
/// Exits with `Ok(())` on:
/// - shutdown signal
/// - stdin EOF
/// - receiver dropped
///
/// Returns [`TuiError::Io`] only on real read errors.
pub async fn run_stdin_reader(
    sender: mpsc::UnboundedSender<StdinBufferEvent>,
    shutdown: watch::Receiver<bool>,
) -> TuiResult<()> {
    run_stdin_reader_with(tokio::io::stdin(), sender, shutdown).await
}

/// Generic variant used in tests: pumps any `AsyncRead + Unpin` source
/// through a [`StdinBuffer`] with the same cancellation semantics as
/// [`run_stdin_reader`]. Production code uses the wrapper above.
pub async fn run_stdin_reader_with<R>(
    mut reader: R,
    sender: mpsc::UnboundedSender<StdinBufferEvent>,
    mut shutdown: watch::Receiver<bool>,
) -> TuiResult<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = StdinBuffer::new();
    let mut buf = [0u8; 4096];

    // Already-shutdown is a clean immediate exit.
    if *shutdown.borrow() {
        return Ok(());
    }

    loop {
        tokio::select! {
            biased;
            res = shutdown.changed() => {
                // sender dropped or value flipped — either way, exit.
                if res.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            read = reader.read(&mut buf) => {
                let n = read.map_err(TuiError::Io)?;
                if n == 0 {
                    return Ok(()); // EOF
                }
                for event in buffer.push(&buf[..n]) {
                    if sender.send(event).is_err() {
                        return Ok(()); // receiver dropped
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_terminal_basic() {
        let mut term = TestTerminal::new(80, 24);
        assert_eq!(term.columns(), 80);
        assert_eq!(term.rows(), 24);

        term.write("hello");
        assert_eq!(term.last_output(), Some("hello"));
    }

    #[test]
    fn test_test_terminal_cursor() {
        let mut term = TestTerminal::new(80, 24);
        term.hide_cursor();
        assert_eq!(term.output.last().unwrap(), "\x1b[?25l");
        term.show_cursor();
        assert_eq!(term.output.last().unwrap(), "\x1b[?25h");
    }

    #[test]
    fn test_capabilities_default() {
        let caps = TerminalCapabilities::default();
        assert!(caps.supports_color);
        assert!(caps.supports_unicode);
        assert!(!caps.supports_images);
    }

    #[test]
    fn test_move_by() {
        let mut term = TestTerminal::new(80, 24);
        term.move_by(3);
        assert_eq!(term.output.last().unwrap(), "\x1b[3B");
        term.move_by(-2);
        assert_eq!(term.output.last().unwrap(), "\x1b[2A");
        let len = term.output.len();
        term.move_by(0);
        assert_eq!(term.output.len(), len); // No output for 0 movement
    }

    // ---------- ProcessTerminal raw / alternate-screen toggling ----------
    //
    // These tests exercise the bookkeeping (`is_raw_mode`, `is_alternate_screen`)
    // and the idempotency contract. The actual `crossterm::enable_raw_mode` call
    // is gated behind `#[cfg(not(test))]` (see `enable_raw_mode_impl`) so the
    // test runner's tty is never put into raw mode — otherwise CI would deadlock.
    // Alternate-screen toggling does write the escape sequence to stdout via
    // crossterm; that's harmless under `cargo test`'s captured stdout.

    #[test]
    fn test_raw_mode_idempotent() {
        let mut term = ProcessTerminal::new().expect("ProcessTerminal::new");
        assert!(!term.is_raw_mode());
        term.enter_raw_mode().unwrap();
        assert!(term.is_raw_mode());
        // Second enter is a no-op.
        term.enter_raw_mode().unwrap();
        assert!(term.is_raw_mode());
        term.leave_raw_mode().unwrap();
        assert!(!term.is_raw_mode());
        // Second leave is a no-op.
        term.leave_raw_mode().unwrap();
        assert!(!term.is_raw_mode());
    }

    #[test]
    fn test_alternate_screen_idempotent() {
        let mut term = ProcessTerminal::new().expect("ProcessTerminal::new");
        assert!(!term.is_alternate_screen());
        term.enter_alternate_screen().unwrap();
        assert!(term.is_alternate_screen());
        term.enter_alternate_screen().unwrap();
        assert!(term.is_alternate_screen());
        term.leave_alternate_screen().unwrap();
        assert!(!term.is_alternate_screen());
        term.leave_alternate_screen().unwrap();
        assert!(!term.is_alternate_screen());
    }

    #[test]
    fn test_drop_restores_terminal() {
        // Construct, enter raw + alt screen, drop. The Drop impl must run
        // without panicking. Bookkeeping cannot be observed across drops, but
        // we can at least confirm the path is exercised cleanly.
        {
            let mut term = ProcessTerminal::new().expect("ProcessTerminal::new");
            term.enter_raw_mode().unwrap();
            term.enter_alternate_screen().unwrap();
            assert!(term.is_raw_mode());
            assert!(term.is_alternate_screen());
        } // drop here — must not panic

        // A fresh instance starts clean.
        let term = ProcessTerminal::new().unwrap();
        assert!(!term.is_raw_mode());
        assert!(!term.is_alternate_screen());
    }

    // ---------- run_stdin_reader cancellation ----------

    #[tokio::test]
    async fn test_stdin_reader_exits_on_shutdown() {
        // Use a duplex pair so the reader has a real (but never-written) source.
        let (_writer, reader) = tokio::io::duplex(64);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(run_stdin_reader_with(reader, tx, shutdown_rx));

        // Give the reader a moment to park inside the select.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        shutdown_tx.send(true).unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .expect("reader did not exit within 100ms")
            .expect("task panicked");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stdin_reader_exits_on_send_error() {
        // Drop the receiver, then write a byte. The reader must observe the
        // send failure on its first emitted event and return Ok(()).
        let (mut writer, reader) = tokio::io::duplex(64);
        let (tx, rx) = mpsc::unbounded_channel();
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        drop(rx);

        let task = tokio::spawn(run_stdin_reader_with(reader, tx, shutdown_rx));

        // Push a printable byte — StdinBuffer emits it as a Data event.
        use tokio::io::AsyncWriteExt;
        writer.write_all(b"x").await.unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_millis(200), task)
            .await
            .expect("reader did not exit on send failure within 200ms")
            .expect("task panicked");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stdin_reader_exits_on_eof() {
        // Closing the writer side surfaces as read() returning 0 — clean exit.
        let (writer, reader) = tokio::io::duplex(64);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        drop(writer);

        let task = tokio::spawn(run_stdin_reader_with(reader, tx, shutdown_rx));

        let result = tokio::time::timeout(std::time::Duration::from_millis(200), task)
            .await
            .expect("reader did not exit on EOF within 200ms")
            .expect("task panicked");
        assert!(result.is_ok());
    }
}
