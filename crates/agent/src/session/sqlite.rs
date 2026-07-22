//! SQLite session storage — every session in one database file.
//!
//! Backend for embedders that want session logs in a single file with
//! transactional appends instead of a directory of JSONL logs. The
//! header is stored as JSON (the same payload the JSONL header line
//! carries), so unknown header fields survive round-trips; entries
//! keep their `{kind, payload}` envelope split across columns, with a
//! per-session `seq` preserving append order.
//!
//! [`SqliteStore::open_with_import`] adopts an existing JSONL session
//! directory on open: read-only over the source files, idempotent
//! across runs, and tolerant of the same partial-write corruption
//! [`JsonlStore`] tolerates.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use super::jsonl::JsonlStore;
use super::store::{SessionStore, entries_up_to, forked_header};
use super::types::{SessionEntry, SessionHeader, SessionStoreError, SessionSummary};

/// Idempotent schema: sessions carry the full header as JSON plus the
/// columns queried without deserializing (listing order, provenance);
/// entries are the envelope split across columns, ordered by `seq`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    header_json TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    parent_session TEXT
);
CREATE TABLE IF NOT EXISTS entries (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions (updated_ms DESC);
";

impl From<rusqlite::Error> for SessionStoreError {
    fn from(e: rusqlite::Error) -> Self {
        SessionStoreError::Backend(e.to_string())
    }
}

/// [`SessionStore`] backend over a single SQLite database file.
///
/// The connection lives behind a `Mutex` because the trait requires
/// `Sync` while `rusqlite::Connection` is only `Send`; callers doing
/// blocking work off async runtimes should wrap calls in
/// `spawn_blocking` as with every other backend.
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (or create) the database at `db_path`, apply the schema
    /// idempotently, and enable WAL journaling plus foreign keys.
    pub fn open(db_path: impl Into<PathBuf>) -> Result<Self, SessionStoreError> {
        let path: PathBuf = db_path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        // journal_mode returns the resulting mode as a row; query it
        // explicitly instead of execute (which rejects returned rows).
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// [`Self::open`], then import every session found under
    /// `jsonl_dir` that is not already in the database. Import is
    /// idempotent (already-present ids are left untouched), skips
    /// files that fail the header scan or are corrupt beyond
    /// [`JsonlStore`]'s truncated-tail tolerance, and never modifies
    /// or deletes the JSONL files.
    pub fn open_with_import(
        db_path: impl Into<PathBuf>,
        jsonl_dir: &Path,
    ) -> Result<Self, SessionStoreError> {
        let store = Self::open(db_path)?;
        store.import_jsonl_dir(jsonl_dir)?;
        Ok(store)
    }

    fn import_jsonl_dir(&self, jsonl_dir: &Path) -> Result<(), SessionStoreError> {
        let jsonl = JsonlStore::new(jsonl_dir);
        // list() applies the bounded header scan and already skips
        // files that are not sessions; updated_ms is the file mtime.
        for summary in jsonl.list()? {
            // Full load through JsonlStore for its tail tolerance; a
            // file corrupt beyond the header is skipped, not an error.
            let Ok((header, entries)) = jsonl.load(&summary.header.id) else {
                continue;
            };
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            // INSERT OR IGNORE keys idempotency: ids already imported
            // (or created directly in the db) are left untouched, and
            // the no-op transaction rolls back on drop.
            if insert_session(&tx, &header, summary.updated_ms)? {
                insert_entries(&tx, &header.id, 1, &entries)?;
                tx.commit()?;
            }
        }
        Ok(())
    }
}

fn corrupt(session: &str, detail: impl Into<String>) -> SessionStoreError {
    SessionStoreError::Corrupt {
        session: session.to_string(),
        detail: detail.into(),
    }
}

/// Header for `session_id`, or NotFound. Touches only the sessions
/// table.
fn select_header(conn: &Connection, session_id: &str) -> Result<SessionHeader, SessionStoreError> {
    let header_json: Option<String> = conn
        .query_row(
            "SELECT header_json FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()?;
    match header_json {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| corrupt(session_id, format!("malformed stored header: {e}"))),
        None => Err(SessionStoreError::NotFound(session_id.to_string())),
    }
}

fn select_entries(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionEntry>, SessionStoreError> {
    let mut stmt =
        conn.prepare("SELECT kind, payload_json FROM entries WHERE session_id = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (kind, payload_json) = row?;
        let payload = serde_json::from_str(&payload_json)
            .map_err(|e| corrupt(session_id, format!("malformed stored entry payload: {e}")))?;
        entries.push(SessionEntry::new(kind, payload));
    }
    Ok(entries)
}

/// Insert a session row; `false` means the id already existed and
/// nothing was written (INSERT OR IGNORE).
fn insert_session(
    conn: &Connection,
    header: &SessionHeader,
    updated_ms: i64,
) -> Result<bool, SessionStoreError> {
    let header_json = serde_json::to_string(header)?;
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO sessions (id, header_json, created_ms, updated_ms, parent_session) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            header.id,
            header_json,
            header.timestamp,
            updated_ms,
            header.parent_session
        ],
    )?;
    Ok(inserted > 0)
}

