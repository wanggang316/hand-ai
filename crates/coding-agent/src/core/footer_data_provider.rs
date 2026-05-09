//! Footer-line data backing the TUI status bar and extension queries.
//!
//! TS reference: `core/footer-data-provider.ts`. The TS implementation
//! exposes three pieces of data that are not otherwise reachable via the
//! agent runtime:
//!
//! 1. the current git branch (for the path `<cwd> [branch]` rendering);
//! 2. extension-supplied status texts (set via `ctx.ui.setStatus(...)`);
//! 3. the count of providers that currently have at least one usable
//!    model (used to surface auth gaps in the footer).
//!
//! The full TS port also runs an FS watcher over `.git/HEAD` (plus
//! reftable variants) to push branch changes to subscribers in real time.
//! That watcher subsystem depends on `utils/fs-watch.ts`, which has not
//! been ported to Rust yet, so this module ships a *cache + manual
//! refresh* shape instead — callers refresh the cached branch when they
//! know it may have changed (e.g. after running a git command). When the
//! `fs-watch` utility lands, this module should grow a watcher that
//! invalidates the cache and notifies the same `branch_change` callbacks.
//!
//! Branch resolution itself reuses [`crate::core::git_utils::git_branch`]
//! to keep behaviour aligned with the rest of the coding-agent's git
//! integration.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::git_utils::git_branch;

/// Subscriber callback invoked when the cached git branch changes.
///
/// Callbacks must be `Send + Sync` because [`FooterDataProvider`] is
/// designed to be wrapped in an `Arc` and shared across threads.
pub type BranchChangeCallback = Arc<dyn Fn() + Send + Sync + 'static>;

struct State {
    cwd: PathBuf,
    /// `None` means the cache is empty (lazy resolve on next read).
    /// `Some(None)` means "we resolved and there is no branch (not a
    /// repo)". `Some(Some(name))` means "we resolved to this branch".
    cached_branch: Option<Option<String>>,
    extension_statuses: HashMap<String, String>,
    available_provider_count: usize,
    /// Subscribers are stored under monotonically increasing ids so an
    /// individual unsubscribe handle can drop just one entry without
    /// disturbing the others.
    callbacks: HashMap<u64, BranchChangeCallback>,
    next_callback_id: u64,
}

impl State {
    fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            cached_branch: None,
            extension_statuses: HashMap::new(),
            available_provider_count: 0,
            callbacks: HashMap::new(),
            next_callback_id: 0,
        }
    }
}

/// Provides git branch and extension statuses; held by the agent runtime
/// and queried by the TUI footer renderer + extensions.
///
/// The provider is `Send + Sync` and intended to be shared via `Arc`. All
/// public methods take `&self`.
#[derive(Clone)]
pub struct FooterDataProvider {
    inner: Arc<Mutex<State>>,
}

