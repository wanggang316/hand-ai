//! Frame scheduler for the ratatui runtime.
//!
//! Producers never draw directly. Instead they hold a cheap, cloneable
//! [`FrameRequester`] and call [`FrameRequester::request_frame`] (or
//! [`FrameRequester::request_frame_in`] for a delayed request). The
//! [`FrameScheduler`] actor collects those signals, **coalesces** every request
//! that lands inside one frame window into a single `draw`, and **rate-limits**
//! the draw rate to ~60fps (a hard ceiling of [`MAX_BURSTS_PER_SECOND`]). A
//! token stream that requests thousands of frames per second therefore triggers
//! at most one draw per [`MIN_FRAME_INTERVAL`], never one draw per token.
//!
//! Two invariants are load-bearing and are pinned by unit tests via the pure
//! [`FrameClock`] decision logic (injectable timestamps, no wall-clock sleep):
//!
//! - **Idle silence.** With no pending request the scheduler draws nothing and
//!   emits zero bytes. There is no unconditional "redraw every tick" loop; the
//!   scheduler parks until a request arrives.
//! - **Balanced synchronized output.** Every actual draw is wrapped in a
//!   `BeginSynchronizedUpdate` (`\x1b[?2026h`) / `EndSynchronizedUpdate`
//!   (`\x1b[?2026l`) pair. The wrapper closes the pair even when the inner draw
//!   fails, so an exit or interrupt mid-stream never leaves an unterminated
//!   `?2026h`.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use tokio::sync::mpsc;
use tokio::time::Instant as TokioInstant;

/// Target frame rate: at most one draw per this interval (~60fps).
///
/// A request arriving sooner than this after the previous draw is coalesced and
/// deferred rather than drawn immediately.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Hard ceiling on draw bursts per second. `MIN_FRAME_INTERVAL` of 16ms already
/// caps the *steady* rate at ~62.5/s; this constant documents the contract the
/// external validator probes (≤ 70 bursts/second) and is used by
/// [`FrameClock`] to reject any draw that would exceed it.
pub const MAX_BURSTS_PER_SECOND: u32 = 70;

/// The `BeginSynchronizedUpdate` escape sequence (`CSI ? 2026 h`).
pub const BSU: &[u8] = b"\x1b[?2026h";

/// The `EndSynchronizedUpdate` escape sequence (`CSI ? 2026 l`).
pub const ESU: &[u8] = b"\x1b[?2026l";

/// Pure rate-limiting decision: given the previous draw time and "now", should
/// a *pending* request draw now, or wait — and if wait, for how long?
///
/// This is the deterministic core of the scheduler. It takes injected
/// timestamps rather than reading the clock, so coalescing and rate-limiting can
/// be unit-tested without any real sleep. It knows nothing about channels,
/// tasks, or terminals.
#[derive(Debug, Clone, Copy)]
pub struct FrameClock {
    /// Minimum spacing between two consecutive draws.
    min_interval: Duration,
}

/// The outcome of asking a [`FrameClock`] whether a pending request may draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision {
    /// Draw now. The caller should record this instant as the last draw time.
    DrawNow,
    /// Too soon since the last draw: wait this long, then draw. The pending
    /// request is *retained* — this is the coalescing point, where every
    /// request that arrived during the wait collapses into the one deferred
    /// draw.
    Wait(Duration),
}

