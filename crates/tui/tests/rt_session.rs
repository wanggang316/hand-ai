//! Unit tests for the rt terminal session lifecycle helpers
//! (`hand_tui::rt::session`).
//!
//! Covers the pieces that are testable without a live terminal: enter/restore
//! escape sequences, fallback geometry, the size-fallback backend, and TTY
//! detection.

use std::io;

use hand_tui::rt::session::{
    FALLBACK_COLS, FALLBACK_ROWS, FallbackSizeBackend, SessionError, check_tty, effective_size,
    write_enter_sequences, write_restore_sequences,
};
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::widgets::Block;
use ratatui::{Terminal, TerminalOptions, Viewport};

const ENABLE_PASTE: &[u8] = b"\x1b[?2004h";
const DISABLE_PASTE: &[u8] = b"\x1b[?2004l";
const KITTY_POP: &[u8] = b"\x1b[<1u";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const MOUSE_ENABLE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?1006h",
    b"\x1b[?1015h",
];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Finds a kitty keyboard push sequence (`CSI > <bits> u`) and returns its
/// flag bits, or `None` when no push sequence is present.
fn kitty_push_bits(bytes: &[u8]) -> Option<u32> {
    let start = bytes.windows(3).position(|window| window == b"\x1b[>")?;
    let rest = &bytes[start + 3..];
    let end = rest.iter().position(|&byte| byte == b'u')?;
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
}

// --- fallback geometry -----------------------------------------------------

#[test]
fn effective_size_falls_back_only_when_a_dimension_is_zero() {
    assert_eq!(effective_size(0, 0), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(effective_size(0, 30), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(effective_size(120, 0), (FALLBACK_COLS, FALLBACK_ROWS));
    assert_eq!(effective_size(120, 40), (120, 40));
    assert_eq!(effective_size(1, 1), (1, 1));
}

// --- enter / restore escape sequences ---------------------------------------

#[test]
fn enter_sequences_plain_enable_bracketed_paste_only() {
    let mut buf = Vec::new();
    write_enter_sequences(&mut buf, false).unwrap();
    assert!(contains(&buf, ENABLE_PASTE), "missing ?2004h");
    assert_eq!(
        kitty_push_bits(&buf),
        None,
        "plain enter must not push kitty flags"
    );
}

#[test]
fn enter_sequences_kitty_push_disambiguate_and_event_types() {
    let mut buf = Vec::new();
    write_enter_sequences(&mut buf, true).unwrap();
    assert!(contains(&buf, ENABLE_PASTE), "missing ?2004h");
    let bits = kitty_push_bits(&buf).expect("kitty push sequence present");
    assert_eq!(bits & 0b01, 0b01, "DISAMBIGUATE_ESCAPE_CODES must be set");
    assert_eq!(bits & 0b10, 0b10, "REPORT_EVENT_TYPES must be set");
}

#[test]
fn restore_sequences_plain_disable_paste_and_show_cursor() {
    let mut buf = Vec::new();
    write_restore_sequences(&mut buf, false).unwrap();
    assert!(contains(&buf, DISABLE_PASTE), "missing ?2004l");
    assert!(contains(&buf, SHOW_CURSOR), "missing cursor show");
    assert!(
        !contains(&buf, KITTY_POP),
        "plain restore must not pop kitty flags"
    );
}

#[test]
fn restore_sequences_kitty_pops_flags() {
    let mut buf = Vec::new();
    write_restore_sequences(&mut buf, true).unwrap();
    assert!(contains(&buf, KITTY_POP), "missing kitty pop");
    assert!(contains(&buf, DISABLE_PASTE), "missing ?2004l");
    assert!(contains(&buf, SHOW_CURSOR), "missing cursor show");
}

#[test]
fn no_mouse_capture_sequences_ever() {
    for kitty in [false, true] {
        let mut enter = Vec::new();
        write_enter_sequences(&mut enter, kitty).unwrap();
        let mut restore = Vec::new();
        write_restore_sequences(&mut restore, kitty).unwrap();
        for sequence in MOUSE_ENABLE_SEQUENCES {
            assert!(
                !contains(&enter, sequence) && !contains(&restore, sequence),
                "mouse capture sequence {:?} emitted (kitty={kitty})",
                String::from_utf8_lossy(sequence),
            );
        }
    }
}

// --- size-fallback backend ---------------------------------------------------

/// Minimal in-memory backend whose size / cursor / window-size queries can be
/// configured to succeed or fail; every other operation is a no-op.
struct FakeBackend {
    size: Option<Size>,
    cursor: Option<Position>,
    window: Option<WindowSize>,
}

impl FakeBackend {
    fn new(size: Option<Size>, cursor: Option<Position>) -> Self {
        let window = size.map(|size| WindowSize {
            columns_rows: size,
            pixels: Size::new(0, 0),
        });
        Self {
            size,
            cursor,
            window,
        }
    }
}

fn fake_error() -> io::Error {
    io::Error::other("fake backend failure")
}

impl Backend for FakeBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        content.for_each(drop);
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.cursor.ok_or_else(fake_error)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, _position: P) -> io::Result<()> {
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        self.size.ok_or_else(fake_error)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.window.ok_or_else(fake_error)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn scroll_region_up(
        &mut self,
        _region: std::ops::Range<u16>,
        _line_count: u16,
    ) -> io::Result<()> {
        Ok(())
    }

    fn scroll_region_down(
        &mut self,
        _region: std::ops::Range<u16>,
        _line_count: u16,
    ) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn fallback_backend_substitutes_80x24_for_zero_size() {
    let backend = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(0, 0)), None));
    assert_eq!(
        backend.size().unwrap(),
        Size::new(FALLBACK_COLS, FALLBACK_ROWS)
    );
}

