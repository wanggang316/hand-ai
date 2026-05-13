//! Per-file mutation serialiser.
//!
//! Ported from `pi-mono` `core/tools/file-mutation-queue.ts`. The TS
//! original chains promises in a `Map<path, Promise<void>>` so callers
//! that target the same file run sequentially while different files run
//! in parallel.
//!
//! The Rust port reaches for a global `HashMap<PathBuf, Arc<Mutex<()>>>`.
//! Each [`with_file_mutation_queue`] call:
//!
//! 1. canonicalises the target path so two callers that resolve to the
//!    same on-disk file (e.g. via symlink) share a queue;
//! 2. fetches or inserts the per-path lock under a brief shared mutex
//!    on the registry;
//! 3. acquires the per-path lock asynchronously and runs the closure.
//!
//! The registry is intentionally not cleaned up on drop — a future
//! caller for the same path will simply find the existing entry. This
//! mirrors the TS original, which also accumulates entries (it deletes
//! a key only when the last queued promise was the most recent one,
//! which races with new arrivals).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};

use tokio::sync::Mutex as AsyncMutex;

/// Registry of per-file locks. Keyed by canonicalised path.
static FILE_MUTATION_QUEUES: LazyLock<StdMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Resolve `path` to a stable key. Falls back to the literal path if
/// canonicalisation fails (e.g. the file does not yet exist — common
/// for write tools).
fn mutation_queue_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Run `fut` while holding the mutation lock for `path`.
///
/// Different paths run in parallel; the same path serialises. The lock
/// is released as soon as `fut` resolves — including via panic, since
/// `tokio::sync::Mutex` releases the guard on drop.
pub async fn with_file_mutation_queue<F, T>(path: &Path, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let key = mutation_queue_key(path);

    let lock = {
        let mut registry = FILE_MUTATION_QUEUES
            .lock()
            .expect("file mutation registry mutex poisoned");
        registry
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };

    let _guard = lock.lock().await;
    fut.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::time::sleep;

    #[tokio::test]
    async fn same_path_serialises_concurrent_callers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"x").unwrap();

        // Track concurrent occupancy.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let path = path.clone();
            let in_flight = Arc::clone(&in_flight);
            let max_seen = Arc::clone(&max_seen);
            handles.push(tokio::spawn(async move {
                with_file_mutation_queue(&path, async {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    let prev_max = max_seen.load(Ordering::SeqCst);
                    if now > prev_max {
                        max_seen.store(now, Ordering::SeqCst);
                    }
                    sleep(Duration::from_millis(10)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "same-path callers must never overlap"
        );
    }

    #[tokio::test]
    async fn different_paths_run_in_parallel() {
        let dir = TempDir::new().unwrap();
        let p1 = dir.path().join("a.txt");
        let p2 = dir.path().join("b.txt");
        std::fs::write(&p1, b"x").unwrap();
        std::fs::write(&p2, b"x").unwrap();

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let in_flight_a = Arc::clone(&in_flight);
        let max_seen_a = Arc::clone(&max_seen);
        let p1c = p1.clone();
        let h1 = tokio::spawn(async move {
            with_file_mutation_queue(&p1c, async {
                let now = in_flight_a.fetch_add(1, Ordering::SeqCst) + 1;
                let prev_max = max_seen_a.load(Ordering::SeqCst);
                if now > prev_max {
                    max_seen_a.store(now, Ordering::SeqCst);
                }
                sleep(Duration::from_millis(50)).await;
                in_flight_a.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let in_flight_b = Arc::clone(&in_flight);
        let max_seen_b = Arc::clone(&max_seen);
        let p2c = p2.clone();
        let h2 = tokio::spawn(async move {
            with_file_mutation_queue(&p2c, async {
                let now = in_flight_b.fetch_add(1, Ordering::SeqCst) + 1;
                let prev_max = max_seen_b.load(Ordering::SeqCst);
                if now > prev_max {
                    max_seen_b.store(now, Ordering::SeqCst);
                }
                sleep(Duration::from_millis(50)).await;
                in_flight_b.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let _ = tokio::join!(h1, h2);
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            2,
            "different-path callers should run in parallel"
        );
    }

    #[tokio::test]
    async fn returns_closure_value() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.txt");
        std::fs::write(&path, b"x").unwrap();

        let v = with_file_mutation_queue(&path, async { 7_i32 }).await;
        assert_eq!(v, 7);
    }

    #[tokio::test]
    async fn missing_file_uses_literal_key() {
        // Path does not exist on disk yet (common for write tools).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-yet-exist.txt");
        let v = with_file_mutation_queue(&path, async { "ok" }).await;
        assert_eq!(v, "ok");
    }

    /// Pi-mono parity: two paths that resolve to the same on-disk file
    /// through a symlink MUST share the queue. Without canonical-path
    /// keying, `link.txt` and `real.txt` would land in two different
    /// `Mutex` cells and race.
    #[cfg(unix)]
    #[tokio::test]
    async fn same_path_via_symlink_shares_queue() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, b"x").unwrap();
        symlink(&real, &link).unwrap();

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let in_flight_a = Arc::clone(&in_flight);
        let max_seen_a = Arc::clone(&max_seen);
        let real_c = real.clone();
        let h1 = tokio::spawn(async move {
            with_file_mutation_queue(&real_c, async {
                let now = in_flight_a.fetch_add(1, Ordering::SeqCst) + 1;
                let prev_max = max_seen_a.load(Ordering::SeqCst);
                if now > prev_max {
                    max_seen_a.store(now, Ordering::SeqCst);
                }
                sleep(Duration::from_millis(50)).await;
                in_flight_a.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let in_flight_b = Arc::clone(&in_flight);
        let max_seen_b = Arc::clone(&max_seen);
        let link_c = link.clone();
        let h2 = tokio::spawn(async move {
            with_file_mutation_queue(&link_c, async {
                let now = in_flight_b.fetch_add(1, Ordering::SeqCst) + 1;
                let prev_max = max_seen_b.load(Ordering::SeqCst);
                if now > prev_max {
                    max_seen_b.store(now, Ordering::SeqCst);
                }
                sleep(Duration::from_millis(50)).await;
                in_flight_b.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let _ = tokio::join!(h1, h2);
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "symlink alias must share the queue with real path"
        );
    }
}