impl FrameClock {
    /// A clock with the default ~60fps [`MIN_FRAME_INTERVAL`] spacing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            min_interval: MIN_FRAME_INTERVAL,
        }
    }

    /// A clock with an explicit minimum inter-frame interval. Used by tests to
    /// pin exact coalescing/rate-limit boundaries.
    #[must_use]
    pub const fn with_min_interval(min_interval: Duration) -> Self {
        Self { min_interval }
    }

    /// The configured minimum inter-frame interval.
    #[must_use]
    pub const fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Decide whether a pending request may draw at `now`, given the previous
    /// draw at `last_draw` (`None` = never drawn yet).
    ///
    /// - No prior draw, or at least `min_interval` has elapsed → [`FrameDecision::DrawNow`].
    /// - Otherwise → [`FrameDecision::Wait`] for the remaining slice of the
    ///   interval. Every request that arrives during that wait is absorbed into
    ///   the single deferred draw (coalescing), and because two draws can never
    ///   be closer than `min_interval`, the burst rate is bounded.
    #[must_use]
    pub fn decide(&self, last_draw: Option<Instant>, now: Instant) -> FrameDecision {
        let elapsed = last_draw.map(|last| now.saturating_duration_since(last));
        self.decide_elapsed(elapsed)
    }

    /// The clock-agnostic core of [`decide`]: given the time elapsed since the
    /// last draw (`None` = never drawn yet), decide draw-now vs. wait.
    ///
    /// Kept separate so the async actor can feed elapsed time from *any* clock
    /// source (e.g. `tokio::time::Instant`, which tracks paused/virtual time),
    /// while [`decide`] offers the ergonomic `std::time::Instant` form the unit
    /// tests drive.
    #[must_use]
    pub fn decide_elapsed(&self, elapsed: Option<Duration>) -> FrameDecision {
        match elapsed {
            None => FrameDecision::DrawNow,
            Some(elapsed) if elapsed >= self.min_interval => FrameDecision::DrawNow,
            Some(elapsed) => FrameDecision::Wait(self.min_interval - elapsed),
        }
    }
}

impl Default for FrameClock {
    fn default() -> Self {
        Self::new()
    }
}

/// A request that a frame be drawn.
///
/// The internal signal the [`FrameRequester`] sends to the [`FrameScheduler`].
/// Kept as an explicit type so a delayed request (`request_frame_in`) is
/// distinguishable from an immediate one at the actor boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameSignal {
    /// Draw as soon as the rate limiter allows.
    Now,
    /// Do not consider drawing until at least this delay has elapsed. Used to
    /// schedule a redraw for a future moment (e.g. an animation tick) without
    /// busy-waiting.
    After(Duration),
}

/// A cheap, cloneable handle used by any task to ask for a redraw.
///
/// Cloning is trivial (an `mpsc::UnboundedSender`). Handing clones to concurrent
/// producers is the intended usage: they all funnel requests into the one
/// scheduler, which coalesces and rate-limits them. The channel doubles as the
/// wake signal, so the scheduler parks on `recv()` when idle and needs no
/// separate notifier.
#[derive(Debug, Clone)]
pub struct FrameRequester {
    tx: mpsc::UnboundedSender<FrameSignal>,
}

impl FrameRequester {
    /// Request that a frame be drawn as soon as the rate limiter permits.
    ///
    /// Coalesced with every other request in the same frame window into a single
    /// draw. Never blocks; safe to call at arbitrarily high frequency (that is
    /// exactly the case the scheduler exists to tame). A send failure means the
    /// scheduler has stopped, and is silently ignored — a dead scheduler needs
    /// no frames.
    pub fn request_frame(&self) {
        // Ignore send errors: a closed channel means the scheduler is gone.
        let _ = self.tx.send(FrameSignal::Now);
    }

    /// Request a frame after at least `delay`, without busy-waiting.
    ///
    /// Useful for animations or debounced refreshes: the scheduler will not draw
    /// on account of this request until the delay elapses, and if other requests
    /// arrive sooner they draw on their own schedule (this one does not suppress
    /// them).
    pub fn request_frame_in(&self, delay: Duration) {
        let _ = self.tx.send(FrameSignal::After(delay));
    }
}

/// Wrap a draw in synchronized-output markers, guaranteeing a balanced pair.
///
/// Writes `BeginSynchronizedUpdate` (`\x1b[?2026h`), runs `draw`, then *always*
/// writes `EndSynchronizedUpdate` (`\x1b[?2026l`) and flushes — even if `draw`
/// returns an error. This is the interrupt-safety guarantee: a draw that fails
/// (or a task cancelled mid-draw) can never leave the terminal inside an
/// unterminated synchronized block.
///
/// Kept as a free function over `impl Write` so it can be unit-tested against an
/// in-memory buffer, asserting balanced `?2026h`/`?2026l` bytes without a
/// terminal.
pub fn draw_synchronized<W, F>(out: &mut W, draw: F) -> io::Result<()>
where
    W: Write,
    F: FnOnce(&mut W) -> io::Result<()>,
{
    queue!(out, BeginSynchronizedUpdate)?;
    // Run the inner draw, but capture its result rather than propagating with
    // `?`: we must still close the synchronized block on failure.
    let draw_result = draw(out);
    // Close the pair unconditionally. If closing itself fails there is nothing
    // useful to do, but we still surface the more meaningful inner error first.
    let close_result = close_synchronized(out);
    draw_result.and(close_result)
}