#[test]
fn fallback_backend_passes_real_size_through() {
    let backend = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(132, 43)), None));
    assert_eq!(backend.size().unwrap(), Size::new(132, 43));
}

#[test]
fn fallback_backend_survives_size_error() {
    let backend = FallbackSizeBackend::new(FakeBackend::new(None, None));
    assert_eq!(
        backend.size().unwrap(),
        Size::new(FALLBACK_COLS, FALLBACK_ROWS)
    );
}

#[test]
fn fallback_backend_cursor_error_propagates_on_a_sized_terminal() {
    // A failed cursor query on a live, sized terminal is a real fault: masking
    // it as the origin would anchor the inline viewport at row 0, over the
    // transcript, and home `Terminal::clear` to the screen top (the resize
    // regression). It must propagate.
    let mut backend = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(80, 24)), None));
    assert!(
        backend.get_cursor_position().is_err(),
        "a cursor failure on nonzero geometry must not be masked as the origin"
    );
}

#[test]
fn fallback_backend_cursor_error_becomes_origin_on_degenerate_geometry() {
    // A 0x0 (or size-unqueryable) PTY is the headless case the wrapper exists
    // for: the cursor query legitimately goes unanswered there, so the origin
    // keeps rendering alive — the same signal the size fallback keys on
    // (VAL-COMPAT-011).
    let mut zeroed = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(0, 0)), None));
    assert_eq!(zeroed.get_cursor_position().unwrap(), Position::ORIGIN);

    let mut unqueryable = FallbackSizeBackend::new(FakeBackend::new(None, None));
    assert_eq!(unqueryable.get_cursor_position().unwrap(), Position::ORIGIN);
}

#[test]
fn fallback_backend_cursor_passthrough() {
    let mut backend = FallbackSizeBackend::new(FakeBackend::new(
        Some(Size::new(80, 24)),
        Some(Position::new(5, 7)),
    ));
    assert_eq!(backend.get_cursor_position().unwrap(), Position::new(5, 7));
}

