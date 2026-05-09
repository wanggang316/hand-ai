//! Path-related helpers ported from `pi-coding-agent`'s `paths.ts`.
//!
//! Two small utilities:
//! - [`canonicalize_path`] — best-effort canonicalization that falls back to
//!   the input when the target does not exist or cannot be resolved.
//! - [`is_local_path`] — reject known non-local prefixes (`npm:`, `git:`,
//!   `github:`, `http:`, `https:`, `ssh:`); everything else (bare names,
//!   absolute paths, relative paths) is treated as local.

use std::path::{Path, PathBuf};

/// Resolve a path to its canonical form, following symlinks.
///
/// Falls back to the raw input when canonicalization fails (e.g. the target
/// does not exist yet, or the caller lacks permission to traverse), so this
/// function never panics and never errors. This mirrors the TS helper which
/// silently swallows `realpathSync` failures.
pub fn canonicalize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Return `true` when `value` is a local filesystem path rather than a
/// package source or URL.
///
/// Recognized non-local prefixes (case-sensitive, matching the TS source):
/// `npm:`, `git:`, `github:`, `http:`, `https:`, `ssh:`. Bare names and
/// relative paths without a `./` prefix are still considered local.
pub fn is_local_path(value: &str) -> bool {
    const NON_LOCAL_PREFIXES: &[&str] = &["npm:", "git:", "github:", "http:", "https:", "ssh:"];
    let trimmed = value.trim();
    !NON_LOCAL_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_existing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = canonicalize_path(tmp.path());
        // tempdir() may itself live behind a symlink (macOS /var → /private/var).
        // Result must end in the same directory name.
        assert_eq!(canonical.file_name(), tmp.path().file_name());
        assert!(canonical.is_absolute());
    }

    #[test]
    fn canonicalize_nonexistent_path_returns_input() {
        let nonexistent = "/this/path/should/not/exist/anywhere/12345";
        let result = canonicalize_path(nonexistent);
        assert_eq!(result, PathBuf::from(nonexistent));
    }

    #[test]
    fn canonicalize_empty_path_returns_input() {
        let result = canonicalize_path("");
        assert_eq!(result, PathBuf::from(""));
    }

    #[test]
    fn local_paths_recognized() {
        assert!(is_local_path("./relative/path"));
        assert!(is_local_path("/absolute/path"));
        assert!(is_local_path("relative-no-dot"));
        assert!(is_local_path("file.txt"));
        assert!(is_local_path(""));
        assert!(is_local_path("   /some/path  "));
    }

    #[test]
    fn url_and_package_prefixes_are_not_local() {
        assert!(!is_local_path("npm:lodash"));
        assert!(!is_local_path("git:owner/repo"));
        assert!(!is_local_path("github:owner/repo"));
        assert!(!is_local_path("http://example.com"));
        assert!(!is_local_path("https://example.com"));
        assert!(!is_local_path("ssh://git@example.com/repo"));
    }

    #[test]
    fn leading_whitespace_is_trimmed_before_prefix_check() {
        assert!(!is_local_path("   https://example.com"));
        assert!(!is_local_path("\tnpm:lodash"));
    }

    #[test]
    fn unknown_protocol_is_treated_as_local() {
        // `file:` and `ftp:` are not in the non-local set; mirrors TS behavior.
        assert!(is_local_path("file:///etc/hosts"));
        assert!(is_local_path("ftp://example.com"));
    }
}
