//! Reusable countdown state for dialog components.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/countdown-timer.ts`.
//!
//! pi-mono drives the timer with `setInterval(..., 1000)` and triggers a TUI
//! re-render on each tick. The Rust port keeps the same observable behaviour
//! but inverts ownership: the caller invokes [`CountdownTimer::tick`] from
//! whatever cadence its driver provides (frame loop, tokio interval, manual
//! test stepping). This avoids leaking a runtime dependency into a UI helper
//! and stays compatible with any future driver the interactive mode adopts.
//!
//! Callers receive expiry/tick notifications via two callbacks. Both are
//! `Box<dyn FnMut>` so callers may capture mutable state. Following pi-mono,
//! the initial remaining-seconds count is reported synchronously from
//! [`CountdownTimer::new`] before the first tick fires.

use std::time::Duration;

/// Default tick cadence — one second, mirroring pi-mono's `setInterval(_, 1000)`.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Callback fired with the remaining whole seconds (after each tick *and*
/// once during construction with the initial count).
type TickCallback = Box<dyn FnMut(i64) + Send>;
/// Callback fired exactly once when the timer expires.
type ExpireCallback = Box<dyn FnOnce() + Send>;

/// Countdown state machine.
///
/// Construct with [`CountdownTimer::new`]; advance with [`Self::tick`] from a
/// driver loop. The timer fires `on_tick` after every tick (and once eagerly
/// in the constructor) and fires `on_expire` once when the remaining count
/// reaches zero or below. Subsequent ticks after expiry are no-ops.
pub struct CountdownTimer {
    remaining_seconds: i64,
    on_tick: Option<TickCallback>,
    on_expire: Option<ExpireCallback>,
    expired: bool,
}

impl CountdownTimer {
    /// Construct a timer counting down from `timeout`. The remaining-seconds
    /// count is computed as `ceil(timeout_ms / 1000)` to match pi-mono.
    /// `on_tick` is invoked immediately with that initial count.
    pub fn new(
        timeout: Duration,
        on_tick: impl FnMut(i64) + Send + 'static,
        on_expire: impl FnOnce() + Send + 'static,
    ) -> Self {
        let mut on_tick: TickCallback = Box::new(on_tick);
        let total_ms = timeout.as_millis();
        // Ceil division to match the TS `Math.ceil(timeoutMs / 1000)` form.
        let remaining = total_ms.div_ceil(1000) as i64;

        on_tick(remaining);

        Self {
            remaining_seconds: remaining,
            on_tick: Some(on_tick),
            on_expire: Some(Box::new(on_expire)),
            expired: false,
        }
    }

    /// Advance the timer by one second. Calls `on_tick` with the new count
    /// then triggers `on_expire` exactly once when the count reaches zero or
    /// below. After expiry, further calls are no-ops.
    pub fn tick(&mut self) {
        if self.expired {
            return;
        }
        self.remaining_seconds -= 1;
        if let Some(cb) = self.on_tick.as_mut() {
            cb(self.remaining_seconds);
        }
        if self.remaining_seconds <= 0 {
            self.expired = true;
            if let Some(cb) = self.on_expire.take() {
                cb();
            }
            // Drop the tick callback too — no further ticks should fire.
            self.on_tick = None;
        }
    }

    /// Stop the timer without firing `on_expire`. Equivalent to pi-mono's
    /// `dispose()` while the timer is still running.
    pub fn dispose(&mut self) {
        self.expired = true;
        self.on_tick = None;
        self.on_expire = None;
    }

    /// Remaining whole seconds (may be zero or negative once expired).
    pub fn remaining_seconds(&self) -> i64 {
        self.remaining_seconds
    }

    /// Whether the timer has fired its expiry callback (or been disposed).
    pub fn is_expired(&self) -> bool {
        self.expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn fires_initial_tick_with_ceiling_seconds() {
        let log = Arc::new(Mutex::new(Vec::<i64>::new()));
        let log_clone = Arc::clone(&log);
        let _timer = CountdownTimer::new(
            Duration::from_millis(2500),
            move |s| log_clone.lock().unwrap().push(s),
            || {},
        );
        // 2500ms ⇒ ceil(2.5) = 3 seconds.
        assert_eq!(*log.lock().unwrap(), vec![3]);
    }

    #[test]
    fn ticks_decrement_and_expire_at_zero() {
        let ticks = Arc::new(Mutex::new(Vec::<i64>::new()));
        let expired = Arc::new(Mutex::new(false));
        let ticks_clone = Arc::clone(&ticks);
        let expired_clone = Arc::clone(&expired);

        let mut t = CountdownTimer::new(
            Duration::from_secs(2),
            move |s| ticks_clone.lock().unwrap().push(s),
            move || *expired_clone.lock().unwrap() = true,
        );

        // Initial tick recorded 2.
        t.tick(); // 1
        assert!(!*expired.lock().unwrap());
        t.tick(); // 0 → expire
        assert!(*expired.lock().unwrap());
        // Further ticks are no-ops.
        t.tick();
        assert_eq!(*ticks.lock().unwrap(), vec![2, 1, 0]);
    }

    #[test]
    fn dispose_suppresses_expire_callback() {
        let expired = Arc::new(Mutex::new(false));
        let expired_clone = Arc::clone(&expired);

        let mut t = CountdownTimer::new(
            Duration::from_secs(1),
            |_| {},
            move || *expired_clone.lock().unwrap() = true,
        );
        t.dispose();
        t.tick();
        assert!(!*expired.lock().unwrap());
        assert!(t.is_expired());
    }

    #[test]
    fn zero_timeout_expires_on_first_tick() {
        let expired = Arc::new(Mutex::new(false));
        let expired_clone = Arc::clone(&expired);
        let mut t = CountdownTimer::new(
            Duration::from_millis(0),
            |_| {},
            move || *expired_clone.lock().unwrap() = true,
        );
        // Initial remaining is 0, but expire fires only on tick (matching TS,
        // where the expiry check happens *after* the decrement).
        assert!(!*expired.lock().unwrap());
        t.tick();
        assert!(*expired.lock().unwrap());
    }

    #[test]
    fn remaining_seconds_reflects_state() {
        let mut t = CountdownTimer::new(Duration::from_secs(3), |_| {}, || {});
        assert_eq!(t.remaining_seconds(), 3);
        t.tick();
        assert_eq!(t.remaining_seconds(), 2);
    }
}