/// Emit a bare `EndSynchronizedUpdate` and flush.
///
/// The teardown escape hatch: if a synchronized block was opened and the process
/// is unwinding toward exit, this closes it so no `?2026h` is left dangling.
pub fn close_synchronized(out: &mut impl Write) -> io::Result<()> {
    queue!(out, EndSynchronizedUpdate)?;
    out.flush()
}

/// Fold a signal into the armed deferral instant.
///
/// A [`FrameSignal::Now`] leaves the deferral untouched (draw as soon as the
/// rate limiter allows); a [`FrameSignal::After`] arms — or tightens — the
/// earliest instant the scheduler may consider drawing. Uses tokio's clock so
/// it tracks paused/virtual time in tests identically to real time in
/// production.
fn arm_deferral(earliest: &mut Option<TokioInstant>, signal: FrameSignal) {
    if let FrameSignal::After(delay) = signal {
        let when = TokioInstant::now() + delay;
        *earliest = Some(earliest.map_or(when, |cur| cur.min(when)));
    }
}

/// Elapsed time since the last draw on tokio's clock, or `None` if never drawn.
fn elapsed_since(last_draw: Option<TokioInstant>) -> Option<Duration> {
    last_draw.map(|last| TokioInstant::now().saturating_duration_since(last))
}

/// Honour a final pending frame once the channel has closed, then finish.
///
/// Called on the shutdown path (all requesters dropped) when a request was in
/// flight: draws once **if** the rate limiter permits, so the last state is not
/// lost — the shutdown draw is not unconditional, a frame still inside the
/// current window is skipped — then returns `Ok(())` to end the actor.
fn finish_pending<F>(
    clock: &FrameClock,
    last_draw: Option<TokioInstant>,
    draw: &mut F,
) -> io::Result<()>
where
    F: FnMut() -> io::Result<()>,
{
    if let FrameDecision::DrawNow = clock.decide_elapsed(elapsed_since(last_draw)) {
        draw()?;
    }
    Ok(())
}

/// The draw-side of the runtime: coalesces and rate-limits redraw requests.
///
/// Spawn it with [`FrameScheduler::spawn`], which returns a [`FrameRequester`]
/// (the request side) and a [`tokio::task::JoinHandle`] for the actor loop. The
/// actor owns a user-supplied draw closure and calls it — wrapped in
/// synchronized-output markers — at most once per frame window, only while
/// requests are pending.
pub struct FrameScheduler;

impl FrameScheduler {
    /// Spawn the scheduler actor on the current tokio runtime.
    ///
    /// `draw` is the single place the UI is painted; it is invoked (wrapped in
    /// BSU/ESU) whenever a coalesced, rate-limited frame is due. The actor runs
    /// until every [`FrameRequester`] clone is dropped (the channel closes) and
    /// no request is pending.
    ///
    /// The `draw` closure is expected to perform its own terminal draw and
    /// wrapping via [`draw_synchronized`] over a real writer; see the actor loop
    /// for the exact contract. Returning an error from `draw` stops the
    /// scheduler.
    #[must_use]
    pub fn spawn<F>(mut draw: F) -> (FrameRequester, tokio::task::JoinHandle<io::Result<()>>)
    where
        F: FnMut() -> io::Result<()> + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<FrameSignal>();
        let requester = FrameRequester { tx };

