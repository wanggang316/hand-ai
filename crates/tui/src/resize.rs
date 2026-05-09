//! Terminal resize watcher.
//!
//! Spawns a platform-specific task that emits `(cols, rows)` whenever the
//! controlling terminal is resized:
//!
//! - **Unix**: listens for `SIGWINCH` via `tokio::signal::unix`. On each
//!   signal, queries `crossterm::terminal::size()` and forwards.
//! - **Windows**: polls `crossterm::terminal::size()` periodically and emits
//!   when the (cols, rows) tuple changes. We deliberately avoid
//!   `crossterm::event::read()` here because it drains every console event
//!   (keys, mouse, focus) from the same handle the stdin reader is using —
//!   on Windows that would silently steal user keystrokes.
//! - **Other**: returns an empty receiver — no events ever arrive.
//!
//! The watcher exits when `shutdown` flips to `true` or the receiver is
//! dropped (best-effort on the latter; sends are non-fatal).

use tokio::sync::{mpsc, watch};

/// Spawn a platform-specific resize watcher and return the receiver. The
/// task exits when `shutdown` is signalled or the receiver is dropped.
pub fn watch_resizes(shutdown: watch::Receiver<bool>) -> mpsc::UnboundedReceiver<(u16, u16)> {
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_watcher(tx, shutdown);
    rx
}

#[cfg(unix)]
fn spawn_watcher(tx: mpsc::UnboundedSender<(u16, u16)>, mut shutdown: watch::Receiver<bool>) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let mut sig = match signal(SignalKind::window_change()) {
            Ok(s) => s,
            Err(_) => return,
        };

        if *shutdown.borrow() {
            return;
        }

        loop {
            tokio::select! {
                biased;
                res = shutdown.changed() => {
                    if res.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                got = sig.recv() => {
                    if got.is_none() {
                        return;
                    }
                    if let Ok((cols, rows)) = crossterm::terminal::size()
                        && tx.send((cols, rows)).is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_watcher(tx: mpsc::UnboundedSender<(u16, u16)>, mut shutdown: watch::Receiver<bool>) {
    use std::time::Duration;

    // Windows has no SIGWINCH equivalent that's accessible without claiming
    // the console event handle. The previous implementation called
    // `crossterm::event::read()`, which consumes ALL console events — keys,
    // mouse, focus — from the same handle the stdin reader uses. Two
    // consumers on one handle silently lose keystrokes.
    //
    // Poll `terminal::size()` instead: it's cheap, doesn't consume events,
    // and a 200ms cadence is well below human perception for resize.
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    tokio::spawn(async move {
        let (mut last_cols, mut last_rows) = crossterm::terminal::size().unwrap_or((0, 0));
        loop {
            tokio::select! {
                biased;
                res = shutdown.changed() => {
                    if res.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {
                    if let Ok((cols, rows)) = crossterm::terminal::size()
                        && (cols, rows) != (last_cols, last_rows)
                    {
                        last_cols = cols;
                        last_rows = rows;
                        if tx.send((cols, rows)).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
}

#[cfg(not(any(unix, windows)))]
fn spawn_watcher(_tx: mpsc::UnboundedSender<(u16, u16)>, _shutdown: watch::Receiver<bool>) {
    // No-op: receiver stays open but never produces events.
}
