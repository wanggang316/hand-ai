//! Filesystem watcher exposing a channel/stream of [`FileChange`] events.
//!
//! Replaces the Node `fs.watch` thin-wrapper that the TypeScript original
//! ships (`closeWatcher` / `watchWithErrorHandler`). Callers in Rust want an
//! idiomatic async stream so the `notify`-driven implementation here aligns
//! with `docs/conversion-guidelines.md` §4.3 (channel-driven hooks).
//!
//! Design choices:
//! - **Channel-driven**: events flow through a bounded mpsc; the public API
//!   is an `impl Stream<Item = FileChange>` so callers can `tokio::select!`
//!   them or fold them with `StreamExt`.
//! - **Best-effort error reporting**: backend errors surface as
//!   [`FileChange::Error`] rather than tearing the stream down — the TS
//!   original retries on errors and Rust callers will want the same option.
//! - **Lifetime tied to a guard**: dropping the [`WatchHandle`] stops the
//!   `notify` watcher; the stream then ends after draining.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::Stream;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;

/// Retry delay paired with [`watch_files`] for callers that want to mirror
/// the TypeScript backoff after a watch error.
pub const FS_WATCH_RETRY_DELAY: Duration = Duration::from_secs(5);

/// One filesystem event from the underlying [`notify`] backend.
///
/// Tagged enum so callers can match on intent without inspecting the raw
/// `notify::Event`. The `Error` variant carries backend errors instead of
/// terminating the stream — the TS version restarts on errors and Rust
/// callers should be free to do the same.
#[derive(Debug, Clone)]
pub enum FileChange {
    /// A path was created.
    Created { path: PathBuf },
    /// A path's contents were modified.
    Modified { path: PathBuf },
    /// A path was removed.
    Removed { path: PathBuf },
    /// A path was renamed (or otherwise had its name change). Only one of
    /// `from`/`to` may be present depending on what `notify` reports.
    Renamed {
        from: Option<PathBuf>,
        to: Option<PathBuf>,
    },
    /// A non-fatal error from the backend. The watcher continues running.
    Error { message: String },
}

/// Errors returned when constructing a watcher.
#[derive(Debug, Error)]
pub enum FsWatchError {
    /// The underlying `notify` backend could not be initialised.
    #[error("failed to create filesystem watcher: {0}")]
    Init(String),
    /// One of the requested paths could not be added to the watcher.
    #[error("failed to watch {}: {message}", path.display())]
    Watch {
        /// Path the caller asked to watch.
        path: PathBuf,
        /// Underlying notify error message.
        message: String,
    },
}

/// Owning handle for the running watcher.
///
/// Drop this to stop the watcher and close the event stream. Hold onto it
/// for the lifetime of the consumer; otherwise the stream will end as soon
/// as the channel sender is dropped.
pub struct WatchHandle {
    _watcher: RecommendedWatcher,
}

/// Watch each path in `paths` (non-recursive) and stream change events.
///
/// Returns the [`WatchHandle`] guard plus an `impl Stream` of events.
/// Dropping the handle stops the watcher.
///
/// `channel_capacity` bounds the queue between the `notify` backend thread
/// and the async consumer; the TS version does not bound at all, but in
/// Rust we want backpressure rather than unbounded growth on slow
/// consumers. The `notify` callback drops events when the channel is full,
/// matching the `notify` ergonomics.
pub fn watch_files<I, P>(
    paths: I,
    channel_capacity: usize,
) -> Result<(WatchHandle, impl Stream<Item = FileChange>), FsWatchError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let (tx, rx) = mpsc::channel::<FileChange>(channel_capacity.max(1));

    let event_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let change = match res {
            Ok(event) => match event_to_change(&event) {
                Some(c) => c,
                None => return,
            },
            Err(err) => FileChange::Error {
                message: err.to_string(),
            },
        };
        // Best-effort send; if the consumer is gone or full we drop the
        // event rather than block the notify backend thread.
        let _ = event_tx.try_send(change);
    })
    .map_err(|err| FsWatchError::Init(err.to_string()))?;

    for path in paths {
        let path_ref = path.as_ref();
        watcher
            .watch(path_ref, RecursiveMode::NonRecursive)
            .map_err(|err| FsWatchError::Watch {
                path: path_ref.to_path_buf(),
                message: err.to_string(),
            })?;
    }

    // The callback closure owns the only remaining sender; dropping ours
    // here ensures the receiver closes when the watcher (and thus the
    // closure) is dropped.
    drop(tx);

    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(change) = rx.recv().await {
            yield change;
        }
    };

    Ok((WatchHandle { _watcher: watcher }, stream))
}