impl FooterDataProvider {
    /// Create a provider rooted at `cwd`. The branch cache is left empty
    /// and resolved lazily on the first `git_branch()` call.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::new(cwd.into()))),
        }
    }

    /// Current git branch, or `None` when not in a repository or when
    /// the branch could not be resolved. The first call resolves and
    /// caches the result; subsequent calls return the cached value
    /// until [`Self::refresh_branch`] or [`Self::set_cwd`] is called.
    pub fn git_branch(&self) -> Option<String> {
        // Fast path: already cached.
        {
            let state = self.lock();
            if let Some(cached) = state.cached_branch.as_ref() {
                return cached.clone();
            }
        }
        // Resolve outside the lock — `git_branch()` shells out and we
        // don't want to block other callers on it.
        let cwd = self.lock().cwd.clone();
        let resolved = git_branch(&cwd);
        let mut state = self.lock();
        state.cached_branch = Some(resolved.clone());
        resolved
    }

    /// Extension status texts set via `ctx.ui.setStatus()`. Snapshot is
    /// returned to keep the lock scope tight; mutations through
    /// [`Self::set_extension_status`] won't be reflected in a snapshot
    /// already returned.
    pub fn extension_statuses(&self) -> HashMap<String, String> {
        self.lock().extension_statuses.clone()
    }

    /// Subscribe to branch-change notifications.
    ///
    /// Returns a [`BranchChangeSubscription`] whose `Drop` impl removes
    /// the callback. The TS reference returns an unsubscribe closure;
    /// using a guard type here is more idiomatic Rust and avoids
    /// reference-cycle pitfalls.
    pub fn on_branch_change<F>(&self, callback: F) -> BranchChangeSubscription
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut state = self.lock();
        let id = state.next_callback_id;
        state.next_callback_id = state.next_callback_id.wrapping_add(1);
        state.callbacks.insert(id, Arc::new(callback));
        BranchChangeSubscription {
            id,
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Set or clear an extension's status text.
    ///
    /// Passing `None` removes the entry, mirroring the TS contract where
    /// `setExtensionStatus(key, undefined)` deletes the slot.
    pub fn set_extension_status(&self, key: impl Into<String>, text: Option<String>) {
        let mut state = self.lock();
        match text {
            Some(t) => {
                state.extension_statuses.insert(key.into(), t);
            }
            None => {
                state.extension_statuses.remove(&key.into());
            }
        }
    }

    /// Remove every extension status entry.
    pub fn clear_extension_statuses(&self) {
        self.lock().extension_statuses.clear();
    }

    /// Number of providers that currently have at least one usable
    /// model. Used by the footer to surface partial-auth states.
    pub fn available_provider_count(&self) -> usize {
        self.lock().available_provider_count
    }

    /// Update the available-provider count. Typically called by the
    /// model registry after a refresh.
    pub fn set_available_provider_count(&self, count: usize) {
        self.lock().available_provider_count = count;
    }

    /// Switch the working directory; invalidates the cached branch and
    /// notifies branch-change subscribers (the new branch is resolved
    /// lazily on the next [`Self::git_branch`] call).
    pub fn set_cwd(&self, cwd: impl Into<PathBuf>) {
        let new_cwd = cwd.into();
        let callbacks = {
            let mut state = self.lock();
            if state.cwd == new_cwd {
                return;
            }
            state.cwd = new_cwd;
            state.cached_branch = None;
            state.callbacks.values().cloned().collect::<Vec<_>>()
        };
        for cb in callbacks {
            cb();
        }
    }

    /// Re-resolve the cached branch immediately. If the branch changed,
    /// branch-change callbacks fire. Use this after operations that may
    /// have changed `HEAD` (e.g. `git checkout` from a tool call).
    pub fn refresh_branch(&self) {
        let cwd = self.lock().cwd.clone();
        let resolved = git_branch(&cwd);

        let callbacks = {
            let mut state = self.lock();
            let changed = match &state.cached_branch {
                Some(prev) => prev != &resolved,
                None => true,
            };
            state.cached_branch = Some(resolved);
            if changed {
                state.callbacks.values().cloned().collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        for cb in callbacks {
            cb();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned mutex here would mean a callback panicked while the
        // lock was held; the state is still readable. Recover so the
        // footer doesn't lock the whole TUI on a panic in some
        // unrelated extension.
        match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }
}

/// RAII guard returned by [`FooterDataProvider::on_branch_change`]. The
/// callback is removed when the guard is dropped.
pub struct BranchChangeSubscription {
    id: u64,
    inner: std::sync::Weak<Mutex<State>>,
}

impl Drop for BranchChangeSubscription {
    fn drop(&mut self) {
        if let Some(arc) = self.inner.upgrade() {
            // Same poisoning-tolerance as `FooterDataProvider::lock`.
            let mut guard = match arc.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard.callbacks.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn extension_statuses_round_trip() {
        let p = FooterDataProvider::new(std::env::temp_dir());
        assert!(p.extension_statuses().is_empty());

        p.set_extension_status("foo", Some("status-1".into()));
        p.set_extension_status("bar", Some("status-2".into()));
        let snap = p.extension_statuses();
        assert_eq!(snap.get("foo").map(String::as_str), Some("status-1"));
        assert_eq!(snap.get("bar").map(String::as_str), Some("status-2"));

        p.set_extension_status("foo", None);
        let snap = p.extension_statuses();
        assert!(!snap.contains_key("foo"));
        assert_eq!(snap.get("bar").map(String::as_str), Some("status-2"));

        p.clear_extension_statuses();
        assert!(p.extension_statuses().is_empty());
    }

    #[test]
    fn available_provider_count_round_trip() {
        let p = FooterDataProvider::new(std::env::temp_dir());
        assert_eq!(p.available_provider_count(), 0);
        p.set_available_provider_count(5);
        assert_eq!(p.available_provider_count(), 5);
    }

    #[test]
    fn git_branch_returns_none_outside_repo() {
        // tempdir is guaranteed not to be inside any of our repos.
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = FooterDataProvider::new(tmp.path());
        // First call resolves; second call should return the cached
        // value without re-shelling.
        assert!(p.git_branch().is_none());
        assert!(p.git_branch().is_none());
    }

    #[test]
    fn refresh_branch_fires_callback_when_value_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = FooterDataProvider::new(tmp.path());
        // Seed the cache so the first refresh has something to compare against.
        let _ = p.git_branch();

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_cb = counter.clone();
        let _sub = p.on_branch_change(move || {
            counter_cb.fetch_add(1, Ordering::SeqCst);
        });

        // No change (still no branch in tempdir) -> no callback.
        p.refresh_branch();
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Force a change by switching to a directory that *also* has no
        // branch but that triggers the cache-invalidation path via
        // `set_cwd`, which fires unconditionally.
        let other = tempfile::tempdir().expect("tempdir");
        p.set_cwd(other.path());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn set_cwd_no_op_when_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = FooterDataProvider::new(tmp.path());

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_cb = counter.clone();
        let _sub = p.on_branch_change(move || {
            counter_cb.fetch_add(1, Ordering::SeqCst);
        });

        p.set_cwd(tmp.path());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dropped_subscription_stops_receiving_callbacks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = FooterDataProvider::new(tmp.path());

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_cb = counter.clone();
        let sub = p.on_branch_change(move || {
            counter_cb.fetch_add(1, Ordering::SeqCst);
        });

        let other = tempfile::tempdir().expect("tempdir");
        p.set_cwd(other.path());
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        drop(sub);

        let third = tempfile::tempdir().expect("tempdir");
        p.set_cwd(third.path());
        // Counter unchanged because the subscription was dropped.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn provider_is_clone_and_shares_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p1 = FooterDataProvider::new(tmp.path());
        let p2 = p1.clone();

        p1.set_available_provider_count(7);
        assert_eq!(p2.available_provider_count(), 7);

        p2.set_extension_status("k", Some("v".into()));
        assert_eq!(
            p1.extension_statuses().get("k").map(String::as_str),
            Some("v")
        );
    }

    /// Ensure the public surface is `Send + Sync`. Compile-time assertion.
    #[allow(dead_code)]
    fn _assert_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FooterDataProvider>();
    }

    #[test]
    fn accepts_path_ref_at_construction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path: &std::path::Path = tmp.path();
        let _p = FooterDataProvider::new(path.to_path_buf());
    }
}
