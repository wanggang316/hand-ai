//! Raw-PTY resize integration test for the rt runtime.
//!
//! Pins the user-visible fix for "dragging the terminal window does not
//! re-lay-out the UI": on a **raw PTY** (per `docs/user-test-patterns.md` —
//! never `tmux capture -S`, whose own resize-reflow contaminates the capture),
//! a `TIOCSWINSZ` + `SIGWINCH` with **no subsequent key events** must produce
//! a repaint at the new width in under a second, anchored at the viewport's
//! real position rather than the screen origin.
//!
//! Both halves of the fix are exercised end to end through the `rt_demo`
//! example running as a child on the PTY slave:
//!
//! - **Bounded-poll pump.** The resize path needs cursor-position queries
//!   (`ESC[6n`) answered while the input pump idles. The old `EventStream`
//!   pump parked crossterm's global reader without a timeout, stranding the
//!   `ESC[..R` reply until the next keypress — this test sends *no* keys, so
//!   it would have stalled ~2s per query and failed the <1s repaint bound.
//! - **No masked cursor failure.** A failed query used to degrade to the
//!   origin, homing the viewport to row 0 over the transcript. The harness
//!   replies with a realistic cursor row, and the test asserts the repainted
//!   box top never lands on the top screen row.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Initial PTY geometry: comfortably wider than the fallback so the demo's
/// real size is in play, but too narrow for the full box title.
const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

/// Post-resize geometry: a horizontal widen (the clean direction — upstream
/// ratatui wipes the visible region on a shrink; see docs/architecture.md).
const RESIZED_COLS: u16 = 100;
const RESIZED_ROWS: u16 = 30;

/// The cursor row (1-based, ANSI) the harness reports for every `ESC[6n`
/// query. Realistic for a session with transcript above the viewport, and
/// deep enough that the inline viewport must anchor well below the top row.
const CURSOR_REPLY_ROW: u16 = 15;

/// A marker only paintable at the new width: the tail of the demo box's
/// title. The title is 86 cells wide, so "s=stream" fits inside the border
/// only at ≥87 columns — it cannot appear in any 80-column frame.
const NEW_WIDTH_MARKER: &[u8] = b"s=stream";

/// The box-drawing top-left corner the demo's bordered active area paints —
/// exactly one per frame, at the box top row.
const BOX_TOP_LEFT: &[u8] = "┌".as_bytes();

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Open a PTY pair at the given geometry.
// `libc::openpty`'s `winp` is `*mut winsize` on macOS but `*const winsize` on
// Linux, so `&mut winsize` is required to compile on macOS yet reads as an
// unnecessary `mut` to clippy on Linux. Allow the lint rather than drop the
// `mut` (which would break the macOS build).
#[allow(clippy::unnecessary_mut_passed)]
fn open_pty(cols: u16, rows: u16) -> (OwnedFd, OwnedFd) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let mut winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut winsize,
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // SAFETY: openpty returned two fresh, owned descriptors.
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// Apply a new window size to the PTY (the kernel also SIGWINCHes the
/// foreground process group of the controlling terminal).
fn set_winsize(master: &OwnedFd, cols: u16, rows: u16) {
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize) };
    assert_eq!(rc, 0, "TIOCSWINSZ failed");
}

/// The prebuilt `rt_demo` example binary. `cargo test` compiles examples
/// alongside the test targets, so it sits next to this test's own binary.
fn rt_demo_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("examples");
    path.push("rt_demo");
    assert!(
        path.exists(),
        "rt_demo example binary not found at {} (cargo test builds examples)",
        path.display()
    );
    path
}

