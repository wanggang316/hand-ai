//! JSONL session storage — one `<dir>/<session_id>.jsonl` file per
//! session, format-compatible with the session files written by the
//! hand binary.
//!
//! Layout: the first line is the header envelope
//! `{"type":"session","data":{<header fields>}}`; every subsequent
//! line is one entry envelope. Appends go through
//! `OpenOptions::append`, so concurrent readers only ever see whole
//! lines plus at most one trailing partial line (a truncated write),
//! which [`JsonlStore::load`] tolerates.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use super::store::{SessionStore, entries_up_to, forked_header};
use super::types::{
    NO_HEADER_DETAIL, SessionEntry, SessionHeader, SessionStoreError, SessionSummary,
};

/// Byte cap for the bounded header scan in [`JsonlStore::read_header`]
/// and [`JsonlStore::list`]. Generous enough for a header line with a
/// long cwd, small enough that listing a directory of multi-megabyte
/// session files stays proportional to the number of files, not their
/// sizes.
const MAX_HEADER_SCAN_BYTES: u64 = 64 * 1024;

/// [`SessionStore`] backend over a directory of `.jsonl` files.
pub struct JsonlStore {
    dir: PathBuf,
}

impl JsonlStore {
    /// Build a store rooted at `dir`. The directory is created lazily
    /// on the first write; listing a never-written store yields an
    /// empty vec.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.jsonl"))
    }

    fn header_envelope(header: &SessionHeader) -> Result<SessionEntry, SessionStoreError> {
        Ok(SessionEntry::new("session", serde_json::to_value(header)?))
    }

    /// Bounded header scan: read at most [`MAX_HEADER_SCAN_BYTES`] from
    /// the start of the file, skip leading blank lines, and require the
    /// first non-blank line to be a parseable session envelope. A line
    /// the cap truncated fails to parse and lands in the Corrupt arm,
    /// so oversized headers error instead of hanging on a full read.
    fn scan_header(path: &Path, session: &str) -> Result<SessionHeader, SessionStoreError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file.take(MAX_HEADER_SCAN_BYTES));
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err(corrupt(session, NO_HEADER_DETAIL));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: SessionEntry = serde_json::from_str(trimmed).map_err(|e| {
                corrupt(
                    session,
                    format!("first line is not a session envelope: {e}"),
                )
            })?;
            return header_from_envelope(&entry, session);
        }
    }
}

fn corrupt(session: &str, detail: impl Into<String>) -> SessionStoreError {
    SessionStoreError::Corrupt {
        session: session.to_string(),
        detail: detail.into(),
    }
}

fn header_from_envelope(
    entry: &SessionEntry,
    session: &str,
) -> Result<SessionHeader, SessionStoreError> {
    if !entry.is_header() {
        return Err(corrupt(
            session,
            format!("first line has kind '{}', expected 'session'", entry.kind),
        ));
    }
    serde_json::from_value(entry.payload.clone())
        .map_err(|e| corrupt(session, format!("malformed session header: {e}")))
}

/// Session ids double as file names; refuse anything that could escape
/// the store directory.
fn validate_session_id(id: &str) -> Result<(), SessionStoreError> {
    if id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\\') {
        return Err(SessionStoreError::Invalid(format!(
            "invalid session id: '{id}'"
        )));
    }
    Ok(())
}

/// Map io NotFound onto the store's NotFound so callers see a session
/// error, not a filesystem detail.
fn open_session_file(path: &Path, session: &str) -> Result<File, SessionStoreError> {
    match File::open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(SessionStoreError::NotFound(session.to_string()))
        }
        Err(e) => Err(e.into()),
    }
}

impl SessionStore for JsonlStore {
    fn create(&self, header: &SessionHeader) -> Result<(), SessionStoreError> {
        validate_session_id(&header.id)?;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.session_path(&header.id);
        let mut file = match OpenOptions::new().create_new(true).append(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SessionStoreError::Invalid(format!(
                    "session already exists: {}",
                    header.id
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let line = serde_json::to_string(&Self::header_envelope(header)?)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    fn append(&self, session_id: &str, entry: &SessionEntry) -> Result<(), SessionStoreError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(SessionStoreError::NotFound(session_id.to_string()));
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    fn read_header(&self, session_id: &str) -> Result<SessionHeader, SessionStoreError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(SessionStoreError::NotFound(session_id.to_string()));
        }
        Self::scan_header(&path, session_id)
    }

    fn load(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeader, Vec<SessionEntry>), SessionStoreError> {
        let path = self.session_path(session_id);
        let file = open_session_file(&path, session_id)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut line_no = 0usize;
        let mut header: Option<SessionHeader> = None;
        let mut entries = Vec::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            line_no += 1;
            let complete = line.ends_with('\n');
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEntry>(trimmed) {
                Ok(entry) => match header {
                    None => header = Some(header_from_envelope(&entry, session_id)?),
                    Some(_) => entries.push(entry),
                },
                // A line without a trailing newline is a truncated
                // write; it can only be the file's last line, so stop
                // and return the complete prefix. A malformed line that
                // was fully written (newline present) is corruption.
                Err(e) => {
                    if complete {
                        return Err(corrupt(
                            session_id,
                            format!("malformed entry at line {line_no}: {e}"),
                        ));
                    }
                    break;
                }
            }
        }
        let header = header.ok_or_else(|| corrupt(session_id, NO_HEADER_DETAIL))?;
        Ok((header, entries))
    }

    fn list(&self) -> Result<Vec<SessionSummary>, SessionStoreError> {
        let read_dir = match std::fs::read_dir(&self.dir) {
            Ok(read_dir) => read_dir,
            // Lazily-created dir: nothing written yet means no sessions.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut summaries = Vec::new();
        for dent in read_dir.flatten() {
            let path = dent.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            // Best-effort scan: one unreadable or non-session file must
            // not break listing the rest.
            let Ok(header) = Self::scan_header(&path, "") else {
                continue;
            };
            let updated_ms = dent
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(header.timestamp);
            summaries.push(SessionSummary { header, updated_ms });
        }
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
        validate_session_id(new_id)?;
        let (source_header, source_entries) = self.load(from)?;
        let copied = entries_up_to(&source_entries, up_to)?;
        let new_header = forked_header(&source_header, new_id, timestamp);

        std::fs::create_dir_all(&self.dir)?;
        let path = self.session_path(new_id);
        let mut file = match OpenOptions::new().create_new(true).append(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SessionStoreError::Invalid(format!(
                    "session already exists: {new_id}"
                )));
            }
            Err(e) => return Err(e.into()),
        };

        let mut content = serde_json::to_string(&Self::header_envelope(&new_header)?)?;
        content.push('\n');
        for entry in &copied {
            content.push_str(&serde_json::to_string(entry)?);
            content.push('\n');
        }
        file.write_all(content.as_bytes())?;
        Ok(new_header)
    }
}
