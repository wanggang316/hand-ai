//! Stdout containment for protocol modes (RPC / print).
//!
//! Mirrors `pi-mono/packages/coding-agent/src/core/output-guard.ts`. In TS,
//! Node lets a process monkey-patch `process.stdout.write` so that every
//! ambient `console.log` or third-party stdout write is silently rerouted
//! to stderr. This protects modes whose stdout is a structured JSONL
//! channel (RPC, `--print`) from being corrupted by stray prints.
//!
//! ## Rust adaptation
//!
//! Rust does not let us swap `std::io::stdout()`'s writer at runtime:
//! `println!` and `Stdout` go through a global, non-replaceable handle.
//! Rather than fight the runtime, this module provides the same API
//! surface as the TS reference but defines it as a *protocol contract*:
//!
//! - In RPC / print mode, code that emits protocol bytes calls
//!   [`write_raw_stdout`] (or [`flush_raw_stdout`]). General logging
//!   already goes through `tracing`, which targets stderr by default.
//! - [`take_over_stdout`] flips a process-wide flag. While the flag is
//!   set, [`is_stdout_taken_over`] returns `true`, which downstream
//!   helpers can use to gate non-protocol prints.
//! - [`restore_stdout`] clears the flag.
//!
//! The flag is held in a `Mutex<Option<TakeoverState>>`; the cached
//! [`std::io::Stdout`] handle inside is reused across calls so we do not
//! re-`lock()` from scratch per write. The mutex is process-wide because
//! stdout itself is process-wide; trying to scope this per-thread would
//! be misleading (one thread's "raw" write would still hit a sibling
//! thread's `println!`).
//!
//! ## What this is not
//!
//! - It does **not** intercept ambient `println!` / `eprintln!` calls
//!   the way the TS monkey-patch does. Code that wants its bytes routed
//!   correctly must use [`write_raw_stdout`].
//! - It does **not** capture stderr. The TS reference captures `stderr`
//!   only to redirect *stdout* to *stderr*; we have no equivalent
//!   redirection here.

use std::io::{self, Stdout, Write};
use std::sync::Mutex;

/// State held while stdout is "taken over" — a cached [`Stdout`] handle
/// used by [`write_raw_stdout`] and [`flush_raw_stdout`] for protocol
/// writes.
#[derive(Debug)]
struct TakeoverState {
    stdout: Stdout,
}

static STATE: Mutex<Option<TakeoverState>> = Mutex::new(None);

/// Errors raised by stdout-guard operations.
#[derive(Debug, thiserror::Error)]
pub enum OutputGuardError {
    /// I/O error from the underlying [`Stdout`] handle.
    #[error("stdout I/O error: {0}")]
    Io(#[from] io::Error),
    /// The internal mutex was poisoned by a panicking thread.
    #[error("output-guard mutex poisoned")]
    Poisoned,
}

fn lock_state() -> Result<std::sync::MutexGuard<'static, Option<TakeoverState>>, OutputGuardError> {
    STATE.lock().map_err(|_| OutputGuardError::Poisoned)
}

/// Mark stdout as "taken over" by a protocol writer.
///
/// Idempotent: calling this twice without an intervening
/// [`restore_stdout`] is a no-op. Mirrors `takeOverStdout()` in TS.
pub fn take_over_stdout() {
    let Ok(mut guard) = lock_state() else {
        // Mutex poisoned — silently bail. The poisoning thread already
        // panicked; surfacing a second error here would be noise.
        return;
    };
    if guard.is_some() {
        return;
    }
    *guard = Some(TakeoverState {
        stdout: io::stdout(),
    });
}

/// Clear the "taken over" flag. Idempotent.
///
/// Mirrors `restoreStdout()` in TS.
pub fn restore_stdout() {
    let Ok(mut guard) = lock_state() else {
        return;
    };
    *guard = None;
}

/// Whether stdout is currently flagged as taken over.
///
/// Mirrors `isStdoutTakenOver()` in TS.
pub fn is_stdout_taken_over() -> bool {
    lock_state().map(|g| g.is_some()).unwrap_or(false)
}

/// Write `text` to the protocol stdout handle.
///
/// Whether stdout has been taken over or not, this routes through the
/// process [`Stdout`]. Mirrors `writeRawStdout(text)` in TS — TS
/// preserved the pre-takeover writer; Rust cannot replace it, so the
/// distinction is purely informational here.
pub fn write_raw_stdout(text: &str) -> Result<(), OutputGuardError> {
    let mut guard = lock_state()?;
    match guard.as_mut() {
        Some(state) => {
            let mut handle = state.stdout.lock();
            handle.write_all(text.as_bytes())?;
        }
        None => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(text.as_bytes())?;
        }
    }
    Ok(())
}

/// Flush the protocol stdout handle.
///
/// Mirrors `flushRawStdout()` in TS — that function writes an empty
/// chunk and awaits the callback; Rust's [`Write::flush`] is the direct
/// analogue.
pub fn flush_raw_stdout() -> Result<(), OutputGuardError> {
    let mut guard = lock_state()?;
    match guard.as_mut() {
        Some(state) => {
            let mut handle = state.stdout.lock();
            handle.flush()?;
        }
        None => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            handle.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag-state operations all gate on a single process-wide
    /// mutex, so the entire lifecycle is exercised in one
    /// sequentially-ordered test. Cargo's thread-per-test scheduler
    /// would otherwise race parallel tests against each other through
    /// the shared `STATE` slot.
    #[test]
    fn takeover_lifecycle_idempotent_observable_and_io_safe() {
        // Start clean — assertions read the shared global flag.
        restore_stdout();
        assert!(!is_stdout_taken_over());

        // Pre-takeover I/O should still be valid: empty writes/flushes
        // touch the global stdout via the same code path used in
        // protocol mode.
        write_raw_stdout("").expect("empty write should succeed pre-takeover");
        flush_raw_stdout().expect("flush should succeed pre-takeover");

        take_over_stdout();
        assert!(is_stdout_taken_over());

        // Idempotent: a second take-over is a no-op.
        take_over_stdout();
        assert!(is_stdout_taken_over());

        // Post-takeover I/O still works through the cached handle.
        write_raw_stdout("").expect("empty write should succeed under takeover");
        flush_raw_stdout().expect("flush should succeed under takeover");

        restore_stdout();
        assert!(!is_stdout_taken_over());

        // Idempotent: restoring an already-restored guard is a no-op.
        restore_stdout();
        assert!(!is_stdout_taken_over());
    }
}
