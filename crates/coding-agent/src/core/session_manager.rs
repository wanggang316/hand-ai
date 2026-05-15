//! Session management — JSONL-based session persistence.

use crate::core::error::CodingAgentError;
use chrono::Utc;
use model::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current session schema version. Bumped whenever the on-disk JSONL
/// shape changes in a way that requires migration.
///
/// The version is stamped at `3` from the start; the project never
/// shipped a v1 (pre-tree) or v2 (pre-`hookMessage` rename) on-disk
/// layout, so no in-place migration logic is needed today. The
/// constant is exposed so callers can stamp v3 headers consistently
/// and so future bumps have an obvious focal point.
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
///
/// The on-disk shape uses `{"type": <tag>, "data": {...}}` — an
/// envelope established before the entry-tree port. The envelope is
/// retained for backwards compatibility with already-written `.jsonl`
/// files; cross-implementation interop with other readers is tracked
/// separately.
///
/// Every variant carries `parent_id: Option<String>`, deserialized
/// with `#[serde(default)]` so older fixtures (written before
/// parent-id landed) still parse. The field is `null` for tree roots
/// and for flat-list sessions that never tracked parentage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionEntry {
    Session(SessionHeader),
    Message {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        message: Box<Message>,
        timestamp: i64,
    },
    ModelChange {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        provider: String,
        model_id: String,
        timestamp: i64,
    },
    Compaction {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        summary: String,
        first_kept_entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        timestamp: i64,
    },
    Label {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        target_id: String,
        label: Option<String>,
        timestamp: i64,
    },
    /// Branch summary entry — see TS `BranchSummaryEntry`.
    /// Pi-generated when `from_hook` is `None`/`Some(false)`.
    BranchSummary {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        from_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        timestamp: i64,
    },
    /// Custom message entry — extension-injected message that DOES
    /// participate in LLM context. Mirrors TS `CustomMessageEntry`.
    CustomMessage {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        custom_type: String,
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        display: bool,
        timestamp: i64,
    },
    /// Custom (opaque) entry — extension state that does NOT participate
    /// in LLM context. Mirrors TS `CustomEntry`.
    Custom {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        custom_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        timestamp: i64,
    },
    /// Bash command execution recorded via the interactive `!` prefix.
    /// Stored as a dedicated variant (not wrapped in `Custom`) so call
    /// sites can pattern-match without parsing JSON. The TS reference
    /// stores this on the `message` of a regular message entry, but a
    /// dedicated variant is closer to existing Rust call sites and
    /// keeps the data typed.
    BashExecution {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        command: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        timestamp: i64,
    },
    /// Thinking-level change entry — TS `ThinkingLevelChangeEntry`.
    ThinkingLevelChange {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        thinking_level: String,
        timestamp: i64,
    },
    /// Session metadata entry (display name) — TS `SessionInfoEntry`.
    SessionInfo {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        timestamp: i64,
    },
}

impl SessionEntry {
    /// Return the entry id, if any. The session-header variant has no
    /// id (it carries the session id on its own struct).
    ///
    /// Mirrors TS `entry.id` access patterns.
    pub fn id(&self) -> Option<&str> {
        match self {
            SessionEntry::Session(_) => None,
            SessionEntry::Message { id, .. }
            | SessionEntry::ModelChange { id, .. }
            | SessionEntry::Compaction { id, .. }
            | SessionEntry::Label { id, .. }
            | SessionEntry::BranchSummary { id, .. }
            | SessionEntry::CustomMessage { id, .. }
            | SessionEntry::Custom { id, .. }
            | SessionEntry::BashExecution { id, .. }
            | SessionEntry::ThinkingLevelChange { id, .. }
            | SessionEntry::SessionInfo { id, .. } => Some(id),
        }
    }

    /// Return the parent id, if any. Always `None` for the session
    /// header (and for flat-list sessions that never tracked parentage).
    pub fn parent_id(&self) -> Option<&str> {
        match self {
            SessionEntry::Session(_) => None,
            SessionEntry::Message { parent_id, .. }
            | SessionEntry::ModelChange { parent_id, .. }
            | SessionEntry::Compaction { parent_id, .. }
            | SessionEntry::Label { parent_id, .. }
            | SessionEntry::BranchSummary { parent_id, .. }
            | SessionEntry::CustomMessage { parent_id, .. }
            | SessionEntry::Custom { parent_id, .. }
            | SessionEntry::BashExecution { parent_id, .. }
            | SessionEntry::ThinkingLevelChange { parent_id, .. }
            | SessionEntry::SessionInfo { parent_id, .. } => parent_id.as_deref(),
        }
    }

