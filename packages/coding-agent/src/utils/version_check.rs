//! Version comparison + crates.io update probe.
//!
//! Mirrors `pi-coding-agent`'s `version-check.ts`, with two adjustments:
//!
//! 1. The TS source pings `https://pi.dev/api/latest-version`. We replace
//!    that with the crates.io public registry endpoint
//!    (`https://crates.io/api/v1/crates/hand-coding-agent`) since hand-ai
//!    is distributed via crates.io rather than a custom backend.
//! 2. The HTTP fetch is hidden behind the [`VersionFetcher`] trait so unit
//!    tests can inject a stub without going to the network. The default
//!    implementation [`HttpVersionFetcher`] uses `reqwest`.
//!
//! The semver-style comparison (`compare_package_versions`,
//! `is_newer_package_version`) is a direct port of the TS algorithm —
//! tolerant of `v` prefixes, ignores build metadata after `+`, and treats
//! prereleases as ordering-lower than the corresponding release.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use super::pi_user_agent::hand_user_agent;

/// crates.io endpoint returning the latest published version.
const CRATES_IO_URL: &str = "https://crates.io/api/v1/crates/hand-coding-agent";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors raised by the default HTTP fetcher.
#[derive(Debug, Error)]
pub enum VersionFetchError {
    /// Underlying HTTP transport error.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// Response could not be parsed into the expected JSON shape.
    #[error("malformed response: {0}")]
    Malformed(String),
}

/// A parsed `MAJOR.MINOR.PATCH[-PRERELEASE]` triple, with optional `+build`
/// stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedVersion {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

fn parse_package_version(version: &str) -> Option<ParsedVersion> {
    let trimmed = version.trim();
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    // Drop build-metadata suffix (`+...`).
    let core = trimmed.split_once('+').map(|(c, _)| c).unwrap_or(trimmed);
    // Split off prerelease (`-...`) if any.
    let (numeric, prerelease) = match core.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (core, None),
    };

    let parts: Vec<&str> = numeric.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major: u64 = parts[0].parse().ok()?;
    let minor: u64 = parts[1].parse().ok()?;
    let patch: u64 = parts[2].parse().ok()?;
    let prerelease = prerelease.and_then(|p| {
        if p.is_empty() {
            None
        } else if p
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            Some(p.to_string())
        } else {
            None
        }
    });
    Some(ParsedVersion {
        major,
        minor,
        patch,
        prerelease,
    })
}

/// Compare two package version strings. Returns `Some(Ordering)` when both
/// parse cleanly; `None` when either side is unparseable.
///
/// Mirrors the TS `comparePackageVersions` numeric-return convention but
/// expressed as `std::cmp::Ordering`. Prerelease handling matches semver
/// semantics: a release (`1.0.0`) is greater than a prerelease of the same
/// numeric triple (`1.0.0-rc1`), and prereleases compare lexicographically.
pub fn compare_package_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let l = parse_package_version(left)?;
    let r = parse_package_version(right)?;
    let primary = l
        .major
        .cmp(&r.major)
        .then(l.minor.cmp(&r.minor))
        .then(l.patch.cmp(&r.patch));
    if primary != Ordering::Equal {
        return Some(primary);
    }
    Some(match (&l.prerelease, &r.prerelease) {
        (None, None) => Ordering::Equal,
        // Release outranks prerelease.
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => a.cmp(b),
    })
}

/// Returns `true` if `candidate` is strictly newer than `current`.
///
/// When versions are unparseable, falls back to a trimmed string-inequality
/// check (matching TS behavior — any difference at all is treated as
/// "newer"). This is intentionally lenient: we'd rather over-prompt than
/// silently swallow a release with an unusual tag.
pub fn is_newer_package_version(candidate: &str, current: &str) -> bool {
    match compare_package_versions(candidate, current) {
        Some(std::cmp::Ordering::Greater) => true,
        Some(_) => false,
        None => candidate.trim() != current.trim(),
    }
}

/// Trait abstracting the network call so tests can inject deterministic
/// responses. Implementations return the raw version string from the
/// registry, or `None` to indicate "no info available".
#[async_trait]
pub trait VersionFetcher: Send + Sync {
    async fn fetch_latest(
        &self,
        current_version: &str,
    ) -> Result<Option<String>, VersionFetchError>;
}

/// Default fetcher hitting the crates.io registry.
pub struct HttpVersionFetcher {
    client: reqwest::Client,
    timeout: Duration,
    url: String,
}

impl HttpVersionFetcher {
    /// Construct with the default timeout and crates.io URL.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            timeout: DEFAULT_TIMEOUT,
            url: CRATES_IO_URL.to_string(),
        }
    }

    /// Override the HTTP timeout (default 10s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the registry URL — primarily for integration tests pointing
    /// at a fake server.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

