//! Session management — JSONL-based session persistence.

use crate::core::error::CodingAgentError;
use chrono::Utc;
use model::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current session schema version. Bumped whenever the on-disk JSONL
/// shape changes in a way that requires migration. Mirrors
/// `CURRENT_SESSION_VERSION` in pi-mono's `core/session-manager.ts`.
///
/// Note: pi-mono is at v3 because it also does v1->v2 (add tree
/// structure) and v2->v3 (rename `hookMessage` role). The Rust port
/// chose the flat-list shape from the start (no per-entry `parent_id`),
/// so neither migration applies. The constant is exposed for callers
/// that want to stamp v3 headers consistently and for future
/// migrations.
pub const CURRENT_SESSION_VERSION: u32 = 3;

/// Session header (first line in JSONL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    pub version: u32,
    pub id: String,
    pub timestamp: i64,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// Entry types in a session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionEntry {
    Session(SessionHeader),
    Message {
        id: String,
        message: Box<Message>,
        timestamp: i64,
    },
    ModelChange {
        id: String,
        provider: String,
        model_id: String,
        timestamp: i64,
    },
    Compaction {
        id: String,
        summary: String,
        first_kept_entry_id: String,
        timestamp: i64,
    },
    Label {
        id: String,
        target_id: String,
        label: Option<String>,
        timestamp: i64,
    },
}

/// Summary information about a session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub id: String,
    pub cwd: String,
    pub timestamp: i64,
    pub message_count: usize,
}

/// Parse JSONL session content into entries. Malformed lines are
/// silently skipped (matches the TS `parseSessionEntries` behaviour:
/// best-effort tolerance for partially-corrupted files). The session
/// header line, when present, is included as `SessionEntry::Session`.
///
/// This standalone helper is exposed for testing and for callers that
/// need to inspect a session's raw entries without instantiating a
/// [`SessionManager`].
pub fn parse_session_entries(content: &str) -> Vec<SessionEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(line) {
            entries.push(entry);
        }
    }
    entries
}

/// Load JSONL entries from a file. Returns an empty vector when the
/// file is missing or has no valid session header (matching
/// `loadEntriesFromFile` in TS, which treats header-less files as
/// non-sessions).
///
/// I/O errors other than "not found" propagate as
/// [`CodingAgentError::Session`].
pub fn load_entries_from_file(path: &Path) -> Result<Vec<SessionEntry>, CodingAgentError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| CodingAgentError::Session(format!("Failed to read session: {}", e)))?;

    let entries = parse_session_entries(&content);

    // Validate that the first entry is a session header. Mirrors the TS
    // guard: corrupted or non-session files load as empty.
    match entries.first() {
        Some(SessionEntry::Session(_)) => Ok(entries),
        _ => Ok(Vec::new()),
    }
}

/// Find the most recent session file in `session_dir` by file mtime.
/// Returns `None` if the directory is missing, empty, or contains no
/// valid `.jsonl` session files (header validated cheaply by reading
/// the first line). Mirrors `findMostRecentSession` in TS.
pub fn find_most_recent_session(session_dir: &Path) -> Option<PathBuf> {
    let read_dir = std::fs::read_dir(session_dir).ok()?;

    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if !is_valid_session_file(&path) {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match &best {
            Some((_, best_mtime)) if mtime <= *best_mtime => {}
            _ => best = Some((path, mtime)),
        }
    }

    best.map(|(p, _)| p)
}

/// Cheap session-header check: read up to 512 bytes from the start of
/// the file and confirm the first line parses as a `SessionEntry::Session`.
/// Used by [`find_most_recent_session`] to skip corrupted / unrelated
/// `.jsonl` files without loading the whole content.
fn is_valid_session_file(path: &Path) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 512];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let head = match std::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let first_line = head.lines().next().unwrap_or("");
    matches!(
        serde_json::from_str::<SessionEntry>(first_line),
        Ok(SessionEntry::Session(_))
    )
}

/// Manages session files (JSONL format).
pub struct SessionManager {
    path: PathBuf,
    session_dir: PathBuf,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    in_memory: bool,
}

impl SessionManager {
    /// Create a new session file.
    pub fn create(cwd: &Path) -> Result<Self, CodingAgentError> {
        let session_dir = Self::default_session_dir(cwd);
        std::fs::create_dir_all(&session_dir)?;

        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: None,
        };

        let path = session_dir.join(format!("{}.jsonl", id));

        let mgr = Self {
            path: path.clone(),
            session_dir,
            header: header.clone(),
            entries: vec![SessionEntry::Session(header)],
            in_memory: false,
        };