    /// Return the entry timestamp (millis since epoch), if any. The
    /// session-header variant carries its own timestamp on the struct.
    pub fn timestamp(&self) -> Option<i64> {
        match self {
            SessionEntry::Session(h) => Some(h.timestamp),
            SessionEntry::Message { timestamp, .. }
            | SessionEntry::ModelChange { timestamp, .. }
            | SessionEntry::Compaction { timestamp, .. }
            | SessionEntry::Label { timestamp, .. }
            | SessionEntry::BranchSummary { timestamp, .. }
            | SessionEntry::CustomMessage { timestamp, .. }
            | SessionEntry::Custom { timestamp, .. }
            | SessionEntry::BashExecution { timestamp, .. }
            | SessionEntry::ThinkingLevelChange { timestamp, .. }
            | SessionEntry::SessionInfo { timestamp, .. } => Some(*timestamp),
        }
    }
}

/// Summary information about a session, suitable for listing UI and
/// search. Carries the fields a flat-list session can populate
/// (id, first message, mtime, message count).
///
/// Construction is internal; consumers should treat new fields as
/// additive — please go through [`SessionManager::list`] /
/// [`SessionManager::list_all`] rather than building one by hand.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub id: String,
    pub cwd: String,
    /// Header timestamp (creation time), millis since epoch.
    pub timestamp: i64,
    /// File mtime in millis since epoch — used as the listing sort key
    /// (latest activity first), with the header timestamp as fallback.
    /// Mirrors `SessionInfo.modified` in TS, which prefers the latest
    /// message timestamp and falls back to file mtime.
    pub modified: i64,
    pub message_count: usize,
    /// Latest non-empty session label (if any). Mirrors
    /// `SessionInfo.name`. The Rust port stores labels as `Label`
    /// entries with `target_id == header.id` (session-level naming).
    pub name: Option<String>,
    /// Path to the parent session, for forks. Lifted from the header.
    pub parent_session_path: Option<String>,
    /// First user-message text found in the session, or
    /// `"(no messages)"` for empty sessions. Used as a list-row
    /// preview.
    pub first_message: String,
    /// All user/assistant text concatenated with single spaces. Used
    /// as the haystack for free-text search ([`SessionInfo::matches`]).
    pub all_messages_text: String,
}

impl SessionInfo {
    /// Case-insensitive substring search across `name`, `first_message`,
    /// and `all_messages_text`. Empty queries match everything.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        if self
            .name
            .as_deref()
            .map(|n| n.to_lowercase().contains(&q))
            .unwrap_or(false)
        {
            return true;
        }
        if self.first_message.to_lowercase().contains(&q) {
            return true;
        }
        self.all_messages_text.to_lowercase().contains(&q)
    }
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

/// Build a [`SessionInfo`] from a session file by loading the
/// entries, scanning for the latest label and the first user message,
/// and concatenating user/assistant text for search.
///
/// Returns `Ok(None)` for files that have no valid header (so the
/// caller can `flatten` over a directory listing). I/O errors
/// propagate; malformed JSONL lines are tolerated by
/// [`parse_session_entries`].
pub fn build_session_info(path: &Path) -> Result<Option<SessionInfo>, CodingAgentError> {
    let entries = load_entries_from_file(path)?;
    if entries.is_empty() {
        return Ok(None);
    }
    let header = match &entries[0] {
        SessionEntry::Session(h) => h.clone(),
        _ => return Ok(None),
    };

    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(header.timestamp);

    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_message_timestamp: Option<i64> = None;

    for entry in &entries {
        match entry {
            SessionEntry::Label {
                target_id, label, ..
            } if target_id == &header.id => {
                // Latest label wins, including explicit clears (None).
                name = label
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
            SessionEntry::Message {
                message, timestamp, ..
            } => {
                message_count += 1;
                last_message_timestamp = Some(
                    last_message_timestamp
                        .map(|prev| prev.max(*timestamp))
                        .unwrap_or(*timestamp),
                );

                let text = extract_message_text(message);
                if text.is_empty() {
                    continue;
                }
                if first_message.is_empty()
                    && let Message::User(_) = message.as_ref()
                {
                    first_message = text.clone();
                }
                all_messages.push(text);
            }
            _ => {}
        }
    }

    // Prefer latest message timestamp when present (closer to "last
    // activity"), falling back to file mtime, finally header
    // timestamp. Mirrors `getSessionModifiedDate` in TS.
    let modified = last_message_timestamp.unwrap_or(mtime);

    Ok(Some(SessionInfo {
        path: path.to_path_buf(),
        id: header.id,
        cwd: header.cwd,
        timestamp: header.timestamp,
        modified,
        message_count,
        name,
        parent_session_path: header.parent_session,
        first_message: if first_message.is_empty() {
            "(no messages)".to_string()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    }))
}

/// List session info for every valid `.jsonl` file directly under
/// `dir`. Returns an empty vec if `dir` doesn't exist. Used by
/// [`SessionManager::list`] / [`SessionManager::list_all`].
fn list_sessions_from_dir(dir: &Path) -> Result<Vec<SessionInfo>, CodingAgentError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(Some(info)) = build_session_info(&path) {
            out.push(info);
        }
    }
    Ok(out)
}

