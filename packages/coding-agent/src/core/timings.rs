//! Startup-profiling helper. Mirrors
//! `pi-mono/packages/coding-agent/src/core/timings.ts`.
//!
//! A tiny global checkpoint log used to profile process startup. Each call
//! to [`time`] records the label and the milliseconds elapsed since the
//! previous checkpoint (or since [`reset`] / first call). [`print`]
//! dumps the log to stderr. Both are no-ops unless the env-var gate is on.
//!
//! Usage:
//!
//! ```ignore
//! use hand_coding_agent::core::timings;
//! timings::reset();
//! // ... parse args ...
//! timings::time("parse_args");
//! // ... migrations ...
//! timings::time("migrations");
//! // ... at process exit:
//! timings::print();
//! ```
//!
//! ## Rebrand note
//!
//! The TS reference reads `PI_TIMING`. This Rust port reads `HAND_TIMING`
//! for project consistency with the rest of the rebrand (binary name
//! `hand`, settings dir `~/.hand/`, telemetry env var `HAND_TELEMETRY`).
//! The env-var name is the only intentional deviation; truthy parsing,
//! storage shape, and output format match TS.
//!
//! ## Scope (intentional)
//!
//! This is a global startup-profiling helper, exactly mirroring TS. It is
//! NOT a per-turn collector; there is no `Timings` struct, no
//! `AgentSession::last_turn_timings()` accessor, no model / tool /
//! compaction phase tracking. Per-turn instrumentation does not exist in
//! the TS reference and is out of scope here.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Recorded checkpoint. `ms` is elapsed since the previous checkpoint
/// (or since [`reset`] / first call when there is no previous one).
#[derive(Debug, Clone)]
struct Checkpoint {
    label: String,
    ms: u64,
}

/// Process-wide checkpoint log. Mirrors `let timings = []` in TS.
fn timings_log() -> &'static Mutex<Vec<Checkpoint>> {
    static LOG: OnceLock<Mutex<Vec<Checkpoint>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

/// Process-wide marker for "previous checkpoint instant". Mirrors
/// `let lastTime = Date.now()` in TS, but uses `Instant` for monotonicity.
fn timings_marker() -> &'static Mutex<Option<Instant>> {
    static MARKER: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    MARKER.get_or_init(|| Mutex::new(None))
}

/// Truthy parsing matching `core::telemetry::is_truthy_env_flag`:
/// `"1"`, `"true"`, `"yes"` (case-insensitive on the latter two).
fn is_truthy_env_flag(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if value == "1" {
        return true;
    }
    let lower = value.to_lowercase();
    lower == "true" || lower == "yes"
}

/// Whether the global gate is on. Reads `HAND_TIMING` from the
/// environment each call (matches TS `process.env.PI_TIMING === "1"`
/// being read at module load; reading per-call lets tests toggle it).
///
/// Public so diagnostics (and similar inspectors) can report the gate
/// state without re-implementing the truthy-parse rules.
pub fn enabled() -> bool {
    std::env::var("HAND_TIMING")
        .ok()
        .as_deref()
        .map(is_truthy_env_flag)
        .unwrap_or(false)
}

/// Clear the log and reset the marker. No-op when the gate is off.
pub fn reset() {
    if !enabled() {
        return;
    }
    record_reset(timings_log(), timings_marker());
}

/// Record a checkpoint with the milliseconds elapsed since the previous
/// one (or since [`reset`] / first call). No-op when the gate is off.
pub fn time(label: &str) {
    if !enabled() {
        return;
    }
    record_time(label, timings_log(), timings_marker());
}

/// Print the recorded log to stderr in the same format as TS:
/// a header, one row per checkpoint (`  label: Nms`), and a TOTAL.
/// No-op when the gate is off or when nothing has been recorded.
pub fn print() {
    if !enabled() {
        return;
    }
    let log = timings_log().lock().unwrap();
    if log.is_empty() {
        return;
    }
    eprintln!("\n--- Startup Timings ---");
    let mut total: u64 = 0;
    for c in log.iter() {
        eprintln!("  {}: {}ms", c.label, c.ms);
        total = total.saturating_add(c.ms);
    }
    eprintln!("  TOTAL: {}ms", total);
    eprintln!("------------------------\n");
}

// --- Test seam -------------------------------------------------------------
//
// The functions below take the log/marker as arguments so tests can drive
// the recording logic deterministically without touching process-global
// state or the env var. The public `time` / `reset` / `print` functions
// are thin wrappers that bind the global statics.