/// A child process killed and reaped on drop, so a failing assertion never
/// leaks a demo process holding the PTY.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn the demo on the PTY slave as a session leader with the slave as its
/// controlling terminal — the shape a real terminal drag targets, and what
/// routes the kernel's SIGWINCH on `TIOCSWINSZ` to the child.
fn spawn_demo(slave: OwnedFd) -> KillOnDrop {
    let stdin = slave.try_clone().expect("dup slave for stdin");
    let stdout = slave.try_clone().expect("dup slave for stdout");
    let mut command = Command::new(rt_demo_binary());
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(slave))
        .env("TERM", "xterm-256color")
        .env_remove("HAND_TUI_FORCE_KITTY_KEYBOARD")
        .env_remove("HAND_TUI_RT_DEMO_FLOOD")
        .env_remove("HAND_TUI_RT_DEMO_STREAM")
        .env_remove("HAND_TUI_RT_DEMO_ASYNC_OVERLAY");
    // SAFETY: only async-signal-safe calls (setsid, ioctl) run between fork
    // and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Stdio is already wired: fd 0 is the PTY slave. Adopt it as the
            // controlling terminal so SIGWINCH is delivered on TIOCSWINSZ.
            if libc::ioctl(0, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    KillOnDrop(command.spawn().expect("spawn rt_demo on the PTY slave"))
}

/// Everything the child has written to the PTY, accumulated by the reader
/// thread.
type SharedOutput = Arc<Mutex<Vec<u8>>>;

/// Start the harness reader: accumulate child output and answer terminal
/// queries the way a real terminal would —
///
/// - `ESC[6n` (cursor position, the resize path's query) with
///   `ESC[{CURSOR_REPLY_ROW};1R`;
/// - `ESC[c` (primary device attributes, crossterm's keyboard-enhancement
///   probe terminator) with a VT100-class `ESC[?1;2c`, so startup never waits
///   out the probe timeout. The kitty `ESC[?u` query itself is left
///   unanswered, as on a terminal without the protocol.
fn start_reader(master: &OwnedFd) -> SharedOutput {
    let output: SharedOutput = Arc::new(Mutex::new(Vec::new()));
    let shared = output.clone();
    let mut reader = std::fs::File::from(master.try_clone().expect("dup master for reading"));
    let mut writer = std::fs::File::from(master.try_clone().expect("dup master for writing"));
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut answered_cursor = 0usize;
        let mut answered_da1 = 0usize;
        loop {
            let read = match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break, // child exited / PTY closed
                Ok(n) => n,
            };
            let (cursor_queries, da1_queries) = {
                let mut buffer = shared.lock().expect("output mutex poisoned");
                buffer.extend_from_slice(&chunk[..read]);
                (
                    count_occurrences(&buffer, b"\x1b[6n"),
                    count_occurrences(&buffer, b"\x1b[c"),
                )
            };
            while answered_cursor < cursor_queries {
                let reply = format!("\x1b[{CURSOR_REPLY_ROW};1R");
                if writer.write_all(reply.as_bytes()).is_err() {
                    return;
                }
                answered_cursor += 1;
            }
            while answered_da1 < da1_queries {
                if writer.write_all(b"\x1b[?1;2c").is_err() {
                    return;
                }
                answered_da1 += 1;
            }
        }
    });
    output
}

/// Poll the shared output until `predicate` holds, or fail after `timeout`.
fn wait_for(
    output: &SharedOutput,
    timeout: Duration,
    what: &str,
    predicate: impl Fn(&[u8]) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let buffer = output.lock().expect("output mutex poisoned");
            if predicate(&buffer) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {timeout:?} waiting for {what}; got {} bytes: {:?}",
                    buffer.len(),
                    String::from_utf8_lossy(&buffer[buffer.len().saturating_sub(512)..])
                );
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until the child has gone quiet: no output growth for `quiet`.
fn settle(output: &SharedOutput, quiet: Duration, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_len = output.lock().expect("output mutex poisoned").len();
    let mut quiet_since = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
        let len = output.lock().expect("output mutex poisoned").len();
        if len != last_len {
            last_len = len;
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= quiet {
            return;
        }
    }
    panic!("child never went idle before the resize");
}

