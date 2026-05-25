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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::Stream;
use notify::{Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher};
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
    _watcher: PollWatcher,
}

/// Poll interval for the underlying [`PollWatcher`]. Trades latency
/// for cross-platform reliability — `notify::recommended_watcher`
/// (FSEvents on macOS, inotify on Linux) silently dropped events
/// on tempfs-style filesystems used in tests, and even in production
/// macOS FSEvents required watching the parent directory and filtering
/// per-file. Polling sidesteps all of that for the small cost of a
/// ~250 ms latency floor — fine for settings reload, doc tracking,
/// HAND.md change detection, and the other places this module is used.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

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

    // Collect the requested paths up-front. We watch the *parent
    // directory* (NonRecursive) for each file and filter events back
    // down to the requested set inside the callback. macOS FSEvents
    // only fires reliably on directory contents — registering a watch
    // on the file inode itself silently misses modify events when the
    // file is rewritten (notably any atomic-write tool that does
    // `write tmp + rename`). The previous direct-file form passed
    // local cargo runs against Linux/inotify, broke under FSEvents,
    // and surfaced as 6 hung settings-watcher tests on macOS hosts.
    //
    // For each requested file we also canonicalise so the per-event
    // path comparison matches `/private/var/...` (FSEvents-reported)
    // against `/var/...` (caller-supplied) on macOS. We keep both the
    // canonicalised and original form in the targets set so callers
    // that pass a symlink chain still match.
    let mut targets: HashSet<PathBuf> = HashSet::new();
    let mut parent_dirs: HashSet<PathBuf> = HashSet::new();
    let mut input_paths: Vec<PathBuf> = Vec::new();
    for p in paths {
        let path = p.as_ref().to_path_buf();
        input_paths.push(path.clone());
        targets.insert(path.clone());
        if let Ok(canon) = std::fs::canonicalize(&path) {
            targets.insert(canon);
        }
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        parent_dirs.insert(parent);
    }

    let event_tx = tx.clone();
    // `with_compare_contents(true)` hashes file bodies during polling
    // and emits Modified events when content changes even if mtime
    // didn't tick (some editors / atomic-rename flows reuse the
    // previous mtime; without content compare, polling misses the
    // change entirely).
    let config = Config::default()
        .with_poll_interval(POLL_INTERVAL)
        .with_compare_contents(true);
    let mut watcher = PollWatcher::new(
        move |res: notify::Result<Event>| {
            let change = match res {
                Ok(event) => {
                    // Drop directory-level events that aren't about a
                    // path the caller cares about. Compare each path
                    // in the event against the targets set (which
                    // carries both the caller-supplied and
                    // canonicalised forms). An event with no paths in
                    // our set is noise from a sibling file inside the
                    // watched parent directory.
                    let any_match = event.paths.iter().any(|p| {
                        if targets.contains(p) {
                            return true;
                        }
                        // Backends may canonicalise paths
                        // (`/private/var/...` on macOS); the caller
                        // may have passed the non-canonical form.
                        // Canonicalise the event path too before
                        // giving up.
                        if let Ok(canon) = std::fs::canonicalize(p)
                            && targets.contains(&canon)
                        {
                            return true;
                        }
                        false
                    });
                    if !any_match {
                        return;
                    }
                    match event_to_change(&event) {
                        Some(c) => c,
                        None => return,
                    }
                }
                Err(err) => FileChange::Error {
                    message: err.to_string(),
                },
            };
            // Best-effort send; if the consumer is gone or full we drop
            // the event rather than block the notify backend thread.
            let _ = event_tx.try_send(change);
        },
        config,
    )
    .map_err(|err| FsWatchError::Init(err.to_string()))?;

    // Pre-validate: if a requested path's parent doesn't exist, fail
    // early naming the *requested* path (not the parent we'd silently
    // substitute). The `nonexistent_path_returns_watch_error` test
    // pins this contract — callers that pass `/does/not/exist/xyz`
    // expect an error whose `path` field is exactly that string, not
    // the synthesised parent.
    for path in &input_paths {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if !parent.exists() {
            return Err(FsWatchError::Watch {
                path: path.clone(),
                message: format!("parent directory does not exist: {}", parent.display()),
            });
        }
    }

    for parent in &parent_dirs {
        // Canonicalise the parent so FSEvents/inotify register the
        // canonical path (e.g. `/private/var/...` on macOS rather
        // than the `/var/...` symlink). Without this, events arrive
        // with canonical paths that the caller's non-canonical input
        // doesn't match — even with target-set canonicalisation, the
        // watch itself may not fire on the right inode.
        let watch_target = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.clone());
        watcher
            .watch(&watch_target, RecursiveMode::NonRecursive)
            .map_err(|err| FsWatchError::Watch {
                path: parent.clone(),
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
