//! `@`-mention path completion — the cwd file source for the chat editor's
//! autocomplete popup.
//!
//! The editor ([`hand_tui::rt::components::Editor`]) carries the whole
//! autocomplete machinery (the [`AutocompleteProvider`] trait, the `@`/`/`
//! trigger seam via [`context_at_cursor`](hand_tui::rt::components::Editor::context_at_cursor),
//! and the popup that navigates + splices the accepted candidate). What it does
//! *not* carry is a data source: hand-tui's [`PathProvider`] matches over an
//! **injected** file list so its rules are unit-testable without a filesystem.
//!
//! This module is the app-layer bridge that fills that seam. It walks the cwd
//! *once* at editor construction — reusing the [`/tree`](super::tree_selector)
//! picker's [`scan_tree`] walk so the same skip-list (`.git`, `target`,
//! `node_modules`) and graceful degradation on an unreadable directory apply —
//! caps the snapshot to [`MAX_ENTRIES`] paths, and hands the result to a
//! [`PathProvider`]. The provider is queried synchronously on every keystroke,
//! so the walk is a one-time snapshot, never a per-query re-scan.
//!
//! The returned provider answers the `@` trigger only: it is inert on the `/`
//! trigger (returns nothing), so slash-command dispatch and ordinary typing are
//! unaffected — only an `@<prefix>` token under the caret opens the popup.

use std::path::Path;
use std::sync::Arc;

use hand_tui::rt::components::{AutocompleteProvider, PathEntry, PathProvider};

use super::tree_selector::scan_tree;

/// The most cwd paths the `@`-mention source snapshots. The popup itself caps the
/// *visible* rows; this bounds the injected list so a huge tree neither bloats
/// memory nor slows the per-keystroke prefix scan. Directories-first ordering
/// from [`scan_tree`] means shallow entries survive the cap.
pub const MAX_ENTRIES: usize = 500;

/// Build the `@`-path autocomplete provider for `cwd`.
///
/// Walks `cwd` once (via [`scan_tree`], inheriting its noise-dir skip-list and
/// unreadable-dir tolerance), converts each row to a [`PathEntry`], caps the list
/// at [`MAX_ENTRIES`], and wraps it in a [`PathProvider`]. The provider claims the
/// `@` trigger only, so it is a no-op for `/` and normal typing.
///
/// An empty or unreadable cwd yields a provider over an empty list — a
/// well-formed provider that simply never surfaces a candidate, never an error.
#[must_use]
pub fn build_mention_provider(cwd: &Path) -> Arc<dyn AutocompleteProvider> {
    let entries: Vec<PathEntry> = scan_tree(cwd)
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|row| {
            if row.is_dir {
                PathEntry::dir(row.rel_path)
            } else {
                PathEntry::file(row.rel_path)
            }
        })
        .collect();
    Arc::new(PathProvider::new(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[hand_tui::rt::components::AutocompleteItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn at_prefix_surfaces_a_matching_file_in_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();

        let provider = build_mention_provider(dir.path());
        // `@READM` → README.md by basename prefix (case-insensitive).
        let got = labels(&provider.query('@', "READM"));
        assert!(got.contains(&"README.md".to_string()), "got: {got:?}");
        assert!(!got.contains(&"main.rs".to_string()), "got: {got:?}");
    }

    #[test]
    fn a_no_match_prefix_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();

        let provider = build_mention_provider(dir.path());
        assert!(provider.query('@', "zzz-no-such-file").is_empty());
    }

    #[test]
    fn a_directory_prefix_surfaces_the_dir_with_a_trailing_slash() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("crates")).unwrap();
        std::fs::write(dir.path().join("crates").join("keep.rs"), "").unwrap();

        let provider = build_mention_provider(dir.path());
        let items = provider.query('@', "crate");
        let got = labels(&items);
        assert!(got.contains(&"crates/".to_string()), "dir slash: {got:?}");
        // The insertion carries the sigil so the buffer lands `@crates/`.
        let dir_item = items.iter().find(|i| i.label == "crates/").unwrap();
        assert_eq!(dir_item.insert_text, "@crates/");
    }

    #[test]
    fn is_inert_on_a_non_at_trigger() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();

        let provider = build_mention_provider(dir.path());
        // Slash dispatch must be unaffected: the mention source never answers `/`.
        assert!(!provider.handles('/'));
        assert!(provider.query('/', "m").is_empty());
        assert!(provider.query('/', "").is_empty());
    }

    #[test]
    fn noise_directories_are_skipped_via_the_shared_walk() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("config"), "").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "").unwrap();

        let provider = build_mention_provider(dir.path());
        // The empty query returns every snapshot entry; `.git` must not be one.
        let got = labels(&provider.query('@', ""));
        assert!(got.contains(&"keep.txt".to_string()), "got: {got:?}");
        assert!(
            !got.iter().any(|l| l.contains(".git")),
            "noise dir leaked: {got:?}"
        );
    }

    #[test]
    fn an_unreadable_cwd_yields_a_well_formed_empty_provider() {
        // A path that is not a directory yields no rows (scan_tree read_dir fails
        // gracefully) — the provider is well-formed and simply surfaces nothing.
        let provider = build_mention_provider(Path::new("/no/such/path/at/all"));
        assert!(provider.query('@', "").is_empty());
        assert!(!provider.handles('/'));
    }
}