/// The 1-based ANSI screen rows at which a `┌` (box top-left) cell is painted
/// in `bytes`, resolved from the most recent cursor-position (`CSI r ; c H`)
/// sequence preceding it. ratatui positions every diff run with an explicit
/// move, so the row current at the `┌` byte is the row it is painted on.
fn box_corner_rows(bytes: &[u8]) -> Vec<u16> {
    let mut rows = Vec::new();
    let mut current_row: u16 = 1;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // A CSI sequence: params/intermediates run to the final byte
            // (0x40..=0x7e).
            let mut j = i + 2;
            while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                j += 1;
            }
            if j >= bytes.len() {
                break; // truncated trailing sequence
            }
            if bytes[j] == b'H' {
                let params = std::str::from_utf8(&bytes[i + 2..j]).unwrap_or("");
                current_row = params
                    .split(';')
                    .next()
                    .and_then(|row| row.parse::<u16>().ok())
                    .unwrap_or(1);
            }
            i = j + 1;
            continue;
        }
        if bytes[i..].starts_with(BOX_TOP_LEFT) {
            rows.push(current_row);
            i += BOX_TOP_LEFT.len();
            continue;
        }
        i += 1;
    }
    rows
}

/// Drag-resize regression: `TIOCSWINSZ` + `SIGWINCH` alone (no keypresses)
/// must repaint at the new width within a second, with the viewport anchored
/// below the top screen row.
#[test]
fn resize_alone_repaints_at_new_width_off_the_top_row() {
    let (master, slave) = open_pty(INITIAL_COLS, INITIAL_ROWS);
    let child = spawn_demo(slave);
    let output = start_reader(&master);

    // First frame: the demo's box title arrives (its head fits at 80 cols).
    // Generous bound: the very first exec of a freshly linked binary can stall
    // on a loaded host (e.g. macOS scanning it) before the demo even starts;
    // only the post-resize latency below is the timing under test.
    wait_for(
        &output,
        Duration::from_secs(30),
        "the first frame",
        |bytes| contains(bytes, b"rt_demo"),
    );
    assert!(
        !contains(
            &output.lock().expect("output mutex poisoned"),
            NEW_WIDTH_MARKER
        ),
        "the new-width marker must not fit in an 80-column frame"
    );

    // Let startup painting finish so post-resize bytes are unambiguous.
    settle(&output, Duration::from_millis(300), Duration::from_secs(10));
    let resize_offset = output.lock().expect("output mutex poisoned").len();

    // The drag: new winsize + SIGWINCH, and afterwards NOT ONE key event —
    // the repaint may not depend on a keypress unblocking the event pump.
    set_winsize(&master, RESIZED_COLS, RESIZED_ROWS);
    let rc = unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGWINCH) };
    assert_eq!(rc, 0, "kill(SIGWINCH) failed");
    let resized_at = Instant::now();

    // (i) A repaint carrying content only paintable at the new width lands
    // within the 1s bound (the stranded-cursor-reply bug stalled ~2s per
    // query and needed a keypress to unwedge).
    wait_for(
        &output,
        Duration::from_secs(1),
        "a new-width repaint after SIGWINCH",
        |bytes| contains(&bytes[resize_offset..], NEW_WIDTH_MARKER),
    );
    let elapsed = resized_at.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "resize repaint took {elapsed:?}, expected <1s"
    );

    // (ii) The repainted box top sits at the viewport's real anchor — never
    // on the top screen row (the masked-cursor-failure bug homed it to the
    // origin, over the transcript).
    let post_resize = output.lock().expect("output mutex poisoned")[resize_offset..].to_vec();
    let corner_rows = box_corner_rows(&post_resize);
    assert!(
        !corner_rows.is_empty(),
        "expected at least one repainted box corner after the resize"
    );
    assert!(
        corner_rows.iter().all(|&row| row >= 2),
        "box top must not be painted on screen row 1 (viewport homed to origin): rows {corner_rows:?}"
    );

    drop(child); // kill + reap
}
