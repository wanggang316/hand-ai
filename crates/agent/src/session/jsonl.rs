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
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    /// Make the file end on a line boundary before anything is appended.
    ///
    /// A write interrupted mid-line — the process is killed, the disk
    /// fills — leaves a final line with no terminator. [`Self::load`]
    /// tolerates that: an unterminated line that fails to parse can only
    /// be the last one, so the complete prefix is returned and the
    /// session still opens.
    ///
    /// Appending onto it is what turns a survivable state into a fatal
    /// one. The new record fuses onto the fragment, and the resulting
    /// line *does* carry a terminator, so the next load reads it as a
    /// fully written malformed entry — corruption, which fails the whole
    /// session rather than one line.
    ///
    /// The trailing fragment is judged the same way the reader judges
    /// it:
    ///
    /// - It parses — a complete entry that merely lost its newline.
    ///   Terminate it and keep it.
    /// - It does not parse — a torn write the reader already refuses to
    ///   interpret. Drop it, which discards nothing that was ever
    ///   readable.
    ///
    /// Either way the file ends on a boundary and the append lands on
    /// its own line.
    fn terminate_torn_tail(path: &Path) -> Result<(), SessionStoreError> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(());
        }

        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] == b'\n' {
            return Ok(());
        }

        // Find where the unterminated fragment starts.
        let mut contents = String::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_string(&mut contents)?;
        let fragment_start = match contents.rfind('\n') {
            Some(i) => i + 1,
            None => 0,
        };
        let fragment = contents[fragment_start..].trim();

        if serde_json::from_str::<SessionEntry>(fragment).is_ok() {
            file.seek(SeekFrom::End(0))?;
            writeln!(file)?;
        } else {
            file.set_len(fragment_start as u64)?;
        }
        file.sync_all()?;
        Ok(())
    }

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
        Self::terminate_torn_tail(&path)?;
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