fn record_reset(log: &Mutex<Vec<Checkpoint>>, marker: &Mutex<Option<Instant>>) {
    log.lock().unwrap().clear();
    *marker.lock().unwrap() = Some(Instant::now());
}

fn record_time(
    label: &str,
    log: &Mutex<Vec<Checkpoint>>,
    marker: &Mutex<Option<Instant>>,
) -> u64 {
    let now = Instant::now();
    let mut marker_guard = marker.lock().unwrap();
    let elapsed_ms = match *marker_guard {
        Some(prev) => now.saturating_duration_since(prev).as_millis() as u64,
        None => 0,
    };
    *marker_guard = Some(now);
    drop(marker_guard);
    log.lock().unwrap().push(Checkpoint {
        label: label.to_string(),
        ms: elapsed_ms,
    });
    elapsed_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fresh_log() -> Mutex<Vec<Checkpoint>> {
        Mutex::new(Vec::new())
    }
    fn fresh_marker() -> Mutex<Option<Instant>> {
        Mutex::new(None)
    }

    #[test]
    fn truthy_env_flag_matches_telemetry_rules() {
        assert!(is_truthy_env_flag("1"));
        assert!(is_truthy_env_flag("true"));
        assert!(is_truthy_env_flag("TRUE"));
        assert!(is_truthy_env_flag("yes"));
        assert!(is_truthy_env_flag("YES"));
        assert!(!is_truthy_env_flag(""));
        assert!(!is_truthy_env_flag("0"));
        assert!(!is_truthy_env_flag("no"));
        assert!(!is_truthy_env_flag("false"));
    }

    #[test]
    fn record_inner_logs_label_and_ms() {
        let log = fresh_log();
        let marker = fresh_marker();
        record_reset(&log, &marker);
        let ms = record_time("step", &log, &marker);
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "step");
        // First call after reset can be near-zero; just ensure the recorded
        // ms in the log matches the returned value.
        assert_eq!(entries[0].ms, ms);
        // Sanity ceiling so a stuck clock doesn't produce nonsense.
        assert!(ms < 200, "first checkpoint ms = {} (expected < 200)", ms);
    }

    #[tokio::test]
    async fn record_inner_uses_monotonic_elapsed() {
        let log = fresh_log();
        let marker = fresh_marker();
        record_reset(&log, &marker);
        tokio::time::sleep(Duration::from_millis(15)).await;
        let first = record_time("a", &log, &marker);
        tokio::time::sleep(Duration::from_millis(15)).await;
        let second = record_time("b", &log, &marker);

        assert!(
            (8..200).contains(&first),
            "first elapsed = {} (expected 8..200)",
            first
        );
        assert!(
            (8..200).contains(&second),
            "second elapsed = {} (expected 8..200)",
            second
        );

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "a");
        assert_eq!(entries[1].label, "b");
    }

    #[test]
    fn reset_clears_log() {
        let log = fresh_log();
        let marker = fresh_marker();
        record_reset(&log, &marker);
        record_time("a", &log, &marker);
        record_time("b", &log, &marker);
        assert_eq!(log.lock().unwrap().len(), 2);
        record_reset(&log, &marker);
        assert!(log.lock().unwrap().is_empty());
        assert!(marker.lock().unwrap().is_some());
    }

    #[test]
    fn env_off_makes_time_a_noop() {
        // SAFETY: env-var mutation is not thread-safe in general. Tests
        // that touch HAND_TIMING are kept in serial-friendly form by
        // unsetting both before and after, and by not asserting against
        // the global log (which other tests don't touch when the gate is
        // off, because `time` short-circuits).
        // SAFETY: single-threaded test access; remove_var is only unsafe
        // because libc setenv/unsetenv are not thread-safe.
        unsafe {
            std::env::remove_var("HAND_TIMING");
        }
        assert!(!enabled());
        // The public `time` / `reset` should short-circuit and not
        // populate the global log.
        let before = timings_log().lock().unwrap().len();
        time("noop");
        let after = timings_log().lock().unwrap().len();
        assert_eq!(before, after);
    }

    #[test]
    fn print_with_env_off_does_not_panic() {
        // SAFETY: see env_off_makes_time_a_noop.
        unsafe {
            std::env::remove_var("HAND_TIMING");
        }
        // Should be a silent no-op even if other tests have populated the log.
        print();
    }
}
