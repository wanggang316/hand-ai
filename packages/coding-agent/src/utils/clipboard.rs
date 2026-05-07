//! Clipboard text helpers.
//!
//! Mirrors `pi-coding-agent`'s `clipboard.ts` + `clipboard-native.ts`. The
//! TS split lived between the JS bridge (`@mariozechner/clipboard`) and a
//! shell-out fallback. In Rust we let [`arboard`] handle the cross-platform
//! native path and fall back to OSC 52 for SSH / mosh / Termux sessions
//! where there is no usable display server.
//!
//! ## Resolution order
//!
//! 1. Try `arboard::Clipboard::set_text` — works on macOS, Windows, and X11
//!    sessions with `xclip`/`xsel` available.
//! 2. If we look like a remote session (`SSH_CONNECTION`, `SSH_CLIENT`,
//!    `MOSH_CONNECTION`) AND step 1 succeeded, also emit OSC 52 so the
//!    upstream terminal mirrors the value into the *local* clipboard.
//! 3. If step 1 failed, emit OSC 52 as the sole transport.
//!
//! When the payload exceeds [`MAX_OSC52_ENCODED_LENGTH`] OSC 52 is skipped —
//! oversized escape sequences corrupt some terminals.

use std::io::Write;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use thiserror::Error;

/// Maximum base64-encoded length for an OSC 52 payload before we refuse to
/// emit it. Mirrors the TS heuristic.
pub const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

/// Errors raised while attempting to copy text to the clipboard.
#[derive(Debug, Error)]
pub enum ClipboardError {
    /// All transports refused or failed.
    #[error("failed to copy to clipboard: every transport failed")]
    AllTransportsFailed,
    /// Writing to stdout for the OSC 52 fallback failed.
    #[error("failed to emit OSC 52 escape: {0}")]
    Osc52Io(#[source] std::io::Error),
}

/// `true` when one of the well-known SSH / mosh env vars is set, hinting
/// the local terminal is upstream of the running process.
pub fn is_remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("MOSH_CONNECTION").is_some()
}

/// Emit an OSC 52 clipboard escape sequence on stdout.
///
/// Returns `Ok(true)` when the escape was emitted, `Ok(false)` when the
/// payload was too large to safely emit (terminals start truncating around
/// 100 000 base64 characters). I/O errors are surfaced as
/// [`ClipboardError::Osc52Io`].
pub fn emit_osc52(text: &str) -> Result<bool, ClipboardError> {
    let encoded = BASE64.encode(text.as_bytes());
    if encoded.len() > MAX_OSC52_ENCODED_LENGTH {
        return Ok(false);
    }
    let mut stdout = std::io::stdout().lock();
    write!(stdout, "\x1b]52;c;{encoded}\x07").map_err(ClipboardError::Osc52Io)?;
    stdout.flush().map_err(ClipboardError::Osc52Io)?;
    Ok(true)
}

/// Copy `text` to the system clipboard.
///
/// See the module docs for the full transport order. Returns
/// [`ClipboardError::AllTransportsFailed`] when neither the native handle
/// nor the OSC 52 fallback succeeded.
pub fn copy_to_clipboard(text: &str) -> Result<(), ClipboardError> {
    let native_ok = match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).is_ok(),
        Err(_) => false,
    };

    let remote = is_remote_session();
    if native_ok && !remote {
        return Ok(());
    }

    // Either native failed, or we're remote and want OSC 52 to mirror the
    // value upstream regardless.
    let osc52_ok = match emit_osc52(text) {
        Ok(emitted) => emitted,
        Err(ClipboardError::Osc52Io(_)) => false,
        Err(other) => return Err(other),
    };

    if native_ok || osc52_ok {
        Ok(())
    } else {
        Err(ClipboardError::AllTransportsFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_osc52_skips_oversized_payload() {
        // 4 bytes encode to ~6 base64 chars; produce input large enough
        // that the encoded form exceeds the cap.
        let oversized = "x".repeat(MAX_OSC52_ENCODED_LENGTH);
        let emitted = emit_osc52(&oversized).expect("io should not fail");
        assert!(!emitted, "oversized payload must be skipped");
    }

    #[test]
    fn emit_osc52_writes_small_payload() {
        // We can't easily intercept stdout in a unit test without going
        // through the test harness's capture, so we settle for verifying
        // the helper returns Ok(true) without panicking on a tiny input.
        let emitted = emit_osc52("hello").expect("io should not fail");
        assert!(emitted, "small payload must be emitted");
    }

    #[test]
    fn is_remote_session_reads_env() {
        // The result depends on the test process's env, so we only verify
        // the function doesn't panic and returns a bool. Exhaustive
        // env-mutation testing is unsafe in a multi-threaded test runner.
        let _ = is_remote_session();
    }

    /// Round-trip the native clipboard. Gated to macOS — Linux CI rarely
    /// has a display server, and on Windows the test runner can race
    /// against other processes that own the clipboard. Run locally with
    /// `cargo test -p hand-coding-agent --lib clipboard::` to exercise it.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_round_trip_when_clipboard_available() {
        if std::env::var_os("CI").is_some() {
            // CI runners may not provide a clipboard service even on
            // macOS; skip rather than fail spuriously.
            return;
        }
        let Ok(mut cb) = arboard::Clipboard::new() else {
            // No clipboard service — skip.
            return;
        };
        let payload = "hand-ai clipboard round-trip test";
        if cb.set_text(payload.to_string()).is_err() {
            return;
        }
        let got = cb.get_text().expect("get_text after set_text");
        assert_eq!(got, payload);
    }
}