fn insert_entries(
    conn: &Connection,
    session_id: &str,
    start_seq: i64,
    entries: &[SessionEntry],
) -> Result<(), SessionStoreError> {
    let mut stmt = conn.prepare(
        "INSERT INTO entries (session_id, seq, kind, payload_json) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (offset, entry) in entries.iter().enumerate() {
        stmt.execute(params![
            session_id,
            start_seq + offset as i64,
            entry.kind,
            serde_json::to_string(&entry.payload)?
        ])?;
    }
    Ok(())
}

impl SessionStore for SqliteStore {
    fn create(&self, header: &SessionHeader) -> Result<(), SessionStoreError> {
        let conn = self.conn.lock().unwrap();
        if !insert_session(&conn, header, header.timestamp)? {
            return Err(SessionStoreError::Invalid(format!(
                "session already exists: {}",
                header.id
            )));
        }
        Ok(())
    }

    fn append(&self, session_id: &str, entry: &SessionEntry) -> Result<(), SessionStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row("SELECT 1 FROM sessions WHERE id = ?1", [session_id], |_| {
                Ok(true)
            })
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(SessionStoreError::NotFound(session_id.to_string()));
        }
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM entries WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO entries (session_id, seq, kind, payload_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id,
                next_seq,
                entry.kind,
                serde_json::to_string(&entry.payload)?
            ],
        )?;
        // Same semantics as the in-memory backend: the entry's own
        // timestamp bumps updated_ms; entries without one keep it.
        if let Some(ts) = entry.timestamp() {
            tx.execute(
                "UPDATE sessions SET updated_ms = ?1 WHERE id = ?2",
                params![ts, session_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn read_header(&self, session_id: &str) -> Result<SessionHeader, SessionStoreError> {
        let conn = self.conn.lock().unwrap();
        select_header(&conn, session_id)
    }

    fn load(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeader, Vec<SessionEntry>), SessionStoreError> {
        let conn = self.conn.lock().unwrap();
        let header = select_header(&conn, session_id)?;
        let entries = select_entries(&conn, session_id)?;
        Ok((header, entries))
    }

    fn list(&self) -> Result<Vec<SessionSummary>, SessionStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT header_json, updated_ms FROM sessions ORDER BY updated_ms DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            let (header_json, updated_ms) = row?;
            // Best-effort like JsonlStore::list: one corrupt stored
            // header must not break listing the rest.
            let Ok(header) = serde_json::from_str::<SessionHeader>(&header_json) else {
                continue;
            };
            summaries.push(SessionSummary { header, updated_ms });
        }
        Ok(summaries)
    }

    fn fork(
        &self,
        from: &str,
        new_id: &str,
        timestamp: i64,
        up_to: Option<&str>,
    ) -> Result<SessionHeader, SessionStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let source_header = select_header(&tx, from)?;
        let source_entries = select_entries(&tx, from)?;
        let copied = entries_up_to(&source_entries, up_to)?;
        let new_header = forked_header(&source_header, new_id, timestamp);
        if !insert_session(&tx, &new_header, timestamp)? {
            return Err(SessionStoreError::Invalid(format!(
                "session already exists: {new_id}"
            )));
        }
        insert_entries(&tx, new_id, 1, &copied)?;
        tx.commit()?;
        Ok(new_header)
    }
}