/// Best-effort plain-text extraction from a [`Message`] for search /
/// preview purposes. ToolResult messages are skipped (they're noisy
/// and not user-facing); user/assistant text and thinking blocks are
/// concatenated with single spaces.
fn extract_message_text(message: &Message) -> String {
    match message {
        Message::User(u) => match &u.content {
            model::UserContent::Text(s) => s.clone(),
            model::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    model::UserContentBlock::Text(t) => Some(t.text.as_str()),
                    model::UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        },
        Message::Assistant(a) => a
            .content
            .iter()
            .filter_map(|b| match b {
                model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                model::AssistantContentBlock::Thinking(_) => None,
                model::AssistantContentBlock::ToolCall(_) => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        Message::ToolResult(_) => String::new(),
    }
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
    /// Create a new session file under the default
    /// `<cwd>/.hand/sessions` directory.
    pub fn create(cwd: &Path) -> Result<Self, CodingAgentError> {
        Self::create_in(cwd, &Self::default_session_dir(cwd))
    }

    /// Create a new session file under an explicit session directory.
    /// Used by callers that pass `--session-dir`; the directory is
    /// created if it doesn't exist.
    pub fn create_in(cwd: &Path, session_dir: &Path) -> Result<Self, CodingAgentError> {
        let session_dir = session_dir.to_path_buf();
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
            parent_id: None,
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
            parent_id: None,
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
            parent_id: None,
            summary: summary.into(),
            first_kept_entry_id: first_kept_entry_id.into(),
            tokens_before: None,
            details: None,
            from_hook: None,
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
            parent_id: None,
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

    /// Fork a session from an existing session file: produce a new
    /// session in `cwd`'s session dir whose header points at the source
    /// session as `parent_session`, carrying every non-header entry
    /// from the source verbatim.
    ///
    /// The new session gets a freshly-generated id, but body entries
    /// (messages, compactions, model changes, labels) keep their
    /// original ids and timestamps so cross-references like
    /// `Compaction::first_kept_entry_id` and `Label::target_id` remain
    /// valid after the fork.
    pub fn fork_from(source_path: &Path, cwd: &Path) -> Result<Self, CodingAgentError> {
        let source_entries = load_entries_from_file(source_path)?;
        if source_entries.is_empty() {
            return Err(CodingAgentError::Session(format!(
                "Cannot fork: source session is empty or has no header: {}",
                source_path.display()
            )));
        }

        let source_header = match &source_entries[0] {
            SessionEntry::Session(h) => h.clone(),
            _ => {
                return Err(CodingAgentError::Session(format!(
                    "Cannot fork: source session has no header: {}",
                    source_path.display()
                )));
            }
        };

        let session_dir = Self::default_session_dir(cwd);
        std::fs::create_dir_all(&session_dir)?;

        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: Some(source_header.id.clone()),
        };

        // Preserve every non-header entry from the source verbatim —
        // ids included, so downstream cross-references stay valid.
        let mut entries = Vec::with_capacity(source_entries.len());
        entries.push(SessionEntry::Session(header.clone()));
        for entry in source_entries.into_iter().skip(1) {
            if matches!(entry, SessionEntry::Session(_)) {
                continue;
            }
            entries.push(entry);
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

    /// List all sessions for `cwd`'s default session directory. Sorted
    /// by `modified` descending (most recently active first), matching
    /// the TS `SessionManager.list` ordering.
    ///
    /// Malformed / header-less `.jsonl` files in the directory are
    /// silently skipped.
    pub fn list(cwd: &Path) -> Result<Vec<SessionInfo>, CodingAgentError> {
        let session_dir = Self::default_session_dir(cwd);
        let mut sessions = list_sessions_from_dir(&session_dir)?;
        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
        Ok(sessions)
    }

    /// List sessions across every project directory under `root`.
    /// Mirrors `SessionManager.listAll` in TS, scoped to the directory
    /// layout of the Rust port: each `cwd` keeps its sessions under
    /// `<cwd>/.hand/sessions/`, so callers pass a parent directory
    /// containing one or more such project trees, and `list_all`
    /// recurses one level to find the per-project session dirs.
    ///
    /// Concretely, `list_all` looks for both:
    ///   - `<root>/.hand/sessions/*.jsonl` (root itself is a project)
    ///   - `<root>/<child>/.hand/sessions/*.jsonl` (one level down)
    ///
    /// Sorted by `modified` descending. Missing directories yield an
    /// empty list (not an error).
    pub fn list_all(root: &Path) -> Result<Vec<SessionInfo>, CodingAgentError> {
        let mut sessions = Vec::new();

        // root itself, if it's a project dir
        sessions.extend(list_sessions_from_dir(
            &root.join(".hand").join("sessions"),
        )?);

        // one level of children
        if let Ok(read_dir) = std::fs::read_dir(root) {
            for entry in read_dir.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let child_session_dir = entry.path().join(".hand").join("sessions");
                if !child_session_dir.is_dir() {
                    continue;
                }
                sessions.extend(list_sessions_from_dir(&child_session_dir)?);
            }
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
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
    fn test_fork_from_preserves_all_non_header_entries() {
        let dir = TempDir::new().unwrap();
        let mut source = SessionManager::create(dir.path()).unwrap();
        let msg_id = source
            .append_message(Message::User(UserMessage::new_text("first")))
            .unwrap();
        source.append_model_change("openai", "gpt-x").unwrap();
        source
            .append_compaction("rolled-up history", &msg_id)
            .unwrap();
        source.append_label("named").unwrap();
        let source_path = source.path().to_path_buf();
        let source_id = source.id().to_string();

        let target_dir = TempDir::new().unwrap();
        let forked = SessionManager::fork_from(&source_path, target_dir.path()).unwrap();

        // Header references source by id.
        assert_eq!(
            forked.header().parent_session.as_deref(),
            Some(source_id.as_str())
        );
        // New id, not the source's.
        assert_ne!(forked.id(), source_id);

        // Body entries: every variant preserved with original ids.
        let mut saw_message_with_orig_id = false;
        let mut saw_model_change = false;
        let mut saw_compaction = false;
        let mut saw_label = false;
        for entry in forked.entries() {
            match entry {
                SessionEntry::Session(_) => {}
                SessionEntry::Message { id, .. } => {
                    if id == &msg_id {
                        saw_message_with_orig_id = true;
                    }
                }
                SessionEntry::ModelChange { provider, .. } => {
                    if provider == "openai" {
                        saw_model_change = true;
                    }
                }
                SessionEntry::Compaction {
                    summary,
                    first_kept_entry_id,
                    ..
                } => {
                    if summary == "rolled-up history" && first_kept_entry_id == &msg_id {
                        saw_compaction = true;
                    }
                }
                SessionEntry::Label { label, .. } => {
                    if label.as_deref() == Some("named") {
                        saw_label = true;
                    }
                }
                SessionEntry::BranchSummary { .. }
                | SessionEntry::CustomMessage { .. }
                | SessionEntry::Custom { .. }
                | SessionEntry::BashExecution { .. }
                | SessionEntry::ThinkingLevelChange { .. }
                | SessionEntry::SessionInfo { .. } => {}
            }
        }
        assert!(saw_message_with_orig_id, "message id was rewritten");
        assert!(saw_model_change, "model change dropped during fork");
        assert!(saw_compaction, "compaction dropped during fork");
        assert!(saw_label, "label dropped during fork");
    }

    #[test]
    fn test_fork_from_round_trips_through_disk() {
        let dir = TempDir::new().unwrap();
        let mut source = SessionManager::create(dir.path()).unwrap();
        source
            .append_message(Message::User(UserMessage::new_text("hi")))
            .unwrap();
        let source_path = source.path().to_path_buf();

        let target_dir = TempDir::new().unwrap();
        let forked = SessionManager::fork_from(&source_path, target_dir.path()).unwrap();
        let forked_path = forked.path().to_path_buf();

        // Re-open from disk to confirm the fork was actually flushed.
        let reloaded = SessionManager::open(&forked_path).unwrap();
        assert_eq!(
            reloaded.header().parent_session,
            forked.header().parent_session
        );
        assert_eq!(reloaded.message_count(), 1);
    }

    #[test]
    fn test_fork_from_missing_source_errs() {
        let dir = TempDir::new().unwrap();
        let bogus = dir.path().join("nope.jsonl");
        let target = TempDir::new().unwrap();
        match SessionManager::fork_from(&bogus, target.path()) {
            Err(CodingAgentError::Session(msg)) => assert!(msg.contains("Cannot fork")),
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected Err for missing source"),
        }
    }

    #[test]
    fn test_current_session_version_constant() {
        // Sanity: stamp the current value so a change is intentional.
        assert_eq!(CURRENT_SESSION_VERSION, 3);
        let mgr = SessionManager::in_memory();
        assert_eq!(mgr.header().version, CURRENT_SESSION_VERSION);
    }

    #[test]
    fn test_build_session_info_extracts_first_message_and_name() {
        let dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("hello world")))
            .unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("second")))
            .unwrap();
        mgr.append_label("My Project").unwrap();
        let path = mgr.path().to_path_buf();

        let info = build_session_info(&path).unwrap().expect("info present");
        assert_eq!(info.id, mgr.id());
        assert_eq!(info.message_count, 2);
        assert_eq!(info.first_message, "hello world");
        assert_eq!(info.name.as_deref(), Some("My Project"));
        assert!(info.all_messages_text.contains("hello world"));
        assert!(info.all_messages_text.contains("second"));
    }

    #[test]
    fn test_build_session_info_no_messages_yields_placeholder() {
        let dir = TempDir::new().unwrap();
        let mgr = SessionManager::create(dir.path()).unwrap();
        let info = build_session_info(mgr.path()).unwrap().unwrap();
        assert_eq!(info.first_message, "(no messages)");
        assert_eq!(info.message_count, 0);
        assert!(info.name.is_none());
        assert!(info.all_messages_text.is_empty());
    }

    #[test]
    fn test_build_session_info_label_clear_returns_none() {
        let dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        mgr.append_label("first").unwrap();
        // Clear via append_label with an empty string — empty trims
        // away in build_session_info, so name should drop back to None.
        mgr.append_label("   ").unwrap();
        let info = build_session_info(mgr.path()).unwrap().unwrap();
        assert!(
            info.name.is_none(),
            "expected cleared label, got {:?}",
            info.name
        );
    }

    #[test]
    fn test_build_session_info_no_header_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no-header.jsonl");
        std::fs::write(&path, "{\"type\":\"foo\"}\n").unwrap();
        assert!(build_session_info(&path).unwrap().is_none());
    }

    #[test]
    fn test_session_info_matches_search() {
        let info = SessionInfo {
            path: PathBuf::from("/tmp/x.jsonl"),
            id: "s_x".into(),
            cwd: "/proj".into(),
            timestamp: 0,
            modified: 0,
            message_count: 1,
            name: Some("Refactor Pipeline".into()),
            parent_session_path: None,
            first_message: "investigate slow query".into(),
            all_messages_text: "investigate slow query SELECT * FROM users".into(),
        };

        // Empty query — match all.
        assert!(info.matches(""));
        assert!(info.matches("   "));
        // Hits in name.
        assert!(info.matches("refactor"));
        assert!(info.matches("REFACTOR"));
        // Hits in first_message.
        assert!(info.matches("slow"));
        // Hits in all_messages_text only.
        assert!(info.matches("users"));
        // Misses.
        assert!(!info.matches("postgres"));
    }

    #[test]
    fn test_list_returns_modified_descending() {
        let dir = TempDir::new().unwrap();
        let older = SessionManager::create(dir.path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = SessionManager::create(dir.path()).unwrap();

        let listed = SessionManager::list(dir.path()).unwrap();
        assert_eq!(listed.len(), 2);
        // newer first
        assert_eq!(listed[0].id, newer.id());
        assert_eq!(listed[1].id, older.id());
        // modified is set on every info (non-zero)
        assert!(listed[0].modified > 0);
    }

    /// `SessionInfo.modified` MUST prefer the latest message
    /// timestamp over the file's mtime. Listing UIs sort by "last
    /// activity" — using mtime would be wrong because:
    ///   - merely loading a session updates atime/mtime on some FSes;
    ///   - sync engines (Dropbox, iCloud) and backup tools rewrite mtime;
    ///   - a `touch` would silently reshuffle the picker.
    ///
    /// The test pins this by appending a message with an explicit
    /// `timestamp` and asserting `info.modified` matches that timestamp,
    /// not the file's mtime. We write a JSONL file directly so the
    /// message timestamp is decoupled from the file write time.
    #[test]
    fn test_session_info_modified_uses_message_timestamp_not_mtime() {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join(".hand").join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("frozen.jsonl");

        // Header with creation timestamp set well in the past, plus a
        // message whose timestamp ALSO sits in the past. The file's
        // mtime will be `now()` once we write it, so the three timestamps
        // are distinct enough to make the source-of-truth obvious.
        //
        // Hand's on-disk shape uses the `{"type": <tag>, "data": {...}}`
        // envelope from serde's adjacent tagging — flat object shapes
        // without the envelope won't parse here.
        let header = r#"{"type":"session","data":{"version":3,"id":"sid-frozen","timestamp":1000,"cwd":"/tmp"}}"#;
        let message = r#"{"type":"message","data":{"id":"mid1","message":{"role":"user","content":"hi","timestamp":2000},"timestamp":2000}}"#;
        std::fs::write(&path, format!("{header}\n{message}\n")).unwrap();

        let listed = SessionManager::list(dir.path()).unwrap();
        let info = listed
            .into_iter()
            .find(|i| i.id == "sid-frozen")
            .expect("session listed");

        // The message timestamp (2000) must win — NOT the file's mtime
        // (which is `now()` and would be many orders of magnitude larger).
        assert_eq!(
            info.modified, 2000,
            "modified must equal last-message timestamp, not file mtime"
        );
    }

    /// Tail case: when a session has no messages, fall back to file
    /// mtime — never the header creation timestamp. The picker should
    /// still surface recently-touched empty sessions.
    #[test]
    fn test_session_info_modified_falls_back_to_mtime_when_no_messages() {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join(".hand").join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("empty.jsonl");

        // Header only, no message entries. Header timestamp is set far
        // in the past so the mtime path is the only way `modified` can
        // be "now".
        let header = r#"{"type":"session","data":{"version":3,"id":"sid-empty","timestamp":1000,"cwd":"/tmp"}}"#;
        std::fs::write(&path, format!("{header}\n")).unwrap();

        let listed = SessionManager::list(dir.path()).unwrap();
        let info = listed
            .into_iter()
            .find(|i| i.id == "sid-empty")
            .expect("empty session still listed");

        // mtime is the wall clock at write time — at least 1970-01-01 + some
        // nontrivial epoch. The header (1000ms) must NOT win.
        assert!(
            info.modified > 1000,
            "expected mtime fallback, got {} (header was 1000)",
            info.modified
        );
    }

    #[test]
    fn test_list_skips_corrupted_jsonl() {
        let dir = TempDir::new().unwrap();
        SessionManager::create(dir.path()).unwrap();
        let session_dir = dir.path().join(".hand").join("sessions");
        std::fs::write(session_dir.join("garbage.jsonl"), "not json\n").unwrap();

        let listed = SessionManager::list(dir.path()).unwrap();
        assert_eq!(listed.len(), 1, "corrupted file should be skipped");
    }

    #[test]
    fn test_list_all_finds_sessions_across_projects() {
        let root = TempDir::new().unwrap();

        let proj_a = root.path().join("a");
        std::fs::create_dir_all(&proj_a).unwrap();
        let _a = SessionManager::create(&proj_a).unwrap();

        let proj_b = root.path().join("b");
        std::fs::create_dir_all(&proj_b).unwrap();
        let _b = SessionManager::create(&proj_b).unwrap();

        // root itself has no .hand dir — should still work
        let listed = SessionManager::list_all(root.path()).unwrap();
        assert_eq!(listed.len(), 2);
        // Each cwd should be the project, not the root
        let cwds: std::collections::HashSet<_> = listed.iter().map(|i| i.cwd.as_str()).collect();
        assert!(cwds.iter().any(|c| c.ends_with("/a")));
        assert!(cwds.iter().any(|c| c.ends_with("/b")));
    }

    #[test]
    fn test_list_all_includes_root_when_root_has_sessions() {
        let root = TempDir::new().unwrap();
        // root itself is a project with sessions
        let _root_session = SessionManager::create(root.path()).unwrap();
        // and a child too
        let child = root.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        let _child_session = SessionManager::create(&child).unwrap();

        let listed = SessionManager::list_all(root.path()).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn test_list_all_missing_root_yields_empty() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let listed = SessionManager::list_all(&missing).unwrap();
        assert!(listed.is_empty());
    }
}