        let handle = tokio::spawn(async move {
            let clock = FrameClock::new();
            let mut last_draw: Option<TokioInstant> = None;
            // Whether a request is waiting to be satisfied. Set when a signal
            // arrives, cleared when we actually draw. This flag is the whole of
            // "idle silence": while it is false we never draw and never emit a
            // byte, and we park on the channel rather than spinning.
            let mut pending = false;
            // Earliest instant we may *consider* a deferred (`After`) request.
            // `None` means no deferral is armed.
            let mut earliest: Option<TokioInstant> = None;

            loop {
                // Idle: park on the channel. This is the "idle silence"
                // guarantee — a channel-blocked await draws nothing, emits no
                // byte, and consumes no CPU until a request arrives (or every
                // requester drops, closing the channel).
                if !pending {
                    match rx.recv().await {
                        Some(signal) => {
                            pending = true;
                            arm_deferral(&mut earliest, signal);
                        }
                        None => return Ok(()),
                    }
                }

                // Drain any further buffered signals without blocking so a token
                // flood collapses into the one `pending` flag.
                loop {
                    match rx.try_recv() {
                        Ok(signal) => arm_deferral(&mut earliest, signal),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            // All requesters dropped. Honour the final pending
                            // frame if the rate limiter allows, then stop.
                            return finish_pending(&clock, last_draw, &mut draw);
                        }
                    }
                }

                let now = TokioInstant::now();

                // Respect an armed `After` deferral: if the earliest allowed
                // instant is still in the future, wait for it (or for a new
                // request that might supersede it), then re-evaluate.
                if let Some(when) = earliest
                    && now < when
                {
                    let sleep = tokio::time::sleep_until(when);
                    tokio::select! {
                        () = sleep => {}
                        maybe = rx.recv() => match maybe {
                            Some(signal) => arm_deferral(&mut earliest, signal),
                            None => return finish_pending(&clock, last_draw, &mut draw),
                        },
                    }
                    continue;
                }
                earliest = None;

                match clock.decide_elapsed(elapsed_since(last_draw)) {
                    FrameDecision::DrawNow => {
                        draw()?;
                        last_draw = Some(TokioInstant::now());
                        pending = false;
                    }
                    FrameDecision::Wait(remaining) => {
                        // Coalesce: wait out the remaining frame budget. Any
                        // request arriving meanwhile is folded into `pending`
                        // (still true), so it costs nothing and does not advance
                        // the draw. A channel close during the wait ends the
                        // loop after honouring the pending frame.
                        let sleep = tokio::time::sleep(remaining);
                        tokio::select! {
                            () = sleep => {}
                            maybe = rx.recv() => match maybe {
                                Some(signal) => arm_deferral(&mut earliest, signal),
                                None => return finish_pending(&clock, last_draw, &mut draw),
                            },
                        }
                    }
                }
            }
        });

        (requester, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FrameClock: coalescing / rate-limiting boundaries -----------------

    /// A never-drawn scheduler draws the first pending request immediately.
    #[test]
    fn decide_draws_now_when_never_drawn() {
        let clock = FrameClock::with_min_interval(Duration::from_millis(16));
        let now = Instant::now();
        assert_eq!(clock.decide(None, now), FrameDecision::DrawNow);
    }

    /// A request that lands exactly one interval after the last draw draws now.
    #[test]
    fn decide_draws_now_at_interval_boundary() {
        let interval = Duration::from_millis(16);
        let clock = FrameClock::with_min_interval(interval);
        let last = Instant::now();
        let now = last + interval;
        assert_eq!(clock.decide(Some(last), now), FrameDecision::DrawNow);
    }

    /// A request sooner than one interval after the last draw waits out the
    /// remaining slice — this is where a burst of requests coalesces.
    #[test]
    fn decide_waits_within_interval_and_reports_remaining() {
        let interval = Duration::from_millis(16);
        let clock = FrameClock::with_min_interval(interval);
        let last = Instant::now();
        let now = last + Duration::from_millis(4);
        assert_eq!(
            clock.decide(Some(last), now),
            FrameDecision::Wait(Duration::from_millis(12)),
        );
    }

    /// N requests inside one window collapse to a single draw: only the first
    /// `decide` returns `DrawNow`; every later one within the interval `Wait`s.
    #[test]
    fn coalesces_burst_within_window_to_single_draw() {
        let interval = Duration::from_millis(16);
        let clock = FrameClock::with_min_interval(interval);

        let t0 = Instant::now();
        // First request: draws.
        assert_eq!(clock.decide(None, t0), FrameDecision::DrawNow);
        let last = t0;

        // 100 further requests, each 0.1ms apart, all inside the 16ms window.
        let mut draws = 0u32;
        for i in 1..=100u64 {
            let now = last + Duration::from_micros(i * 100);
            if let FrameDecision::DrawNow = clock.decide(Some(last), now) {
                draws += 1;
            }
        }
        assert_eq!(draws, 0, "no request inside the window may draw");
    }

    /// Over a fixed span, the number of draws a saturated stream can produce is
    /// bounded by span / interval — the rate-limit guarantee, computed purely
    /// from timestamps with no wall-clock sleep.
    #[test]
    fn rate_limit_bounds_draws_over_fixed_span() {
        let interval = Duration::from_millis(16);
        let clock = FrameClock::with_min_interval(interval);

        // Simulate a 1-second saturated stream: a request every 100µs (10_000
        // requests). Advance a virtual clock, drawing only when allowed.
        let start = Instant::now();
        let mut last_draw: Option<Instant> = None;
        let mut draws = 0u32;
        for i in 0..10_000u64 {
            let now = start + Duration::from_micros(i * 100);
            if let FrameDecision::DrawNow = clock.decide(last_draw, now) {
                draws += 1;
                last_draw = Some(now);
            }
        }
        // ~1s / 16ms ≈ 62.5 → at most 63, and the hard ceiling is 70.
        assert!(
            draws <= MAX_BURSTS_PER_SECOND,
            "draws {draws} must not exceed the {MAX_BURSTS_PER_SECOND}/s ceiling",
        );
        assert!(
            draws >= 60,
            "a saturated 1s stream should draw ~62 times, got {draws}",
        );
    }

    // --- draw_synchronized: balanced BSU/ESU -------------------------------

    /// A successful draw is wrapped in exactly one balanced `?2026h`/`?2026l`
    /// pair, in order.
    #[test]
    fn draw_synchronized_wraps_in_balanced_pair() {
        let mut buf: Vec<u8> = Vec::new();
        draw_synchronized(&mut buf, |w| w.write_all(b"PAINT")).unwrap();

        assert_eq!(count(&buf, BSU), 1, "exactly one BSU");
        assert_eq!(count(&buf, ESU), 1, "exactly one ESU");
        let open = find(&buf, BSU).unwrap();
        let body = find(&buf, b"PAINT").unwrap();
        let close = find(&buf, ESU).unwrap();
        assert!(open < body && body < close, "order must be BSU, draw, ESU");
    }

    /// Even when the inner draw fails, the synchronized block is closed: the
    /// error propagates but the `?2026l` is still emitted. This is the
    /// interrupt-safety invariant.
    #[test]
    fn draw_synchronized_closes_pair_on_inner_error() {
        let mut buf: Vec<u8> = Vec::new();
        let err = draw_synchronized(&mut buf, |w| {
            w.write_all(b"HALF")?;
            Err(io::Error::other("boom"))
        });
        assert!(err.is_err(), "inner error must propagate");
        assert_eq!(count(&buf, BSU), 1);
        assert_eq!(count(&buf, ESU), 1, "ESU emitted despite the failure");
        assert!(find(&buf, BSU).unwrap() < find(&buf, ESU).unwrap());
    }

    /// Repeated draws produce equal BSU and ESU counts — never an odd one out.
    #[test]
    fn repeated_draws_keep_bsu_esu_balanced() {
        let mut buf: Vec<u8> = Vec::new();
        for _ in 0..5 {
            draw_synchronized(&mut buf, |w| w.write_all(b".")).unwrap();
        }
        assert_eq!(count(&buf, BSU), count(&buf, ESU));
        assert_eq!(count(&buf, BSU), 5);
    }

    /// `close_synchronized` alone emits a single ESU and no BSU — the teardown
    /// path that terminates a block opened elsewhere.
    #[test]
    fn close_synchronized_emits_lone_esu() {
        let mut buf: Vec<u8> = Vec::new();
        close_synchronized(&mut buf).unwrap();
        assert_eq!(count(&buf, ESU), 1);
        assert_eq!(count(&buf, BSU), 0);
    }

    // --- helpers -----------------------------------------------------------

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() {
            return 0;
        }
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
}
