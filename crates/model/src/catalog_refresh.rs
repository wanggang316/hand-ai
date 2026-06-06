//! Runtime catalog refresh.
//!
//! Keeps the in-memory model catalog fresh without a rebuild or restart by
//! pulling a newer `models.json` from a remote URL into a local cache and
//! hot-swapping it into the registry. The layering is:
//!
//! ```text
//! embedded baseline (MODELS_JSON)  >  local cache  >  remote
//! ```
//!
//! The baseline always works offline; the cache lets the last fetched
//! catalog survive restarts; [`refresh_from_remote`] pulls a newer one in
//! the background. Every step degrades gracefully — on any error the
//! in-memory catalog is left untouched.

use std::path::{Path, PathBuf};

use crate::models::{Registry, install_catalog};

const CATALOG_FILE: &str = "models.json";
const ETAG_FILE: &str = "models.etag";

/// Outcome of a remote refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The remote returned `304 Not Modified`; the current catalog is up to date.
    Unchanged,
    /// A newer catalog was fetched, cached, and hot-swapped into the registry.
    Updated {
        /// Number of providers in the installed catalog.
        providers: usize,
        /// Total number of models in the installed catalog.
        models: usize,
    },
}

/// Errors raised by the refresh path. None of these mutate the in-memory
/// catalog — callers can ignore them and keep serving the previous data.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The HTTP request itself failed (DNS, TLS, timeout, …).
    #[error("catalog request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// The endpoint returned a non-success, non-304 status.
    #[error("catalog endpoint returned status {0}")]
    Status(u16),
    /// The payload did not parse into a non-empty catalog.
    #[error("catalog payload is invalid: {0}")]
    Parse(String),
    /// Reading or writing the on-disk cache failed.
    #[error("catalog cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Default cache directory: `~/.hand-ai` (shared with OAuth storage).
fn default_cache_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => home.join(".hand-ai"),
        None => PathBuf::from(".hand-ai"),
    }
}

/// Parse and sanity-check a catalog payload. Rejects empty or garbage input
/// so a bad remote push can never blank the in-memory catalog.
fn parse_and_validate(bytes: &str) -> Result<Registry, RefreshError> {
    let registry: Registry =
        serde_json::from_str(bytes).map_err(|e| RefreshError::Parse(e.to_string()))?;
    if registry.is_empty() || registry.values().all(|models| models.is_empty()) {
        return Err(RefreshError::Parse("catalog contains no models".into()));
    }
    Ok(registry)
}

fn read_cached_etag(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(ETAG_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_cache(dir: &Path, json: &str, etag: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(CATALOG_FILE), json)?;
    match etag {
        Some(tag) => std::fs::write(dir.join(ETAG_FILE), tag)?,
        None => {
            // Drop a stale ETag so the next refresh re-fetches in full.
            let _ = std::fs::remove_file(dir.join(ETAG_FILE));
        }
    }
    Ok(())
}

/// Load the locally cached catalog (if present and valid) and install it.
/// Returns `true` on success.
///
/// Call this once at startup, before kicking off a remote refresh, so the
/// last fetched catalog is used immediately instead of waiting on the
/// network. A missing or corrupt cache is a no-op (the embedded baseline
/// stays active) and returns `false`.
pub fn load_cached_catalog() -> bool {
    load_cached_catalog_from(&default_cache_dir())
}

fn load_cached_catalog_from(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read_to_string(dir.join(CATALOG_FILE)) else {
        return false;
    };
    match parse_and_validate(&bytes) {
        Ok(registry) => {
            install_catalog(registry);
            true
        }
        Err(_) => false,
    }
}

/// Fetch the catalog from `url`; if it changed (by ETag), validate it, write
/// it to the local cache, and hot-swap it into the registry.
///
/// Sends `If-None-Match` with the cached ETag so an unchanged catalog costs
/// a single `304`. On any error the in-memory catalog is left untouched and
/// the previous data keeps serving.
pub async fn refresh_from_remote(url: &str) -> Result<RefreshOutcome, RefreshError> {
    refresh_from_remote_in(url, &default_cache_dir()).await
}

async fn refresh_from_remote_in(url: &str, dir: &Path) -> Result<RefreshOutcome, RefreshError> {
    let client = reqwest::Client::new();
    let mut request = client.get(url);
    if let Some(etag) = read_cached_etag(dir) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(RefreshOutcome::Unchanged);
    }
    if !response.status().is_success() {
        return Err(RefreshError::Status(response.status().as_u16()));
    }

    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await?;

    let registry = parse_and_validate(&body)?;
    let providers = registry.len();
    let models = registry.values().map(|m| m.len()).sum();

    write_cache(dir, &body, etag.as_deref())?;
    install_catalog(registry);
    Ok(RefreshOutcome::Updated { providers, models })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate_rejects_empty_and_garbage() {
        assert!(parse_and_validate("").is_err());
        assert!(parse_and_validate("not json").is_err());
        assert!(parse_and_validate("{}").is_err(), "empty catalog rejected");
        assert!(
            parse_and_validate(r#"{"p":{}}"#).is_err(),
            "provider with no models rejected"
        );
    }

    #[test]
    fn parse_and_validate_accepts_the_embedded_baseline() {
        let registry =
            parse_and_validate(crate::models::MODELS_JSON).expect("baseline must validate");
        assert!(!registry.is_empty());
    }

    #[test]
    fn cache_round_trips_catalog_and_etag() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), r#"{"p":{"m":{}}}"#, Some("\"abc123\"")).unwrap();
        assert_eq!(read_cached_etag(tmp.path()).as_deref(), Some("\"abc123\""));
        let cached = std::fs::read_to_string(tmp.path().join(CATALOG_FILE)).unwrap();
        assert!(cached.contains("\"p\""));
    }

    #[test]
    fn write_cache_without_etag_clears_stale_etag() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "{}", Some("\"old\"")).unwrap();
        write_cache(tmp.path(), "{}", None).unwrap();
        assert_eq!(read_cached_etag(tmp.path()), None);
    }

    #[test]
    fn load_cached_catalog_from_missing_or_corrupt_is_false() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!load_cached_catalog_from(&tmp.path().join("absent")));
        write_cache(tmp.path(), "garbage", None).unwrap();
        assert!(!load_cached_catalog_from(tmp.path()));
    }
}
