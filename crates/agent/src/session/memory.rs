//! In-memory session storage — ephemeral sessions and tests.

use std::collections::HashMap;
use std::sync::Mutex;

use super::store::{SessionStore, entries_up_to, forked_header};
use super::types::{SessionEntry, SessionHeader, SessionStoreError, SessionSummary};

/// Stored state per session: header, body entries, updated_ms.
type SessionRecord = (SessionHeader, Vec<SessionEntry>, i64);

/// [`SessionStore`] backend that keeps everything in process memory.
///
/// `updated_ms` bookkeeping: `create` initializes it from the header
/// timestamp; `append` bumps it to the entry's `timestamp()` when the
/// payload carries one, otherwise the previous value is kept.
#[derive(Default)]
pub struct InMemoryStore {
    sessions: Mutex<HashMap<String, SessionRecord>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for InMemoryStore {
    fn create(&self, header: &SessionHeader) -> Result<(), SessionStoreError> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.contains_key(&header.id) {
            return Err(SessionStoreError::Invalid(format!(
                "session already exists: {}",
                header.id
            )));
        }
        sessions.insert(
            header.id.clone(),
            (header.clone(), Vec::new(), header.timestamp),
        );
        Ok(())
    }

    fn append(&self, session_id: &str, entry: &SessionEntry) -> Result<(), SessionStoreError> {
        let mut sessions = self.sessions.lock().unwrap();
        let record = sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        if let Some(ts) = entry.timestamp() {
            record.2 = ts;
        }
        record.1.push(entry.clone());
        Ok(())
    }

    fn read_header(&self, session_id: &str) -> Result<SessionHeader, SessionStoreError> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .map(|(header, _, _)| header.clone())
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))
    }

    fn load(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeader, Vec<SessionEntry>), SessionStoreError> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .map(|(header, entries, _)| (header.clone(), entries.clone()))
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))
    }

    fn list(&self) -> Result<Vec<SessionSummary>, SessionStoreError> {
        let sessions = self.sessions.lock().unwrap();
        let mut summaries: Vec<SessionSummary> = sessions
            .values()
            .map(|(header, _, updated_ms)| SessionSummary {
                header: header.clone(),
                updated_ms: *updated_ms,
            })
            .collect();
        summaries.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        Ok(summaries)
    }

    fn fork(
        &self,
        from: &str,
        new_id: &str,
        timestamp: i64,
        up_to: Option<&str>,
    ) -> Result<SessionHeader, SessionStoreError> {
        let mut sessions = self.sessions.lock().unwrap();
        let (source_header, source_entries, _) = sessions
            .get(from)
            .ok_or_else(|| SessionStoreError::NotFound(from.to_string()))?;
        if sessions.contains_key(new_id) {
            return Err(SessionStoreError::Invalid(format!(
                "session already exists: {new_id}"
            )));
        }
        let copied = entries_up_to(source_entries, up_to)?;
        let new_header = forked_header(source_header, new_id, timestamp);
        sessions.insert(new_id.to_string(), (new_header.clone(), copied, timestamp));
        Ok(new_header)
    }
}
