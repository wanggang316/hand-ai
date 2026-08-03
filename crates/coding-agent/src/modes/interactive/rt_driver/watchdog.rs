//! Turn watchdog: an injectable ceiling on how long a single agent turn may run.
//!
//! The legacy driver hard-coded a 5-minute (`from_secs(300)`) timeout around
//! [`AgentSession::send_message`](crate::core::agent_session::AgentSession::send_message),
//! so a hung HTTP request would pin the loader forever with no way to probe the
//! recovery deterministically. Here the ceiling is a plain value threaded into
//! the driver, defaulting to the same 5 minutes but overridable — a test (and
//! the `stall` mock-provider scenario) injects a short timeout to exercise the
//! timeout banner path (VAL-CHAT-022) without waiting five real minutes.
//!
//! The type is deliberately tiny and free of any terminal / async coupling: it
//! is just the duration plus the banner text a timeout produces, so its policy
//! is unit-tested in isolation and the driver only has to `tokio::time::timeout`
//! against [`Watchdog::turn_timeout`] and, on elapse, push
//! [`Watchdog::timeout_banner`] into scrollback and cancel the turn.

use std::time::Duration;

/// The default per-turn ceiling, matching the legacy hard-coded 5 minutes.
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// Environment variable that overrides the per-turn watchdog ceiling, in
/// milliseconds. This is the probe seam for VAL-CHAT-022: a validator sets it to
/// a small value and drives the `stall` mock-provider scenario so the timeout
/// banner fires in seconds instead of the 5-minute default. Unset / unparseable
/// leaves the default in force.
pub const TURN_TIMEOUT_ENV: &str = "HAND_TURN_TIMEOUT_MS";

/// An injectable per-turn timeout policy.
///
/// Cheap and `Copy`: the driver holds one and consults it around every
/// `send_message`. Construct with [`Watchdog::new`] (explicit ceiling) or
/// [`Watchdog::default`] (the 5-minute default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watchdog {
    turn_timeout: Duration,
}

impl Watchdog {
    /// A watchdog with an explicit per-turn ceiling.
    ///
    /// A zero duration is meaningful — it means "fire immediately" — and is left
    /// as given so a test can force the timeout path on the very next poll.
    #[must_use]
    pub const fn new(turn_timeout: Duration) -> Self {
        Self { turn_timeout }
    }

    /// A watchdog whose ceiling comes from [`TURN_TIMEOUT_ENV`] (milliseconds)
    /// when set to a parseable value, else the [`DEFAULT_TURN_TIMEOUT`].
    ///
    /// This is how the driver injects a short ceiling for the VAL-CHAT-022 probe
    /// without a code change or a five-minute wait: the validator exports
    /// `HAND_TURN_TIMEOUT_MS=<n>` and drives the `stall` scenario. Production
    /// runs leave it unset and get the 5-minute default.
    #[must_use]
    pub fn from_env_or_default() -> Self {
        Self::from_env_value(std::env::var(TURN_TIMEOUT_ENV).ok().as_deref())
    }

    /// The pure core of [`from_env_or_default`]: resolve a watchdog from an
    /// optional raw env value (the milliseconds string), falling back to the
    /// default on `None` or an unparseable value. Split out so the override
    /// policy is unit-tested without mutating process-global env.
    #[must_use]
    pub fn from_env_value(raw: Option<&str>) -> Self {
        match raw.and_then(|v| v.parse::<u64>().ok()) {
            Some(ms) => Self::new(Duration::from_millis(ms)),
            None => Self::default(),
        }
    }

    /// The maximum wall-clock time a single turn may run before the watchdog
    /// cancels it. Handed straight to `tokio::time::timeout`.
    #[must_use]
    pub const fn turn_timeout(&self) -> Duration {
        self.turn_timeout
    }

    /// The banner text pushed into scrollback when a turn exceeds the ceiling.
    ///
    /// Phrased in terms of the configured duration so the injected-short-timeout
    /// test and the real 5-minute default both read sensibly, and so a validator
    /// has a stable, greppable string (`timed out`) to assert on.
    #[must_use]
    pub fn timeout_banner(&self) -> String {
        let secs = self.turn_timeout.as_secs();
        format!("request timed out after {secs}s; cancelled")
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new(DEFAULT_TURN_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_the_legacy_five_minute_ceiling() {
        let watchdog = Watchdog::default();
        assert_eq!(watchdog.turn_timeout(), Duration::from_secs(300));
    }

    #[test]
    fn new_carries_the_injected_ceiling() {
        let watchdog = Watchdog::new(Duration::from_millis(250));
        assert_eq!(watchdog.turn_timeout(), Duration::from_millis(250));
    }

    #[test]
    fn timeout_banner_reports_the_configured_seconds_and_is_greppable() {
        let watchdog = Watchdog::new(Duration::from_secs(2));
        let banner = watchdog.timeout_banner();
        assert!(banner.contains("timed out"), "banner: {banner}");
        assert!(
            banner.contains('2'),
            "banner should name the ceiling: {banner}"
        );
    }

    #[test]
    fn a_zero_ceiling_is_preserved_for_immediate_fire() {
        let watchdog = Watchdog::new(Duration::ZERO);
        assert_eq!(watchdog.turn_timeout(), Duration::ZERO);
    }

    #[test]
    fn env_value_overrides_the_default_when_parseable() {
        let watchdog = Watchdog::from_env_value(Some("1500"));
        assert_eq!(watchdog.turn_timeout(), Duration::from_millis(1500));
    }

    #[test]
    fn env_value_falls_back_to_default_when_absent_or_garbage() {
        assert_eq!(
            Watchdog::from_env_value(None).turn_timeout(),
            DEFAULT_TURN_TIMEOUT
        );
        assert_eq!(
            Watchdog::from_env_value(Some("not-a-number")).turn_timeout(),
            DEFAULT_TURN_TIMEOUT
        );
        assert_eq!(
            Watchdog::from_env_value(Some("")).turn_timeout(),
            DEFAULT_TURN_TIMEOUT
        );
    }
}
