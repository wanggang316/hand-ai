//! Integration tests for the rt frame scheduler (`hand_tui::rt::scheduler`).
//!
//! The pure coalescing/rate-limit boundaries live in the module's own unit
//! tests (driven by injected timestamps, no clock). These tests exercise the
//! *live* [`FrameScheduler`] actor and the synchronized-output wrapper end to
//! end, pinning the feature's testable assertions:
//!
//! - **Coalescing (VAL-CORE-004).** A burst of `request_frame()` calls inside
//!   one frame window produces a single `draw` callback, never one per request.
//! - **Rate limit (VAL-CORE-004).** A saturated request stream over a fixed
//!   span yields a bounded number of draws (≤ the ceiling), far fewer than the
//!   request count.
//! - **Idle silence (VAL-CORE-032).** With no request the actor draws zero
//!   times and emits zero bytes.
//! - **Balanced synchronized output (VAL-CORE-003, VAL-CORE-017).** Every draw
//!   the actor performs through `draw_synchronized` is wrapped in a balanced
//!   `?2026h`/`?2026l` pair, including the final flush when requesters drop.
//!
//! Timing is kept deterministic by pausing tokio's clock (`start_paused`) so the
//! scheduler's `sleep`/`sleep_until` auto-advance without real wall-clock waits;
//! coalescing is asserted on callback *counts*, not durations.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hand_tui::rt::scheduler::{
    BSU, ESU, FrameScheduler, MAX_BURSTS_PER_SECOND, MIN_FRAME_INTERVAL, draw_synchronized,
};

// --- helpers ---------------------------------------------------------------

