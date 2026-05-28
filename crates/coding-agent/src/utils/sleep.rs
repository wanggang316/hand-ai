//! Async sleep helper with optional cancellation support.
//!
//! Mirrors the TS `sleep(ms, signal?)` helper from `upstream coding-agent`. Rust's
//! cancellation idiom is `tokio::select!` over an external future, so this
//! module exposes:
//!
//! - [`sleep`] — a thin wrapper over [`tokio::time::sleep`] for the simple
//!   "wait N milliseconds" case.
//! - [`sleep_cancellable`] — waits for either the timer or a user-supplied
//!   cancellation future. When the cancel future fires first, returns
//!   [`SleepError::Aborted`].
//!
//! Cancellation futures are typically built from
//! `tokio_util::sync::CancellationToken::cancelled()` or any other future
//! that resolves on abort. We accept any `Future<Output = ()>` to avoid
//! pulling extra dependencies into this leaf module.

use std::future::Future;
use std::time::Duration;

use thiserror::Error;

/// Error returned by [`sleep_cancellable`] when the cancellation future
/// resolves before the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SleepError {
    /// Cancellation signal fired before the sleep completed.
    #[error("sleep aborted before completion")]
    Aborted,
}

/// Sleep for `duration`, yielding back to the runtime.
///
/// Equivalent to calling [`tokio::time::sleep`] directly; provided so callers
/// don't need to import `tokio::time` for the common case.
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Sleep for `duration`, returning [`SleepError::Aborted`] if `cancel`
/// resolves first.
///
/// Both arms are polled cooperatively via `tokio::select!`. If the timer
/// completes first the function returns `Ok(())`. If `cancel` completes
/// first the timer is dropped and `Err(SleepError::Aborted)` is returned.
pub async fn sleep_cancellable<F>(duration: Duration, cancel: F) -> Result<(), SleepError>
where
    F: Future<Output = ()>,
{
    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = cancel => Err(SleepError::Aborted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn sleep_completes_after_duration() {
        let start = Instant::now();
        sleep(Duration::from_millis(20)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(15),
            "expected at least ~20ms, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn cancellable_completes_normally_when_cancel_never_fires() {
        let (_tx, rx) = oneshot::channel::<()>();
        // rx will never resolve because tx is held; the timer should win.
        let result = sleep_cancellable(Duration::from_millis(20), async move {
            let _ = rx.await;
        })
        .await;
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn cancellable_returns_aborted_when_cancel_fires_first() {
        let (tx, rx) = oneshot::channel::<()>();
        // Resolve the cancel future immediately on a separate task.
        tokio::spawn(async move {
            let _ = tx.send(());
        });
        let result = sleep_cancellable(Duration::from_secs(60), async move {
            let _ = rx.await;
        })
        .await;
        assert_eq!(result, Err(SleepError::Aborted));
    }

    #[tokio::test]
    async fn cancellable_returns_aborted_when_already_resolved() {
        // A pre-resolved future should win the select on first poll.
        let result = sleep_cancellable(Duration::from_secs(60), std::future::ready(())).await;
        assert_eq!(result, Err(SleepError::Aborted));
    }
}
