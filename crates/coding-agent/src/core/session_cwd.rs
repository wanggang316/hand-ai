//! Detect and report sessions whose stored working directory no longer
//! exists.
//!
//! When the user resumes a session whose original cwd has been deleted
//! (or the session is opened on a different machine), surface a clear
//! error with both the stored cwd and the current cwd so the caller
//! can decide whether to abort or fall back to the current directory.
//!
//! ## Source abstraction
//!
//! The module exposes a [`SessionCwdSource`] trait so the lookup can
//! be satisfied by both the real `SessionManager` and lightweight test
//! doubles. The real [`crate::core::session_manager::SessionManager`]
//! the real [`crate::core::session_manager::SessionManager`] does
//! **not** auto-implement (its `cwd` and session-file accessors have
//! different signatures); callers wire it up at the call site, e.g.
//! by passing a thin closure-backed adapter. That keeps this module
//! free of a hard dependency on `SessionManager`'s shape.
//!
//! Error reporting comes in two layers:
//!
//! - [`get_missing_session_cwd_issue`] — returns `Some(issue)` when the
//!   stored cwd is missing, `None` otherwise.
//! - [`MissingSessionCwdError`] — `thiserror`-derived; constructed via
//!   [`assert_session_cwd_exists`] for the common "either return Ok
//!   or fail loudly" path.
//!
//! `format_missing_session_cwd_error` and
//! `format_missing_session_cwd_prompt` mirror the TS string formatters
//! used by the CLI banner / TUI prompt respectively.

use std::path::{Path, PathBuf};

/// Details of a missing-cwd issue surfaced by
/// [`get_missing_session_cwd_issue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    /// On-disk session file (e.g. `~/.hand/sessions/<id>.jsonl`) when
    /// the source can identify one. `None` for in-memory sessions or
    /// when the source intentionally hides the path.
    pub session_file: Option<PathBuf>,
    /// The cwd recorded in the session header.
    pub session_cwd: PathBuf,
    /// The cwd to fall back to if the user accepts the prompt.
    pub fallback_cwd: PathBuf,
}

/// Source the session-cwd checker queries for the stored cwd and the
/// session file path. Mirrors TS `SessionCwdSource`.
pub trait SessionCwdSource {
    /// The cwd recorded in the session header, or an empty path /
    /// `None`-equivalent if no cwd is stored.
    fn cwd(&self) -> Option<PathBuf>;

    /// Path to the on-disk session file, or `None` for in-memory
    /// sessions.
    fn session_file(&self) -> Option<PathBuf>;
}

/// Return `Some(issue)` if the source has a session file *and* a
/// stored cwd that no longer exists on disk. Otherwise `None`.
///
/// Mirrors TS `getMissingSessionCwdIssue` with the same short-circuit
/// order — no session file means we have nothing to anchor the user
/// to, so we never surface an issue in that case.
pub fn get_missing_session_cwd_issue<S: SessionCwdSource + ?Sized>(
    source: &S,
    fallback_cwd: &Path,
) -> Option<SessionCwdIssue> {
    let session_file = source.session_file()?;

    let session_cwd = source.cwd()?;
    if session_cwd.as_os_str().is_empty() {
        return None;
    }
    if session_cwd.exists() {
        return None;
    }

    Some(SessionCwdIssue {
        session_file: Some(session_file),
        session_cwd,
        fallback_cwd: fallback_cwd.to_path_buf(),
    })
}

/// Format a [`SessionCwdIssue`] for the CLI banner / process error.
///
/// Mirrors TS `formatMissingSessionCwdError` byte-for-byte (modulo
/// path display, which uses [`std::path::Display`] rather than the JS
/// `string` coercion).
pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = match &issue.session_file {
        Some(p) => format!("\nSession file: {}", p.display()),
        None => String::new(),
    };
    format!(
        "Stored session working directory does not exist: {}{}\nCurrent working directory: {}",
        issue.session_cwd.display(),
        session_file,
        issue.fallback_cwd.display(),
    )
}

/// Format a [`SessionCwdIssue`] as a multi-line prompt for the TUI
/// "continue in current cwd?" overlay. Mirrors TS
/// `formatMissingSessionCwdPrompt`.
pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd.display(),
        issue.fallback_cwd.display(),
    )
}

/// Error raised by [`assert_session_cwd_exists`] when the stored cwd
/// is missing.
///
/// `Display` mirrors TS `MissingSessionCwdError.message`, produced by
/// [`format_missing_session_cwd_error`]; the structured `issue` is
/// preserved so callers can drive a recovery prompt.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", format_missing_session_cwd_error(.0))]
pub struct MissingSessionCwdError(pub SessionCwdIssue);