        mgr.flush()?;
        Ok(mgr)
    }

    /// Open an existing session file.
    pub fn open(path: &Path) -> Result<Self, CodingAgentError> {
        let entries = load_entries_from_file(path)?;
        if entries.is_empty() {
            return Err(CodingAgentError::Session(format!(
                "No session header found in {}",
                path.display()
            )));
        }
        // load_entries_from_file guarantees entries[0] is a Session
        // header when the vec is non-empty.
        let header = match &entries[0] {
            SessionEntry::Session(h) => h.clone(),
            _ => {
                return Err(CodingAgentError::Session("No session header found".into()));
            }
        };

        let session_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        Ok(Self {
            path: path.to_path_buf(),
            session_dir,
            header,
            entries,
            in_memory: false,
        })
    }

    /// Create an in-memory (ephemeral) session.
    pub fn in_memory() -> Self {
        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id,
            timestamp: Utc::now().timestamp_millis(),
            cwd: ".".into(),
            parent_session: None,
        };
        Self {
            path: PathBuf::new(),
            session_dir: PathBuf::new(),
            header: header.clone(),
            entries: vec![SessionEntry::Session(header)],
            in_memory: true,
        }
    }

    /// Get the session ID.
    pub fn id(&self) -> &str {
        &self.header.id
    }

    /// Borrow the entry list. Used by callers that need to inspect raw
    /// JSONL entries (e.g. [`crate::core::agent_session::AgentSession::fork`]
    /// to look up a message by `entry_id`).
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    /// Build a fresh session manager that adopts the given body entries
    /// verbatim under a freshly-generated session header. Used by
    /// `AgentSession::fork` and `AgentSession::clone_session` to create
    /// the replacement session — the body entries (messages,
    /// model-changes, compactions, labels) keep their original IDs so
    /// that internal cross-references (e.g. `Compaction::first_kept_entry_id`)
    /// remain valid after the branch.
    ///
    /// `body_entries` must be free of `SessionEntry::Session` headers;
    /// the new header is generated here.
    ///
    /// `parent_id` is recorded on the new header (when present) for
    /// provenance, mirroring [`Self::fork_from`].
    pub fn from_branched_entries(
        cwd: &Path,
        in_memory: bool,
        parent_id: Option<&str>,
        body_entries: Vec<SessionEntry>,
    ) -> Result<Self, CodingAgentError> {
        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: parent_id.map(|s| s.to_string()),
        };

        let mut entries = Vec::with_capacity(body_entries.len() + 1);
        entries.push(SessionEntry::Session(header.clone()));
        entries.extend(body_entries);

        if in_memory {
            return Ok(Self {
                path: PathBuf::new(),
                session_dir: PathBuf::new(),
                header,
                entries,
                in_memory: true,
            });
        }

        let session_dir = Self::default_session_dir(cwd);
        std::fs::create_dir_all(&session_dir)?;
        let path = session_dir.join(format!("{}.jsonl", id));

        let mgr = Self {
            path,
            session_dir,
            header,
            entries,
            in_memory: false,
        };
        mgr.flush()?;
        Ok(mgr)
    }

    /// Whether this session manager is purely in-memory (no JSONL file
    /// backing it). Used by callers like
    /// [`crate::core::agent_session::AgentSession::reset_session`] to pick
    /// the right constructor for the replacement manager — an in-memory
    /// session must reset to an in-memory session, otherwise we would
    /// suddenly try to write `./.hand/sessions/*.jsonl` from a test.
    pub fn is_in_memory(&self) -> bool {
        self.in_memory
    }

    /// Get the session file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the session directory.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Append a message entry.
    pub fn append_message(&mut self, message: Message) -> Result<String, CodingAgentError> {
        let id = generate_entry_id();
        let entry = SessionEntry::Message {
            id: id.clone(),
            message: Box::new(message),
            timestamp: Utc::now().timestamp_millis(),
        };
        self.entries.push(entry);
        if !self.in_memory {
            self.append_to_file(self.entries.last().unwrap())?;
        }
        Ok(id)
    }

    /// Append a model change entry.
    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<(), CodingAgentError> {
        let entry = SessionEntry::ModelChange {
            id: generate_entry_id(),
            provider: provider.into(),
            model_id: model_id.into(),
            timestamp: Utc::now().timestamp_millis(),
        };
        self.entries.push(entry);
        if !self.in_memory {
            self.append_to_file(self.entries.last().unwrap())?;
        }
        Ok(())
    }

    /// Append a compaction entry.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
    ) -> Result<(), CodingAgentError> {
        let entry = SessionEntry::Compaction {
            id: generate_entry_id(),
            summary: summary.into(),
            first_kept_entry_id: first_kept_entry_id.into(),
            timestamp: Utc::now().timestamp_millis(),
        };
        self.entries.push(entry);
        if !self.in_memory {
            self.append_to_file(self.entries.last().unwrap())?;
        }
        Ok(())
    }

    /// Append a label entry (session name).
    pub fn append_label(&mut self, label: &str) -> Result<(), CodingAgentError> {
        let entry = SessionEntry::Label {
            id: generate_entry_id(),
            target_id: self.header.id.clone(),
            label: Some(label.into()),
            timestamp: Utc::now().timestamp_millis(),
        };
        self.entries.push(entry);
        if !self.in_memory {
            self.append_to_file(self.entries.last().unwrap())?;
        }
        Ok(())
    }

    /// Get the session label (name), if any.
    pub fn label(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|e| {
            if let SessionEntry::Label { label, .. } = e {
                label.as_deref()
            } else {
                None
            }
        })
    }

    /// Build the message list for LLM context.
    pub fn build_context(&self) -> Vec<Message> {
        // Find the latest compaction and start from there
        let start_id = self.entries.iter().rev().find_map(|e| {
            if let SessionEntry::Compaction {
                first_kept_entry_id,
                ..
            } = e
            {
                Some(first_kept_entry_id.clone())
            } else {
                None
            }
        });

        let mut messages = Vec::new();
        let mut found_start = start_id.is_none();

        for entry in &self.entries {
            if let SessionEntry::Message { id, message, .. } = entry {
                if !found_start {
                    if Some(id.as_str()) == start_id.as_deref() {
                        found_start = true;
                    } else {
                        continue;
                    }
                }
                messages.push((**message).clone());
            }
        }

        messages
    }

    /// Get message count.
    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::Message { .. }))
            .count()
    }

    /// Continue the most recent session in the given working directory.
    pub fn continue_recent(cwd: &Path) -> Result<Self, CodingAgentError> {
        let sessions = Self::list(cwd)?;
        let most_recent = sessions
            .into_iter()
            .next()
            .ok_or_else(|| CodingAgentError::Session("No sessions found to continue".into()))?;
        Self::open(&most_recent.path)
    }

    /// Fork a session from an existing session file.
    pub fn fork_from(source_path: &Path, cwd: &Path) -> Result<Self, CodingAgentError> {
        let source = Self::open(source_path)?;
        let session_dir = Self::default_session_dir(cwd);
        std::fs::create_dir_all(&session_dir)?;

        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: Some(source.header.id.clone()),
        };

        let mut entries = vec![SessionEntry::Session(header.clone())];
        // Copy all message entries from source
        for entry in &source.entries {
            if let SessionEntry::Message { message, .. } = entry {
                entries.push(SessionEntry::Message {
                    id: generate_entry_id(),
                    message: message.clone(),
                    timestamp: Utc::now().timestamp_millis(),
                });
            }
        }

        let path = session_dir.join(format!("{}.jsonl", id));

        let mgr = Self {
            path: path.clone(),
            session_dir,
            header,
            entries,
            in_memory: false,
        };

        mgr.flush()?;
        Ok(mgr)
    }

    /// Get the session display name or ID.
    pub fn display_name(&self) -> &str {
        &self.header.id
    }

    /// Get the session header.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// List all sessions in a directory.
    pub fn list(cwd: &Path) -> Result<Vec<SessionInfo>, CodingAgentError> {
        let session_dir = Self::default_session_dir(cwd);
        if !session_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&session_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl")
                && let Ok(mgr) = Self::open(&path)
            {
                sessions.push(SessionInfo {
                    path: path.clone(),
                    id: mgr.header.id.clone(),
                    cwd: mgr.header.cwd.clone(),
                    timestamp: mgr.header.timestamp,
                    message_count: mgr.message_count(),
                });
            }
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.timestamp));
        Ok(sessions)
    }

    fn default_session_dir(cwd: &Path) -> PathBuf {
        cwd.join(".hand").join("sessions")
    }

    fn flush(&self) -> Result<(), CodingAgentError> {
        if self.in_memory {
            return Ok(());
        }
        let mut content = String::new();
        for entry in &self.entries {
            content.push_str(&serde_json::to_string(entry)?);
            content.push('\n');
        }
        std::fs::write(&self.path, content)?;
        Ok(())
    }

    fn append_to_file(&self, entry: &SessionEntry) -> Result<(), CodingAgentError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let json = serde_json::to_string(entry)?;
        writeln!(file, "{}", json)?;
        Ok(())
    }
}

