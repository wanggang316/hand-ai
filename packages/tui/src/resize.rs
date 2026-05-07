//! Terminal resize watcher.
//!
//! Spawns a platform-specific task that emits `(cols, rows)` whenever the
//! controlling terminal is resized:
//!
//! - **Unix**: listens for `SIGWINCH` via `tokio::signal::unix`. On each
//!   signal, queries `crossterm::terminal::size()` and forwards.
//! - **Windows**: polls `crossterm::event::poll` and forwards
//!   `Event::Resize(cols, rows)`.
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

    use crossterm::event::{Event, poll, read};

    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                return;
            }

            // crossterm's poll/read are blocking — defer to a thread so the
            // tokio runtime stays responsive.
            let polled = tokio::task::spawn_blocking(|| match poll(Duration::from_millis(50)) {
                Ok(true) => read().ok(),
                _ => None,
            })
            .await;

            match polled {
                Ok(Some(Event::Resize(cols, rows))) => {
                    if tx.send((cols, rows)).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }

            if shutdown.has_changed().unwrap_or(true) && *shutdown.borrow() {
                return;
            }
        }
    });
}

#[cfg(not(any(unix, windows)))]
fn spawn_watcher(_tx: mpsc::UnboundedSender<(u16, u16)>, _shutdown: watch::Receiver<bool>) {
    // No-op: receiver stays open but never produces events.
}