impl Default for HttpVersionFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VersionFetcher for HttpVersionFetcher {
    async fn fetch_latest(
        &self,
        current_version: &str,
    ) -> Result<Option<String>, VersionFetchError> {
        // Honor the same opt-out env vars as the TS implementation. These
        // are checked at call time rather than at construction so tests
        // and downstream users can flip them per-run.
        if std::env::var_os("HAND_SKIP_VERSION_CHECK").is_some()
            || std::env::var_os("HAND_OFFLINE").is_some()
        {
            return Ok(None);
        }

        let response = self
            .client
            .get(&self.url)
            .header("User-Agent", hand_user_agent(current_version))
            .header("Accept", "application/json")
            .timeout(self.timeout)
            .send()
            .await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let payload: serde_json::Value = response.json().await?;
        // crates.io shape: { "crate": { "max_stable_version": "x.y.z", ... } }
        let candidate = payload
            .get("crate")
            .and_then(|c| c.get("max_stable_version"))
            .or_else(|| payload.get("crate").and_then(|c| c.get("newest_version")))
            .or_else(|| payload.get("version"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(candidate)
    }
}

/// Fetch the latest published version and, if newer than `current_version`,
/// return it. Returns `None` when up-to-date, when the fetch fails, or when
/// the version-check opt-out env vars are set.
///
/// Errors from the fetcher are intentionally swallowed — version checks are
/// best-effort and must not block startup.
pub async fn check_for_new_version<F: VersionFetcher>(
    fetcher: &F,
    current_version: &str,
) -> Option<String> {
    let latest = fetcher.fetch_latest(current_version).await.ok().flatten()?;
    if is_newer_package_version(&latest, current_version) {
        Some(latest)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---- compare_package_versions ----------------------------------------

    #[test]
    fn compare_equal_versions() {
        assert_eq!(
            compare_package_versions("1.2.3", "1.2.3"),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn compare_left_greater_by_major() {
        assert_eq!(
            compare_package_versions("2.0.0", "1.9.9"),
            Some(std::cmp::Ordering::Greater)
        );
    }

    #[test]
    fn compare_left_lesser_by_patch() {
        assert_eq!(
            compare_package_versions("1.0.0", "1.0.1"),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn compare_release_outranks_prerelease() {
        assert_eq!(
            compare_package_versions("1.0.0", "1.0.0-rc1"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_package_versions("1.0.0-rc1", "1.0.0"),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn compare_prereleases_lexicographically() {
        assert_eq!(
            compare_package_versions("1.0.0-alpha", "1.0.0-beta"),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn compare_strips_v_prefix_and_build_metadata() {
        assert_eq!(
            compare_package_versions("v1.0.0+build123", "1.0.0+other"),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn compare_unparseable_returns_none() {
        assert!(compare_package_versions("not-a-version", "1.0.0").is_none());
        assert!(compare_package_versions("1.0", "1.0.0").is_none());
    }

    // ---- is_newer_package_version ---------------------------------------

    #[test]
    fn is_newer_true_when_strictly_greater() {
        assert!(is_newer_package_version("1.0.1", "1.0.0"));
    }

    #[test]
    fn is_newer_false_when_equal() {
        assert!(!is_newer_package_version("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_false_when_lesser() {
        assert!(!is_newer_package_version("0.9.0", "1.0.0"));
    }

    #[test]
    fn is_newer_falls_back_to_string_inequality_when_unparseable() {
        // Both unparseable — returns true if strings differ.
        assert!(is_newer_package_version("custom-tag-a", "custom-tag-b"));
        assert!(!is_newer_package_version("same-tag", "same-tag"));
        // One unparseable side — falls back to string compare since
        // `compare_package_versions` returns None.
        assert!(is_newer_package_version("not-semver", "1.0.0"));
    }

    // ---- VersionFetcher trait + check_for_new_version --------------------

    struct StubFetcher {
        response: Mutex<Result<Option<String>, VersionFetchError>>,
    }

    impl StubFetcher {
        fn ok(version: Option<&str>) -> Self {
            Self {
                response: Mutex::new(Ok(version.map(|s| s.to_string()))),
            }
        }

        fn err() -> Self {
            // Use a synthetic Malformed error; reqwest::Error can't be
            // constructed externally.
            Self {
                response: Mutex::new(Err(VersionFetchError::Malformed("stub".into()))),
            }
        }
    }

    #[async_trait]
    impl VersionFetcher for StubFetcher {
        async fn fetch_latest(&self, _current: &str) -> Result<Option<String>, VersionFetchError> {
            // Drain the stored response (single-shot). Subsequent calls
            // see Ok(None), which is fine for the tests below.
            let mut guard = self.response.lock().unwrap();
            std::mem::replace(&mut *guard, Ok(None))
        }
    }

    #[tokio::test]
    async fn check_returns_some_when_remote_is_newer() {
        let fetcher = StubFetcher::ok(Some("2.0.0"));
        let result = check_for_new_version(&fetcher, "1.0.0").await;
        assert_eq!(result.as_deref(), Some("2.0.0"));
    }

    #[tokio::test]
    async fn check_returns_none_when_up_to_date() {
        let fetcher = StubFetcher::ok(Some("1.0.0"));
        let result = check_for_new_version(&fetcher, "1.0.0").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn check_returns_none_when_remote_is_older() {
        let fetcher = StubFetcher::ok(Some("0.9.0"));
        let result = check_for_new_version(&fetcher, "1.0.0").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn check_returns_none_when_fetcher_yields_none() {
        let fetcher = StubFetcher::ok(None);
        let result = check_for_new_version(&fetcher, "1.0.0").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn check_swallows_fetcher_errors() {
        let fetcher = StubFetcher::err();
        let result = check_for_new_version(&fetcher, "1.0.0").await;
        assert!(result.is_none());
    }
}