fn generate_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("s_{:x}_{:x}", ts, c)
}

fn generate_entry_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("e_{:x}_{:x}", ts, c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::UserMessage;
    use tempfile::TempDir;

    #[test]
    fn test_session_in_memory() {
        let mut mgr = SessionManager::in_memory();
        assert!(mgr.id().starts_with("s_"));
        assert_eq!(mgr.message_count(), 0);

        let msg = Message::User(UserMessage::new_text("hello"));
        mgr.append_message(msg).unwrap();
        assert_eq!(mgr.message_count(), 1);
    }

    #[test]
    fn test_session_build_context() {
        let mut mgr = SessionManager::in_memory();
        mgr.append_message(Message::User(UserMessage::new_text("msg1")))
            .unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("msg2")))
            .unwrap();

        let context = mgr.build_context();
        assert_eq!(context.len(), 2);
    }

    #[test]
    fn test_session_create_and_open() {
        let dir = TempDir::new().unwrap();
        let mgr = SessionManager::create(dir.path()).unwrap();
        let id = mgr.id().to_string();
        let path = mgr.path().to_path_buf();

        let opened = SessionManager::open(&path).unwrap();
        assert_eq!(opened.id(), id);
    }

    #[test]
    fn test_session_persist_messages() {
        let dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create(dir.path()).unwrap();

        mgr.append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("world")))
            .unwrap();

        let path = mgr.path().to_path_buf();
        let loaded = SessionManager::open(&path).unwrap();
        assert_eq!(loaded.message_count(), 2);
    }

    #[test]
    fn test_session_list() {
        let dir = TempDir::new().unwrap();
        SessionManager::create(dir.path()).unwrap();
        SessionManager::create(dir.path()).unwrap();

        let sessions = SessionManager::list(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_session_compaction() {
        let mut mgr = SessionManager::in_memory();
        let _id1 = mgr
            .append_message(Message::User(UserMessage::new_text("old")))
            .unwrap();
        let id2 = mgr
            .append_message(Message::User(UserMessage::new_text("new")))
            .unwrap();

        mgr.append_compaction("Summary of old messages", &id2)
            .unwrap();

        let context = mgr.build_context();
        // After compaction, only messages from id2 onwards
        assert_eq!(context.len(), 1);
    }

    #[test]
    fn test_entry_id_generation() {
        let id1 = generate_entry_id();
        let id2 = generate_entry_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("e_"));
    }

    #[test]
    fn test_parse_session_entries_skips_blank_and_malformed() {
        // Build a valid header line, then add blank + garbage in the middle
        let header = SessionEntry::Session(SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: "s_1".into(),
            timestamp: 1,
            cwd: "/x".into(),
            parent_session: None,
        });
        let header_line = serde_json::to_string(&header).unwrap();

        let content = format!("{}\n\n   \nnot json\n{}\n", header_line, header_line);
        let parsed = parse_session_entries(&content);

        // blank lines + "not json" skipped, two valid headers kept
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], SessionEntry::Session(_)));
    }

    #[test]
    fn test_load_entries_from_file_missing_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let entries = load_entries_from_file(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_load_entries_from_file_no_header_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("headerless.jsonl");
        // Valid JSONL but no Session header up front
        std::fs::write(
            &path,
            "{\"type\":\"message\",\"data\":{\"id\":\"e_1\",\"message\":{\"role\":\"user\",\"content\":\"hi\"},\"timestamp\":1}}\n",
        )
        .unwrap();

        let entries = load_entries_from_file(&path).unwrap();
        // TS guard: header-less files are treated as non-sessions.
        assert!(entries.is_empty());
    }

    #[test]
    fn test_load_entries_from_file_round_trips_real_session() {
        let dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        let path = mgr.path().to_path_buf();

        let entries = load_entries_from_file(&path).unwrap();
        assert!(matches!(entries[0], SessionEntry::Session(_)));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, SessionEntry::Message { .. }))
        );
    }

    #[test]
    fn test_find_most_recent_session_picks_latest_mtime() {
        let dir = TempDir::new().unwrap();
        let older = SessionManager::create(dir.path()).unwrap();
        // Tiny sleep so mtimes differ even on coarse filesystems.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = SessionManager::create(dir.path()).unwrap();

        let found = find_most_recent_session(&dir.path().join(".hand").join("sessions"))
            .expect("should find a session");
        assert_eq!(found, newer.path());
        assert_ne!(found, older.path());
    }

    #[test]
    fn test_find_most_recent_session_skips_invalid_files() {
        let dir = TempDir::new().unwrap();
        let mgr = SessionManager::create(dir.path()).unwrap();
        let session_dir = dir.path().join(".hand").join("sessions");

        // Add a stray `.jsonl` file that has no session header — should be ignored.
        std::fs::write(
            session_dir.join("garbage.jsonl"),
            "this is definitely not jsonl\n",
        )
        .unwrap();

        let found = find_most_recent_session(&session_dir).expect("session present");
        assert_eq!(found, mgr.path());
    }

    #[test]
    fn test_find_most_recent_session_missing_dir() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nope");
        assert!(find_most_recent_session(&missing).is_none());
    }

    #[test]
    fn test_current_session_version_constant() {
        // Sanity: stamp the current value so a change is intentional.
        assert_eq!(CURRENT_SESSION_VERSION, 3);
        let mgr = SessionManager::in_memory();
        assert_eq!(mgr.header().version, CURRENT_SESSION_VERSION);
    }
}