/// A shared draw counter plus the byte sink each draw writes through
/// `draw_synchronized`, so tests can inspect both the callback count and the
/// exact emitted bytes.
#[derive(Clone, Default)]
struct Recorder {
    draws: Arc<AtomicUsize>,
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl Recorder {
    fn new() -> Self {
        Self::default()
    }

    fn draw_count(&self) -> usize {
        self.draws.load(Ordering::SeqCst)
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }

    /// Build a draw closure suitable for `FrameScheduler::spawn`: it increments
    /// the counter and writes one balanced synchronized block into the sink.
    fn draw_fn(&self) -> impl FnMut() -> std::io::Result<()> + Send + 'static {
        let draws = self.draws.clone();
        let bytes = self.bytes.clone();
        move || {
            let mut guard = bytes.lock().unwrap();
            draw_synchronized(&mut *guard, |w| {
                use std::io::Write;
                w.write_all(b"PAINT")
            })?;
            draws.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack.windows(needle.len()).filter(|w| *w == needle).count()
}

// =============================================================================
// Coalescing — a burst inside one window is a single draw (VAL-CORE-004)
// =============================================================================

#[tokio::test(start_paused = true)]
async fn burst_of_requests_coalesces_to_few_draws() {
    let rec = Recorder::new();
    let (requester, handle) = FrameScheduler::spawn(rec.draw_fn());

    // Fire a flood of requests with no awaits between them: they all land before
    // the scheduler can advance its (paused) clock past one frame window.
    for _ in 0..1_000 {
        requester.request_frame();
    }

    // Let the actor run: the first request draws immediately; the rest are
    // pending. Yield so the actor drains the channel and performs that draw.
    tokio::task::yield_now().await;
    // Advance well past a single frame interval to let at most one deferred
    // draw flush.
    tokio::time::sleep(MIN_FRAME_INTERVAL * 2).await;

    // Drop the requester so the actor finishes and honours any final frame.
    drop(requester);
    handle.await.unwrap().unwrap();

    let draws = rec.draw_count();
    assert!(
        (1..=3).contains(&draws),
        "1000 coalesced requests must yield a handful of draws, got {draws}",
    );
    // The whole point: far fewer draws than requests.
    assert!(draws < 1_000);
}

// =============================================================================
// Rate limit — saturated stream over a span is bounded (VAL-CORE-004)
// =============================================================================

#[tokio::test(start_paused = true)]
async fn saturated_stream_is_rate_limited_over_a_second() {
    let rec = Recorder::new();
    let (requester, handle) = FrameScheduler::spawn(rec.draw_fn());

    // Drive ~1 second of virtual time. Each iteration requests a frame and then
    // advances the paused clock by 1ms, so the scheduler sees a continuous
    // stream but can only draw once per MIN_FRAME_INTERVAL.
    for _ in 0..1_000 {
        requester.request_frame();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
    }

    drop(requester);
    handle.await.unwrap().unwrap();

    let draws = rec.draw_count();
    assert!(
        draws as u32 <= MAX_BURSTS_PER_SECOND,
        "draws {draws} must stay within the {MAX_BURSTS_PER_SECOND}/s ceiling",
    );
    // A 1s stream at 16ms spacing should still draw a healthy number of frames,
    // not stall — but nowhere near the 1000 requests.
    assert!(draws >= 30, "expected a steady stream of draws, got {draws}");
    assert!(draws < 1_000);
}

// =============================================================================
// Idle silence — no request, no draw, no bytes (VAL-CORE-032)
// =============================================================================

#[tokio::test(start_paused = true)]
async fn idle_scheduler_draws_nothing_and_emits_no_bytes() {
    let rec = Recorder::new();
    let (requester, handle) = FrameScheduler::spawn(rec.draw_fn());

    // Never request a frame. Advance a generous virtual window.
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;

    assert_eq!(rec.draw_count(), 0, "idle scheduler must not draw");
    assert!(rec.bytes().is_empty(), "idle scheduler must emit zero bytes");

    // Cleanly stop.
    drop(requester);
    handle.await.unwrap().unwrap();

    // Still nothing after shutdown with no pending frame.
    assert_eq!(rec.draw_count(), 0);
    assert!(rec.bytes().is_empty());
}

// =============================================================================
// Balanced synchronized output across many draws (VAL-CORE-003 / -017)
// =============================================================================

#[tokio::test(start_paused = true)]
async fn every_draw_is_wrapped_in_a_balanced_synchronized_pair() {
    let rec = Recorder::new();
    let (requester, handle) = FrameScheduler::spawn(rec.draw_fn());

    // A spaced stream so several distinct draws happen.
    for _ in 0..50 {
        requester.request_frame();
        tokio::task::yield_now().await;
        tokio::time::advance(MIN_FRAME_INTERVAL).await;
    }

    drop(requester);
    handle.await.unwrap().unwrap();

    let bytes = rec.bytes();
    let opens = count(&bytes, BSU);
    let closes = count(&bytes, ESU);
    assert!(opens > 0, "expected at least one draw");
    assert_eq!(opens, closes, "BSU and ESU counts must be equal (balanced)");
    assert_eq!(
        opens,
        rec.draw_count(),
        "one balanced pair per draw callback",
    );

    // The stream must never end inside an open block: the last marker is a
    // close, and every prefix has closes ≤ opens.
    assert!(ends_balanced(&bytes), "no unterminated ?2026h at the tail");
}

/// True iff, scanning left to right, `?2026l` never precedes an unmatched
/// `?2026h` and the sequence ends with all opens closed.
fn ends_balanced(bytes: &[u8]) -> bool {
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(BSU) {
            depth += 1;
            i += BSU.len();
        } else if bytes[i..].starts_with(ESU) {
            depth -= 1;
            if depth < 0 {
                return false;
            }
            i += ESU.len();
        } else {
            i += 1;
        }
    }
    depth == 0
}

// =============================================================================
// Shutdown honours a final pending frame, still balanced (VAL-CORE-017)
// =============================================================================

#[tokio::test(start_paused = true)]
async fn dropping_requester_flushes_a_pending_frame_balanced() {
    let rec = Recorder::new();
    let (requester, handle) = FrameScheduler::spawn(rec.draw_fn());

    // One request, then immediately drop: the actor should draw once (the first
    // request always draws) and shut down with a balanced block.
    requester.request_frame();
    drop(requester);
    handle.await.unwrap().unwrap();

    assert_eq!(rec.draw_count(), 1);
    let bytes = rec.bytes();
    assert_eq!(count(&bytes, BSU), 1);
    assert_eq!(count(&bytes, ESU), 1);
    assert!(ends_balanced(&bytes));
}