#[test]
fn fallback_backend_window_size_fallback_on_error_and_zero() {
    let mut erroring = FallbackSizeBackend::new(FakeBackend::new(None, None));
    let window = erroring.window_size().unwrap();
    assert_eq!(window.columns_rows, Size::new(FALLBACK_COLS, FALLBACK_ROWS));

    let mut zeroed = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(0, 0)), None));
    let window = zeroed.window_size().unwrap();
    assert_eq!(window.columns_rows, Size::new(FALLBACK_COLS, FALLBACK_ROWS));
}

#[test]
fn inline_terminal_renders_at_fallback_geometry_on_zero_size_pty() {
    // A 0x0 PTY: size reports zero, cursor position queries go unanswered.
    let backend = FallbackSizeBackend::new(FakeBackend::new(Some(Size::new(0, 0)), None));
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(3),
        },
    )
    .expect("inline terminal init must survive a 0x0 PTY");
    terminal
        .draw(|frame| {
            assert_eq!(frame.area(), Rect::new(0, 0, FALLBACK_COLS, 3));
            frame.render_widget(Block::bordered(), frame.area());
        })
        .expect("draw must succeed at fallback geometry");
}

// --- TTY detection -----------------------------------------------------------

#[test]
fn check_tty_rejects_a_pipe() {
    let (reader, _writer) = io::pipe().unwrap();
    let result = check_tty(&reader);
    assert!(matches!(result, Err(SessionError::NotATty)));
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("TTY") || message.contains("terminal"),
        "diagnostic must be readable, got: {message}"
    );
}

#[cfg(unix)]
#[test]
fn check_tty_accepts_a_pty_slave() {
    use std::fs::File;
    use std::os::fd::FromRawFd;

    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    let master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    check_tty(&slave).expect("a PTY slave is a TTY");
    drop(slave);
    drop(master);
}

// --- SIGHUP listener (stdin-close clean exit, VAL-CORE-022) ------------------

/// The SIGHUP listener resolves when a hangup is delivered, so the run loop can
/// route a closing PTY master (stdin close) to the same clean-exit-and-restore
/// path a Ctrl+D takes — instead of the default disposition killing the process
/// raw before `SessionGuard` restores the terminal.
///
/// Registering the listener is what supersedes the terminate-on-hangup default
/// (tokio installs a process-wide handler): the self-sent SIGHUP below does not
/// kill the test binary, it is delivered to the listeners. This is a single
/// test — not two — precisely because it `raise`s a real SIGHUP into this
/// process: splitting it would let two tests `raise` concurrently, and a SIGHUP
/// landing in the window before a listener is registered would terminate the
/// whole test binary under the default disposition.
#[cfg(unix)]
#[test]
fn hangup_listener_wakes_on_delivered_sighup() {
    use hand_tui::rt::session::hangup_listener;
    use std::time::Duration;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    runtime.block_on(async {
        // Two coexisting listeners confirm the handler is installed process-wide
        // (a hangup wakes both) rather than consumed by a single waiter.
        let mut first = hangup_listener().expect("register first SIGHUP listener");
        let mut second = hangup_listener().expect("register second SIGHUP listener");

        // Self-send a SIGHUP only after both listeners are registered, so the
        // default terminate disposition is already superseded and the process
        // survives to observe the signal.
        let rc = unsafe { libc::raise(libc::SIGHUP) };
        assert_eq!(rc, 0, "raise(SIGHUP) failed");

        // Reaching either recv proves the default disposition was superseded;
        // both waking proves the handler is shared, not single-consumer.
        let first_woke = tokio::time::timeout(Duration::from_secs(5), first.recv())
            .await
            .expect("first listener must wake within the timeout");
        let second_woke = tokio::time::timeout(Duration::from_secs(5), second.recv())
            .await
            .expect("second listener must wake within the timeout");
        assert_eq!(
            first_woke,
            Some(()),
            "a delivered SIGHUP resolves to Some(())"
        );
        assert_eq!(
            second_woke,
            Some(()),
            "both listeners observe the same hangup"
        );
    });
}
