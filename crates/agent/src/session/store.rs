//! The storage contract implemented by every session backend.

use super::types::{
    SESSION_FORMAT_VERSION, SessionEntry, SessionHeader, SessionStoreError, SessionSummary,
};

/// Synchronous storage contract for session logs. Implementations are
/// Send + Sync; async callers wrap calls in spawn_blocking.
pub trait SessionStore: Send + Sync {
    /// Persist a new empty session under `header.id`. Errors with
    /// Invalid if the id already exists.
    fn create(&self, header: &SessionHeader) -> Result<(), SessionStoreError>;

    /// Append one entry to an existing session.
    fn append(&self, session_id: &str, entry: &SessionEntry) -> Result<(), SessionStoreError>;

    /// Header only — must not read the whole log on backends where
    /// that is avoidable.
    fn read_header(&self, session_id: &str) -> Result<SessionHeader, SessionStoreError>;

    /// Full log: header plus entries in append order (header NOT
    /// duplicated into the entry vec).
    fn load(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeader, Vec<SessionEntry>), SessionStoreError>;

    /// All sessions, newest first by updated_ms. Headers only.
    fn list(&self) -> Result<Vec<SessionSummary>, SessionStoreError>;

    /// Create a new session copying entries from `from`. With
    /// `up_to = Some(entry_id)`, copy entries strictly BEFORE that
    /// entry; None copies everything. Entry ids are preserved verbatim
    /// so cross-references stay valid. The new header gets `new_id`,
    /// a fresh timestamp (caller-provided ts parameter), the same cwd,
    /// and parent_session = from's id. Returns the new header.
    fn fork(
        &self,
        from: &str,
        new_id: &str,
        timestamp: i64,
        up_to: Option<&str>,
    ) -> Result<SessionHeader, SessionStoreError>;
}

/// Shared fork semantics: the body entries copied into the new session.
///
/// `up_to = Some(id)` copies entries strictly before the entry with
/// that id and errors with [`SessionStoreError::Invalid`] when no entry
/// matches; `None` copies everything. Header envelopes are never copied
/// (the new session mints its own header).
pub(crate) fn entries_up_to(
    entries: &[SessionEntry],
    up_to: Option<&str>,
) -> Result<Vec<SessionEntry>, SessionStoreError> {
    let cut = match up_to {
        None => entries.len(),
        Some(target) => entries
            .iter()
            .position(|e| e.id() == Some(target))
            .ok_or_else(|| {
                SessionStoreError::Invalid(format!("fork target entry not found: {target}"))
            })?,
    };
    Ok(entries[..cut]
        .iter()
        .filter(|e| !e.is_header())
        .cloned()
        .collect())
}

/// Shared fork semantics: the header of the forked session. Carries the
/// source's cwd and records provenance via `parent_session`; `extra`
/// starts empty (fork metadata belongs to the new session, not the
/// old one) and the version is stamped fresh.
pub(crate) fn forked_header(source: &SessionHeader, new_id: &str, timestamp: i64) -> SessionHeader {
    SessionHeader {
        version: SESSION_FORMAT_VERSION,
        id: new_id.to_string(),
        timestamp,
        cwd: source.cwd.clone(),
        parent_session: Some(source.id.clone()),
        extra: serde_json::Map::new(),
    }
}