impl MissingSessionCwdError {
    /// The structured issue underlying this error.
    pub fn issue(&self) -> &SessionCwdIssue {
        &self.0
    }
}

/// Raise [`MissingSessionCwdError`] if the stored cwd is missing.
///
/// Mirrors TS `assertSessionCwdExists`. Returns `Ok(())` when the
/// source has no session file (i.e. in-memory) or the cwd exists.
pub fn assert_session_cwd_exists<S: SessionCwdSource + ?Sized>(
    source: &S,
    fallback_cwd: &Path,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(source, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError(issue)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSource {
        cwd: Option<PathBuf>,
        session_file: Option<PathBuf>,
    }

    impl SessionCwdSource for StubSource {
        fn cwd(&self) -> Option<PathBuf> {
            self.cwd.clone()
        }
        fn session_file(&self) -> Option<PathBuf> {
            self.session_file.clone()
        }
    }

    #[test]
    fn no_session_file_yields_no_issue() {
        let src = StubSource {
            cwd: Some(PathBuf::from("/nope/never/here")),
            session_file: None,
        };
        let fallback = PathBuf::from("/tmp");
        assert!(get_missing_session_cwd_issue(&src, &fallback).is_none());
        assert!(assert_session_cwd_exists(&src, &fallback).is_ok());
    }

    #[test]
    fn empty_cwd_yields_no_issue() {
        let src = StubSource {
            cwd: Some(PathBuf::new()),
            session_file: Some(PathBuf::from("/sessions/abc.jsonl")),
        };
        let fallback = PathBuf::from("/tmp");
        assert!(get_missing_session_cwd_issue(&src, &fallback).is_none());
    }

    #[test]
    fn existing_cwd_yields_no_issue() {
        let dir = std::env::temp_dir();
        let src = StubSource {
            cwd: Some(dir.clone()),
            session_file: Some(PathBuf::from("/sessions/abc.jsonl")),
        };
        let fallback = PathBuf::from("/tmp");
        assert!(get_missing_session_cwd_issue(&src, &fallback).is_none());
        assert!(assert_session_cwd_exists(&src, &fallback).is_ok());
    }

    #[test]
    fn missing_cwd_with_session_file_yields_issue() {
        let missing = PathBuf::from("/this/path/should/never/exist/for-test");
        let session_file = PathBuf::from("/sessions/abc.jsonl");
        let fallback = PathBuf::from("/tmp");
        let src = StubSource {
            cwd: Some(missing.clone()),
            session_file: Some(session_file.clone()),
        };
        let issue = get_missing_session_cwd_issue(&src, &fallback).expect("issue must surface");
        assert_eq!(issue.session_cwd, missing);
        assert_eq!(issue.session_file, Some(session_file));
        assert_eq!(issue.fallback_cwd, fallback);

        let err = assert_session_cwd_exists(&src, &fallback).expect_err("must error out");
        assert_eq!(err.issue(), &issue);
        let displayed = format!("{err}");
        assert!(displayed.contains("Stored session working directory does not exist:"));
        assert!(displayed.contains(&missing.display().to_string()));
    }

    #[test]
    fn format_missing_session_cwd_error_includes_session_file_when_present() {
        let issue = SessionCwdIssue {
            session_file: Some(PathBuf::from("/sessions/x.jsonl")),
            session_cwd: PathBuf::from("/old/cwd"),
            fallback_cwd: PathBuf::from("/new/cwd"),
        };
        let s = format_missing_session_cwd_error(&issue);
        assert!(s.starts_with("Stored session working directory does not exist: /old/cwd"));
        assert!(s.contains("\nSession file: /sessions/x.jsonl"));
        assert!(s.ends_with("Current working directory: /new/cwd"));
    }

    #[test]
    fn format_missing_session_cwd_error_omits_session_file_when_absent() {
        let issue = SessionCwdIssue {
            session_file: None,
            session_cwd: PathBuf::from("/old/cwd"),
            fallback_cwd: PathBuf::from("/new/cwd"),
        };
        let s = format_missing_session_cwd_error(&issue);
        assert!(!s.contains("Session file:"));
        assert!(s.contains("/old/cwd"));
        assert!(s.contains("/new/cwd"));
    }

    #[test]
    fn format_missing_session_cwd_prompt_matches_ts_layout() {
        let issue = SessionCwdIssue {
            session_file: None,
            session_cwd: PathBuf::from("/old/cwd"),
            fallback_cwd: PathBuf::from("/new/cwd"),
        };
        let s = format_missing_session_cwd_prompt(&issue);
        assert_eq!(
            s,
            "cwd from session file does not exist\n/old/cwd\n\ncontinue in current cwd\n/new/cwd"
        );
    }
}