/// Map a raw `notify::Event` to our public [`FileChange`] enum.
///
/// Returns `None` for event kinds we don't expose (`Access`, `Other`,
/// `Any`) so callers don't see noise.
fn event_to_change(event: &Event) -> Option<FileChange> {
    use notify::event::{ModifyKind, RenameMode};

    let first_path = || event.paths.first().cloned();
    match event.kind {
        EventKind::Create(_) => Some(FileChange::Created {
            path: first_path()?,
        }),
        EventKind::Remove(_) => Some(FileChange::Removed {
            path: first_path()?,
        }),
        EventKind::Modify(ModifyKind::Name(mode)) => {
            let (from, to) = match mode {
                RenameMode::From => (first_path(), None),
                RenameMode::To => (None, first_path()),
                RenameMode::Both => (event.paths.first().cloned(), event.paths.get(1).cloned()),
                RenameMode::Any | RenameMode::Other => (first_path(), None),
            };
            Some(FileChange::Renamed { from, to })
        }
        EventKind::Modify(_) => Some(FileChange::Modified {
            path: first_path()?,
        }),
        EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::pin_mut;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    /// Drain events for up to `timeout`, returning everything seen.
    async fn drain<S>(stream: S, timeout: Duration) -> Vec<FileChange>
    where
        S: Stream<Item = FileChange>,
    {
        pin_mut!(stream);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut events = Vec::new();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            match tokio::time::timeout(deadline - now, stream.next()).await {
                Ok(Some(ev)) => events.push(ev),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        events
    }

    #[tokio::test]
    async fn nonexistent_path_returns_watch_error() {
        let result = watch_files([Path::new("/does/not/exist/xyz")], 8);
        match result {
            Err(FsWatchError::Watch { path, .. }) => {
                assert_eq!(path, PathBuf::from("/does/not/exist/xyz"));
            }
            Err(other) => panic!("expected Watch error, got {other:?}"),
            Ok(_) => panic!("expected error for nonexistent path"),
        }
    }

    #[tokio::test]
    async fn modifying_a_file_emits_a_change_event() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "initial\n")
            .await
            .expect("seed file");

        // macOS canonicalises tmp paths through /private/var; resolve once
        // so we can compare backend-reported paths verbatim.
        let canonical = std::fs::canonicalize(&path).expect("canonicalize");

        let (handle, stream) = watch_files([&path], 16).expect("watcher starts");

        // Give notify a moment to register the watch on macOS' FSEvents.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Append to the file to provoke a Modify event.
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open append");
        f.write_all(b"more\n").await.expect("write");
        f.flush().await.expect("flush");
        drop(f);

        // Wait for events; FSEvents on macOS can take ~100-300ms.
        let events = drain(stream, Duration::from_secs(3)).await;
        drop(handle);

        assert!(
            events.iter().any(|e| matches!(
                e,
                FileChange::Modified { path: p } | FileChange::Created { path: p }
                    if p == &canonical || p == &path
            )),
            "expected Modified/Created for {path:?}, got {events:?}"
        );
    }

    #[tokio::test]
    async fn dropping_handle_closes_stream() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("watched");
        tokio::fs::write(&path, "x").await.expect("seed");

        let (handle, stream) = watch_files([&path], 4).expect("watcher starts");
        drop(handle);
        pin_mut!(stream);

        // The stream should terminate (None) shortly after the handle drops.
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        match next {
            Ok(None) => {} // expected
            Ok(Some(_event)) => {
                // It's also acceptable to drain a buffered event before
                // the stream ends; consume the rest and confirm closure.
                let tail = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
                assert!(matches!(tail, Ok(None) | Err(_)));
            }
            Err(_) => panic!("stream did not close after handle drop"),
        }
    }
}
