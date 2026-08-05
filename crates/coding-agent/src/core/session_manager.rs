//! Session management — JSONL-based session persistence.

use crate::core::error::CodingAgentError;
use chrono::Utc;
use hand_agent::session::{
    InMemoryStore, JsonlStore, NO_HEADER_DETAIL, SessionEntry as StoreEntry,
    SessionHeader as StoreHeader, SessionStore, SessionStoreError, SqliteStore,
};
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
/// valid `.jsonl` session files (candidates validated via the bounded
/// header scan in [`read_session_header`], never a full-file load).
/// Mirrors `findMostRecentSession` in TS.
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

/// Byte cap for the bounded header scan in [`read_session_header`].
/// Generous enough for a header line with a long cwd plus some leading
/// line noise, small enough that scanning a directory of multi-megabyte
/// sessions stays proportional to the number of files, not their sizes.
const MAX_SESSION_HEADER_SCAN_BYTES: u64 = 64 * 1024;

/// Flatten an absolute cwd into a single directory name suitable for
/// nesting under `~/.hand/agent/sessions/`. Mirrors the upstream's flattening
/// (`/Users/x/proj` → `--Users-x-proj--`): replaces every path
/// separator with a single `-`, and wraps the result with leading +
/// trailing `--` so it's unambiguously a flattened-cwd marker.
fn flatten_cwd_for_session_dir(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    // upstream uses leading and trailing `--`; the path itself becomes a
    // single token where every separator collapses to one `-`.
    let body = s.replace(std::path::MAIN_SEPARATOR, "-");
    format!("-{body}--")
}

/// Bounded header scan: read at most [`MAX_SESSION_HEADER_SCAN_BYTES`]
/// from the start of `path` and return its parsed session header, if
/// any. Line handling mirrors [`parse_session_entries`]: blank and
/// malformed lines are skipped, and the first line that parses as a
/// [`SessionEntry`] decides the outcome — a `Session` header yields
/// `Some`, anything else yields `None` (matching
/// [`load_entries_from_file`], which treats such files as non-sessions).
///
/// Discovery is best-effort: I/O errors, non-UTF-8 content, and
/// headers buried beyond the scan cap all yield `None` rather than an
/// error, so one odd file can't break scanning a directory.
fn read_session_header(path: &Path) -> Option<SessionHeader> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(MAX_SESSION_HEADER_SCAN_BYTES));
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            // EOF without a parseable entry. A line the cap truncated
            // mid-way fails the parse below and lands here on the
            // next iteration.
            return None;
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEntry>(&line) {
            Ok(SessionEntry::Session(header)) => return Some(header),
            Ok(_) => return None,
            Err(_) => continue,
        }
    }
}

/// Cheap validity probe over [`read_session_header`]: `true` when the
/// bounded scan finds a session header. Used by
/// [`find_most_recent_session`] to skip corrupted / unrelated `.jsonl`
/// files without loading whole session bodies.
fn is_valid_session_file(path: &Path) -> bool {
    read_session_header(path).is_some()
}

/// Build a [`SessionInfo`] from a session file by loading the
/// entries, scanning for the latest label and the first user message,
/// and concatenating user/assistant text for search.
///
/// Returns `Ok(None)` for files that have no valid header (so the
/// caller can `flatten` over a directory listing). I/O errors
/// propagate; entry kinds the typed enum doesn't know are skipped.
pub fn build_session_info(path: &Path) -> Result<Option<SessionInfo>, CodingAgentError> {
    let Ok((dir, stem)) = store_addr(path) else {
        return Ok(None);
    };
    let (store_header, body) = match JsonlStore::new(&dir).load(&stem) {
        Ok(loaded) => loaded,
        // Missing / corrupt / header-less files list as non-sessions,
        // matching the tolerant reader this replaced.
        Err(SessionStoreError::NotFound(_)) | Err(SessionStoreError::Corrupt { .. }) => {
            return Ok(None);
        }
        Err(SessionStoreError::Io(e)) => {
            return Err(CodingAgentError::Session(format!(
                "Failed to read session: {}",
                e
            )));
        }
        Err(e) => return Err(store_err(e)),
    };
    let header = to_typed_header(&store_header);
    let entries: Vec<SessionEntry> = body.iter().filter_map(to_typed_entry).collect();

    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(header.timestamp);

    Ok(Some(session_info_from_parts(
        path.to_path_buf(),
        header,
        &entries,
        mtime,
    )))
}

/// Assemble a [`SessionInfo`] from loaded parts: scan the body
/// entries for the latest label, first user message, searchable text,
/// and message count. `fallback_modified` is used when no message
/// carries a timestamp — file mtime for jsonl listings, store
/// `updated_ms` for sqlite ones.
fn session_info_from_parts(
    path: PathBuf,
    header: SessionHeader,
    entries: &[SessionEntry],
    fallback_modified: i64,
) -> SessionInfo {
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_message_timestamp: Option<i64> = None;

    for entry in entries {
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
    let modified = last_message_timestamp.unwrap_or(fallback_modified);

    SessionInfo {
        path,
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
    }
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

/// Convert the manager's typed header into the store's header shape.
fn to_store_header(header: &SessionHeader) -> StoreHeader {
    StoreHeader {
        version: header.version,
        id: header.id.clone(),
        timestamp: header.timestamp,
        cwd: header.cwd.clone(),
        parent_session: header.parent_session.clone(),
        extra: serde_json::Map::new(),
    }
}

/// Convert a store header back into the typed header. Unknown header
/// fields are dropped, exactly as the previous typed-struct parse did.
fn to_typed_header(header: &StoreHeader) -> SessionHeader {
    SessionHeader {
        version: header.version,
        id: header.id.clone(),
        timestamp: header.timestamp,
        cwd: header.cwd.clone(),
        parent_session: header.parent_session.clone(),
    }
}

/// Convert a typed entry into the store's envelope. The typed enum's
/// serde shape IS the `{"type","data"}` envelope (adjacent tagging), so
/// conversion is serializing into a `Value` and re-reading that as the
/// envelope struct — kind and payload land unchanged.
fn to_store_entry(entry: &SessionEntry) -> Result<StoreEntry, CodingAgentError> {
    Ok(serde_json::from_value(serde_json::to_value(entry)?)?)
}

/// Convert a store envelope back into a typed entry. Returns `None`
/// for kinds or payloads the typed enum doesn't know — the previous
/// tolerant line-parser skipped exactly those lines on read.
fn to_typed_entry(entry: &StoreEntry) -> Option<SessionEntry> {
    serde_json::to_value(entry)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Map a store error into the session error domain.
fn store_err(e: SessionStoreError) -> CodingAgentError {
    CodingAgentError::Session(e.to_string())
}

/// Map a [`SessionStore::load`] failure into the session error domain
/// for callers that address a session by path.
///
/// "There is no session here" gets the caller's own wording via
/// `no_header` — a missing file and a header-less file are the same
/// thing to someone trying to open a session, and the store's internal
/// phrasing reads worse than the caller's. Every other failure keeps
/// the cause the store attached: a corrupt log names the offending
/// line, and that line number is the only way to find it.
fn load_err(
    path: &Path,
    no_header: impl FnOnce() -> String,
    e: SessionStoreError,
) -> CodingAgentError {
    let msg = match e {
        SessionStoreError::Io(io) => format!("Failed to read session: {io}"),
        SessionStoreError::NotFound(_) => no_header(),
        SessionStoreError::Corrupt { ref detail, .. } if detail == NO_HEADER_DETAIL => no_header(),
        SessionStoreError::Corrupt { detail, .. } => {
            format!("Session file is corrupt ({}): {detail}", path.display())
        }
        other => format!("Failed to read {}: {other}", path.display()),
    };
    CodingAgentError::Session(msg)
}

/// Derive the store addressing (directory + file-stem key) for a
/// session file path. The stem is the store key even when the header
/// id differs (literal-path sessions keep working).
fn store_addr(path: &Path) -> Result<(PathBuf, String), CodingAgentError> {
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| {
            CodingAgentError::Session(format!("Invalid session path: {}", path.display()))
        })?;
    Ok((dir, stem))
}

/// Storage backend for session persistence, selected via the
/// `session-backend` setting. `Jsonl` is the historical default: one
/// `.jsonl` file per session. `Sqlite` keeps every session of a
/// session directory in a single database file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionBackend {
    #[default]
    Jsonl,
    Sqlite,
}

/// File name of the per-directory SQLite database used by
/// [`SessionBackend::Sqlite`]. One database per session directory —
/// the multiproject layout (flattened-cwd subdirs, `--session-dir` /
/// base-dir overrides) is unchanged; the db lives in whatever
/// directory resolves.
pub const SQLITE_DB_FILENAME: &str = "sessions.db";

fn sqlite_db_path(session_dir: &Path) -> PathBuf {
    session_dir.join(SQLITE_DB_FILENAME)
}

/// Open the sqlite store for `session_dir`, adopting any JSONL
/// sessions already in that directory (idempotent; the source files
/// are never modified or deleted).
fn open_sqlite_store(session_dir: &Path) -> Result<SqliteStore, CodingAgentError> {
    SqliteStore::open_with_import(sqlite_db_path(session_dir), session_dir).map_err(store_err)
}

/// Resolve an id-or-prefix inside a sqlite store: exact id first, then
/// a unique id-prefix match over the store listing. Zero matches and
/// ambiguous prefixes both error, mirroring the jsonl resolver's
/// user-facing wording.
fn resolve_sqlite_session_id(
    store: &SqliteStore,
    id_or_prefix: &str,
) -> Result<String, CodingAgentError> {
    if store.read_header(id_or_prefix).is_ok() {
        return Ok(id_or_prefix.to_string());
    }
    let mut matches: Vec<String> = store
        .list()
        .map_err(store_err)?
        .into_iter()
        .map(|s| s.header.id)
        .filter(|id| id.starts_with(id_or_prefix))
        .collect();
    matches.sort();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(CodingAgentError::Session(format!(
            "No session found matching '{id_or_prefix}'"
        ))),
        _ => Err(CodingAgentError::Session(format!(
            "Session '{id_or_prefix}' is ambiguous: matches {}",
            matches.join(", ")
        ))),
    }
}

/// Manages session logs. Storage is delegated to a
/// [`hand_agent::session::SessionStore`]: JSONL files for on-disk
/// sessions, the in-memory backend for ephemeral ones. The manager
/// keeps the typed entry list as its in-memory representation and
/// mirrors every mutation through the store.
pub struct SessionManager {
    path: PathBuf,
    session_dir: PathBuf,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    store: Box<dyn SessionStore>,
    /// Store key: the session file's stem for on-disk sessions, the
    /// header id for in-memory and sqlite ones.
    store_key: String,
    backend: SessionBackend,
}

impl SessionManager {
    /// Create a new session file under the default
    /// `<cwd>/.hand/sessions` directory.
    pub fn create(cwd: &Path) -> Result<Self, CodingAgentError> {
        Self::create_in(cwd, &Self::default_session_dir(cwd))
    }

    /// [`Self::create`] with an explicit storage backend.
    pub fn create_with_backend(
        backend: SessionBackend,
        cwd: &Path,
    ) -> Result<Self, CodingAgentError> {
        Self::create_in_with_backend(backend, cwd, &Self::default_session_dir(cwd))
    }

    /// Create a new session file under an explicit session directory.
    /// Used by callers that pass `--session-dir`; the directory is
    /// created if it doesn't exist.
    pub fn create_in(cwd: &Path, session_dir: &Path) -> Result<Self, CodingAgentError> {
        Self::create_in_with_backend(SessionBackend::Jsonl, cwd, session_dir)
    }

    /// [`Self::create_in`] with an explicit storage backend. Under
    /// sqlite the session lands in `<session_dir>/sessions.db`
    /// (existing JSONL sessions in the directory are adopted first).
    pub fn create_in_with_backend(
        backend: SessionBackend,
        cwd: &Path,
        session_dir: &Path,
    ) -> Result<Self, CodingAgentError> {
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

        let (store, path): (Box<dyn SessionStore>, PathBuf) = match backend {
            SessionBackend::Jsonl => (
                Box::new(JsonlStore::new(&session_dir)),
                session_dir.join(format!("{}.jsonl", id)),
            ),
            SessionBackend::Sqlite => (
                Box::new(open_sqlite_store(&session_dir)?),
                sqlite_db_path(&session_dir),
            ),
        };
        store.create(&to_store_header(&header)).map_err(store_err)?;

        Ok(Self {
            path,
            session_dir,
            header: header.clone(),
            entries: vec![SessionEntry::Session(header)],
            store,
            store_key: id,
            backend,
        })
    }

    /// Open an existing session file. The file's stem addresses the
    /// session in its directory's store, even when the header id
    /// differs (literal-path sessions).
    pub fn open(path: &Path) -> Result<Self, CodingAgentError> {
        let (session_dir, stem) = store_addr(path)?;
        let store: Box<dyn SessionStore> = Box::new(JsonlStore::new(&session_dir));
        let (store_header, body) = store.load(&stem).map_err(|e| {
            load_err(
                path,
                || format!("No session header found in {}", path.display()),
                e,
            )
        })?;

        let header = to_typed_header(&store_header);
        let mut entries = Vec::with_capacity(body.len() + 1);
        entries.push(SessionEntry::Session(header.clone()));
        entries.extend(body.iter().filter_map(to_typed_entry));

        Ok(Self {
            path: path.to_path_buf(),
            session_dir,
            header,
            entries,
            store,
            store_key: stem,
            backend: SessionBackend::Jsonl,
        })
    }

    /// Open a session by id (or unique id prefix) inside
    /// `session_dir`, using the given backend. Under jsonl this
    /// resolves `<session_dir>/<id>.jsonl` exactly, then falls back to
    /// a unique file-stem prefix match; under sqlite ids resolve
    /// inside the directory's database. Ambiguous prefixes error.
    pub fn open_by_id_in(
        backend: SessionBackend,
        session_dir: &Path,
        id_or_prefix: &str,
    ) -> Result<Self, CodingAgentError> {
        match backend {
            SessionBackend::Jsonl => {
                let exact = session_dir.join(format!("{id_or_prefix}.jsonl"));
                if exact.is_file() {
                    return Self::open(&exact);
                }
                let mut matches: Vec<PathBuf> = std::fs::read_dir(session_dir)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.path())
                            .filter(|p| {
                                p.extension().and_then(|s| s.to_str()) == Some("jsonl")
                                    && p.file_stem()
                                        .and_then(|s| s.to_str())
                                        .is_some_and(|stem| stem.starts_with(id_or_prefix))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                matches.sort();
                match matches.len() {
                    1 => Self::open(&matches[0]),
                    0 => Err(CodingAgentError::Session(format!(
                        "No session found matching '{id_or_prefix}'"
                    ))),
                    _ => Err(CodingAgentError::Session(format!(
                        "Session '{id_or_prefix}' is ambiguous: matches {}",
                        matches
                            .iter()
                            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))),
                }
            }
            SessionBackend::Sqlite => {
                let store = open_sqlite_store(session_dir)?;
                let id = resolve_sqlite_session_id(&store, id_or_prefix)?;
                Self::from_sqlite_store(store, session_dir, &id)
            }
        }
    }

    /// Build a manager over an already-open sqlite store for the
    /// session `id` (which must exist in the store).
    fn from_sqlite_store(
        store: SqliteStore,
        session_dir: &Path,
        id: &str,
    ) -> Result<Self, CodingAgentError> {
        let (store_header, body) = store.load(id).map_err(store_err)?;
        let header = to_typed_header(&store_header);
        let mut entries = Vec::with_capacity(body.len() + 1);
        entries.push(SessionEntry::Session(header.clone()));
        entries.extend(body.iter().filter_map(to_typed_entry));
        Ok(Self {
            path: sqlite_db_path(session_dir),
            session_dir: session_dir.to_path_buf(),
            header,
            entries,
            store: Box::new(store),
            store_key: id.to_string(),
            backend: SessionBackend::Sqlite,
        })
    }

    /// Create an in-memory (ephemeral) session.
    pub fn in_memory() -> Self {
        let id = generate_session_id();
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: ".".into(),
            parent_session: None,
        };
        let store: Box<dyn SessionStore> = Box::new(InMemoryStore::new());
        store
            .create(&to_store_header(&header))
            .expect("in-memory create with a fresh id cannot fail");
        Self {
            path: PathBuf::new(),
            session_dir: PathBuf::new(),
            header: header.clone(),
            entries: vec![SessionEntry::Session(header)],
            store,
            store_key: id,
            backend: SessionBackend::Jsonl,
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
        Self::from_branched_entries_with_backend(
            SessionBackend::Jsonl,
            cwd,
            in_memory,
            parent_id,
            body_entries,
        )
    }

    /// [`Self::from_branched_entries`] with an explicit storage
    /// backend for the replacement session (in-memory sessions ignore
    /// it — they stay in memory).
    pub fn from_branched_entries_with_backend(
        backend: SessionBackend,
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

        let (store, path, session_dir): (Box<dyn SessionStore>, PathBuf, PathBuf) = if in_memory {
            (
                Box::new(InMemoryStore::new()),
                PathBuf::new(),
                PathBuf::new(),
            )
        } else {
            let session_dir = Self::default_session_dir(cwd);
            std::fs::create_dir_all(&session_dir)?;
            match backend {
                SessionBackend::Jsonl => {
                    let path = session_dir.join(format!("{}.jsonl", id));
                    (
                        Box::new(JsonlStore::new(&session_dir)) as Box<dyn SessionStore>,
                        path,
                        session_dir,
                    )
                }
                SessionBackend::Sqlite => {
                    let store = open_sqlite_store(&session_dir)?;
                    let path = sqlite_db_path(&session_dir);
                    (Box::new(store) as Box<dyn SessionStore>, path, session_dir)
                }
            }
        };

        store.create(&to_store_header(&header)).map_err(store_err)?;
        for entry in &body_entries {
            store
                .append(&id, &to_store_entry(entry)?)
                .map_err(store_err)?;
        }

        let mut entries = Vec::with_capacity(body_entries.len() + 1);
        entries.push(SessionEntry::Session(header.clone()));
        entries.extend(body_entries);

        Ok(Self {
            path,
            session_dir,
            header,
            entries,
            store,
            store_key: id,
            backend,
        })
    }

    /// Whether this session manager is purely in-memory (no JSONL file
    /// backing it). Used by callers like
    /// [`crate::core::agent_session::AgentSession::reset_session`] to pick
    /// the right constructor for the replacement manager — an in-memory
    /// session must reset to an in-memory session, otherwise we would
    /// suddenly try to write `./.hand/sessions/*.jsonl` from a test.
    /// Derived from the absence of a backing file path — only the
    /// in-memory constructor leaves it empty.
    pub fn is_in_memory(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    /// Storage backend this manager was constructed with. Drives
    /// backend-aware replacement flows (reset / fork) and the picker's
    /// id-vs-path selection.
    pub fn backend(&self) -> SessionBackend {
        self.backend
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
        self.persist_entry(self.entries.last().unwrap())?;
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
        self.persist_entry(self.entries.last().unwrap())?;
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
        self.persist_entry(self.entries.last().unwrap())?;
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
        self.persist_entry(self.entries.last().unwrap())?;
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
        Self::continue_recent_in(cwd, None)
    }

    /// Same as [`Self::continue_recent`] but searches the explicit
    /// `--session-dir` override when one is configured. Without this
    /// distinction, a `--continue --session-dir /tmp/foo` invocation
    /// would find the most-recent session under the home-based
    /// default, hand its id to the resume site, and then fail to
    /// open it because the resume site dutifully looked in
    /// `--session-dir` (#58). Now the search and the open agree on
    /// the same directory.
    ///
    /// Discovery goes through [`Self::most_recent_session_path`]
    /// (bounded header scans + file mtime), so the only full read is
    /// the single [`Self::open`] of the chosen file.
    pub fn continue_recent_in(
        cwd: &Path,
        session_dir: Option<&Path>,
    ) -> Result<Self, CodingAgentError> {
        let most_recent = Self::most_recent_session_path(cwd, session_dir)
            .ok_or_else(|| CodingAgentError::Session("No sessions found to continue".into()))?;
        Self::open(&most_recent)
    }

    /// Locate the most recent session file for `cwd` without opening
    /// it: candidates are validated with a bounded header scan and
    /// ranked by file mtime, so discovery cost scales with the number
    /// of files, not their sizes. `session_dir` overrides the
    /// home-based default when set (`--session-dir`).
    ///
    /// Callers that resume via
    /// [`crate::core::agent_session::AgentSessionConfig::resume_session`]
    /// should pass this path straight through so the session body is
    /// read exactly once — by whoever finally opens it.
    pub fn most_recent_session_path(cwd: &Path, session_dir: Option<&Path>) -> Option<PathBuf> {
        let dir = match session_dir {
            Some(dir) => dir.to_path_buf(),
            None => Self::default_session_dir(cwd),
        };
        find_most_recent_session(&dir)
    }

    /// Backend-aware `--continue` discovery: the most recent session
    /// as a resume key. Under jsonl this is the session file path
    /// (unchanged semantics: bounded header scans, mtime ranking);
    /// under sqlite it is the newest session id by store `updated_ms`.
    pub fn most_recent_session_key_with_backend(
        backend: SessionBackend,
        cwd: &Path,
        session_dir: Option<&Path>,
    ) -> Option<String> {
        match backend {
            SessionBackend::Jsonl => Self::most_recent_session_path(cwd, session_dir)
                .map(|p| p.to_string_lossy().into_owned()),
            SessionBackend::Sqlite => {
                let dir = match session_dir {
                    Some(dir) => dir.to_path_buf(),
                    None => Self::default_session_dir(cwd),
                };
                let store = open_sqlite_store(&dir).ok()?;
                store
                    .list()
                    .ok()?
                    .first()
                    .map(|summary| summary.header.id.clone())
            }
        }
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
        Self::fork_from_in(source_path, cwd, None)
    }

    /// Same as [`Self::fork_from`] but writes the new fork under an
    /// explicit `--session-dir` override when provided. Without this,
    /// `hand --fork <id> --session-dir <X>` would write the new fork
    /// to the home-based default and the subsequent resume lookup
    /// (which honours --session-dir via #58) would fail to find it
    /// (#77 part 2).
    pub fn fork_from_in(
        source_path: &Path,
        cwd: &Path,
        session_dir: Option<&Path>,
    ) -> Result<Self, CodingAgentError> {
        // Load through a store rooted at the source's directory: the
        // source may live in a different dir than the fork target, so
        // reads and writes go through separate stores.
        let (source_dir, source_stem) = store_addr(source_path)?;
        let source_store = JsonlStore::new(&source_dir);
        let (source_header, source_body) = source_store.load(&source_stem).map_err(|e| {
            load_err(
                source_path,
                || {
                    format!(
                        "Cannot fork: source session is empty or has no header: {}",
                        source_path.display()
                    )
                },
                e,
            )
        })?;

        let session_dir = match session_dir {
            Some(dir) => dir.to_path_buf(),
            None => Self::default_session_dir(cwd),
        };
        std::fs::create_dir_all(&session_dir)?;

        // Same collision safety as `create_in` (#76): refuse to mint a
        // path that already exists. Two concurrent fork operations
        // landing in the same millisecond bucket must not share a file.
        let (id, path) = mint_unique_session_path(&session_dir)?;
        let header = SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.clone(),
            timestamp: Utc::now().timestamp_millis(),
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: Some(source_header.id.clone()),
        };

        // Preserve every non-header entry from the source verbatim --
        // ids included, so downstream cross-references stay valid.
        // Envelopes the typed enum can't represent are skipped, same
        // as the previous tolerant line-parser.
        let body: Vec<SessionEntry> = source_body
            .iter()
            .filter(|e| !e.is_header())
            .filter_map(to_typed_entry)
            .collect();

        let store: Box<dyn SessionStore> = Box::new(JsonlStore::new(&session_dir));
        store.create(&to_store_header(&header)).map_err(store_err)?;
        for entry in &body {
            store
                .append(&id, &to_store_entry(entry)?)
                .map_err(store_err)?;
        }

        let mut entries = Vec::with_capacity(body.len() + 1);
        entries.push(SessionEntry::Session(header.clone()));
        entries.extend(body);

        Ok(Self {
            path,
            session_dir,
            header,
            entries,
            store,
            store_key: id,
            backend: SessionBackend::Jsonl,
        })
    }

    /// Fork a session by id (or unique id prefix) inside the sqlite
    /// database of `session_dir` (or `cwd`'s default session dir).
    /// Store-level fork: body entries keep their ids, the new header
    /// records the source id as `parent_session`. The forked header
    /// inherits the source session's stored cwd (the CLI only exposes
    /// same-cwd forks, where the two agree).
    pub fn fork_in_sqlite(
        cwd: &Path,
        session_dir: Option<&Path>,
        source: &str,
    ) -> Result<Self, CodingAgentError> {
        let dir = match session_dir {
            Some(dir) => dir.to_path_buf(),
            None => Self::default_session_dir(cwd),
        };
        std::fs::create_dir_all(&dir)?;
        let store = open_sqlite_store(&dir)?;
        let source_id = resolve_sqlite_session_id(&store, source)?;
        let new_id = generate_session_id();
        store
            .fork(&source_id, &new_id, Utc::now().timestamp_millis(), None)
            .map_err(store_err)?;
        Self::from_sqlite_store(store, &dir, &new_id)
    }

    /// Get the session display name or ID.
    pub fn display_name(&self) -> &str {
        &self.header.id
    }

    /// Get the session header.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Stored cwd recorded in this session's header, if any.
    pub fn stored_cwd(&self) -> Option<PathBuf> {
        if self.header.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.header.cwd))
        }
    }

    /// Path to the on-disk session file, or `None` for in-memory
    /// sessions. Bridges `SessionManager` into the
    /// [`crate::core::session_cwd::SessionCwdSource`] trait without
    /// hard-coding a coupling — the impl lives at the call site.
    pub fn on_disk_session_file(&self) -> Option<PathBuf> {
        if self.is_in_memory() {
            None
        } else {
            Some(self.path.clone())
        }
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

    /// Backend-aware listing for the `/resume` picker. Jsonl scans
    /// `cwd`'s default session dir as before; sqlite reads the
    /// directory's database (adopting existing JSONL sessions on
    /// first use) and fills the same [`SessionInfo`] fields.
    /// `SessionInfo.path` carries the database path under sqlite —
    /// selection there resolves by `SessionInfo.id`, not path.
    pub fn list_with_backend(
        backend: SessionBackend,
        cwd: &Path,
    ) -> Result<Vec<SessionInfo>, CodingAgentError> {
        match backend {
            SessionBackend::Jsonl => Self::list(cwd),
            SessionBackend::Sqlite => {
                let dir = Self::default_session_dir(cwd);
                let store = open_sqlite_store(&dir)?;
                let db_path = sqlite_db_path(&dir);
                let mut sessions = Vec::new();
                for summary in store.list().map_err(store_err)? {
                    // Skip sessions that fail to load rather than
                    // breaking the whole listing (mirrors the jsonl
                    // scanner's per-file tolerance).
                    let Ok((store_header, body)) = store.load(&summary.header.id) else {
                        continue;
                    };
                    let header = to_typed_header(&store_header);
                    let entries: Vec<SessionEntry> =
                        body.iter().filter_map(to_typed_entry).collect();
                    sessions.push(session_info_from_parts(
                        db_path.clone(),
                        header,
                        &entries,
                        summary.updated_ms,
                    ));
                }
                sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
                Ok(sessions)
            }
        }
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
        // `root` is treated as the user's HOME-equivalent: we scan
        // `<root>/.hand/agent/sessions/*/` since the new layout stores
        // every project's sessions under HOME, with each project keyed
        // by a flattened-cwd subdir. Production callers pass HOME;
        // tests pass a tempdir and set HAND_HOME so writers land in
        // the same place.
        let root_sessions = root.join(".hand").join("agent").join("sessions");
        let mut sessions = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(&root_sessions) {
            for entry in read_dir.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                sessions.extend(list_sessions_from_dir(&entry.path())?);
            }
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
        Ok(sessions)
    }

    /// Default session storage location. Sessions live under
    /// `~/.hand/agent/sessions/<flattened-cwd>/` — the cwd is encoded
    /// as a single directory name with path separators replaced by
    /// `-` so every project gets its own subdir without polluting
    /// the project tree itself. Mirrors the upstream's `~/.upstream/agent/sessions/`
    /// layout.
    ///
    /// `HAND_HOME` env var overrides the home-dir lookup when set;
    /// tests use this to redirect persistence into a tempdir without
    /// touching the user's real `~/.hand/`. When neither `HAND_HOME`
    /// nor `$HOME` resolves, falls back to `<cwd>/.hand/sessions` so
    /// tests and ephemeral runs still have a deterministic location.
    pub fn default_session_dir(cwd: &Path) -> PathBuf {
        Self::default_session_dir_with_base(None, cwd)
    }

    /// Resolve a user-supplied `--fork` / `--resume` argument to a
    /// concrete session file. Tries, in order:
    ///   1. `source` as a literal path (any path that exists on disk).
    ///   2. `<home-based default_session_dir>/<source>.jsonl` (the
    ///      modern writer location used by both default and embedder
    ///      builds — see [`Self::default_session_dir_with_base`]).
    ///   3. ID-prefix match in the home-based default_session_dir.
    ///   4. `<cwd>/.hand/sessions/<source>.jsonl` (legacy layout).
    ///   5. ID-prefix match in the legacy layout.
    ///
    /// Falls back to `PathBuf::from(source)` when nothing matches so
    /// the caller's error message can carry the user's raw input. The
    /// `base` parameter is honoured the same way as
    /// [`Self::default_session_dir_with_base`] for embedders that route
    /// state through their own data directory.
    pub fn resolve_session_source(base: Option<&Path>, cwd: &Path, source: &str) -> PathBuf {
        Self::resolve_session_source_in(None, base, cwd, source)
    }

    /// Same as [`Self::resolve_session_source`] but probes an explicit
    /// `--session-dir` override before the home-based / legacy
    /// fallbacks. Mirrors the plumbing `--continue` / `--resume` already
    /// have so `--fork <id> --session-dir <X>` resolves ids stored in
    /// `<X>` instead of erroring with "No session found" (#77).
    pub fn resolve_session_source_in(
        session_dir: Option<&Path>,
        base: Option<&Path>,
        cwd: &Path,
        source: &str,
    ) -> PathBuf {
        let raw = PathBuf::from(source);
        if raw.is_file() {
            return raw;
        }
        let home_dir = Self::default_session_dir_with_base(base, cwd);
        let legacy_dir = cwd.join(".hand").join("sessions");
        let override_dir = session_dir.map(|d| d.to_path_buf());
        let probe: Vec<&PathBuf> = override_dir
            .iter()
            .chain([&home_dir, &legacy_dir])
            .collect();
        for dir in probe {
            let exact = dir.join(format!("{source}.jsonl"));
            if exact.is_file() {
                return exact;
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if let Some(name_str) = name.to_str()
                        && name_str.starts_with(source)
                        && name_str.ends_with(".jsonl")
                    {
                        return entry.path();
                    }
                }
            }
        }
        raw
    }

    /// Like [`Self::default_session_dir`] but honors an explicit base
    /// directory when provided.
    ///
    /// When `base` is `Some(b)`, sessions land under
    /// `b/sessions/<flattened-cwd>/` — used by embedders (Tauri,
    /// sandboxed apps) that route persistent state through their own
    /// per-app data directory instead of the user's home. When `None`,
    /// falls back to `HAND_HOME` env, then `dirs::home_dir()`, then
    /// `<cwd>/.hand/sessions` (mirrors the previous behaviour).
    pub fn default_session_dir_with_base(base: Option<&Path>, cwd: &Path) -> PathBuf {
        if let Some(base) = base {
            return base.join("sessions").join(flatten_cwd_for_session_dir(cwd));
        }
        let home = std::env::var_os("HAND_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir);
        match home {
            Some(home) => home
                .join(".hand")
                .join("agent")
                .join("sessions")
                .join(flatten_cwd_for_session_dir(cwd)),
            None => cwd.join(".hand").join("sessions"),
        }
    }

    /// Persist one just-recorded entry through the store. The
    /// in-memory backend absorbs what used to be `if !self.in_memory`
    /// guards at every append site.
    fn persist_entry(&self, entry: &SessionEntry) -> Result<(), CodingAgentError> {
        self.store
            .append(&self.store_key, &to_store_entry(entry)?)
            .map_err(store_err)
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
    // Process- and clock-collision safety: append 64 random bits so two
    // processes that mint an id in the same millisecond (the per-process
    // COUNTER never sees the other) cannot land on the same filename.
    // Without this, concurrent `--print` invocations against a shared
    // --session-dir clobber each other's JSONL files (#76).
    let rand_suffix: u64 = rand::random();
    format!("s_{ts:x}_{c:x}_{rand_suffix:016x}")
}

/// Mint a session id whose `<session_dir>/<id>.jsonl` does not yet
/// exist. Even with the 64-bit random suffix in
/// [`generate_session_id`] a freak collision is astronomically
/// unlikely -- this loop catches the impossible-but-possible case
/// (and any test that monkey-patches the clock/RNG) before the
/// caller would silently overwrite an existing session.
fn mint_unique_session_path(session_dir: &Path) -> Result<(String, PathBuf), CodingAgentError> {
    // Eight tries is two-cubed-plus-two more than the per-process
    // counter can produce in a single millisecond bucket; if all eight
    // collide something is structurally wrong (mocked rng/clock, full
    // disk that materialises stale files, ...) and bubbling an error
    // is far better than corrupting either session.
    for _ in 0..8 {
        let id = generate_session_id();
        let path = session_dir.join(format!("{id}.jsonl"));
        if !path.exists() {
            return Ok((id, path));
        }
    }
    Err(CodingAgentError::Session(format!(
        "Could not mint a unique session id under {} after 8 attempts",
        session_dir.display()
    )))
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

    /// Process-wide mutex guarding `HAND_HOME` env-var mutations.
    /// Tests that need to redirect the session root acquire it via
    /// `scoped_hand_home`; this serialises them so the env-var
    /// override doesn't race across parallel test threads.
    static HAND_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard for tests that need to redirect `default_session_dir`
    /// at a chosen home. Acquires `HAND_HOME_LOCK` on construction and
    /// holds it until drop, so tests that touch the same env var run
    /// one at a time even under cargo's parallel runner.
    struct ScopedHandHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl Drop for ScopedHandHome {
        fn drop(&mut self) {
            // SAFETY: HAND_HOME_LOCK is held for the duration of the
            // mutation, so no other test thread reads/writes the env
            // var concurrently.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("HAND_HOME", v),
                    None => std::env::remove_var("HAND_HOME"),
                }
            }
        }
    }
    fn scoped_hand_home(root: &Path) -> ScopedHandHome {
        let lock = HAND_HOME_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let prev = std::env::var_os("HAND_HOME");
        // SAFETY: HAND_HOME_LOCK is held.
        unsafe {
            std::env::set_var("HAND_HOME", root);
        }
        ScopedHandHome { _lock: lock, prev }
    }

    /// Issue #58: `--continue --session-dir <X>` must search inside
    /// `<X>` for the most-recent session, not the home-based default.
    /// The pre-fix flow found a session under HOME, handed its id to
    /// the resume site, and then errored because the resume opened
    /// `<X>/<id>.jsonl` which never existed. The two should agree on
    /// one directory.
    #[test]
    fn continue_recent_in_with_override_searches_that_dir_not_home() {
        let cwd_dir = TempDir::new().unwrap();
        let cwd = cwd_dir.path();
        let override_dir = TempDir::new().unwrap();

        // Empty override → "no sessions" error, NOT a synthetic id
        // pointing at a session that doesn't exist.
        match SessionManager::continue_recent_in(cwd, Some(override_dir.path())) {
            Ok(_) => panic!("empty override should error with no-sessions"),
            Err(e) => assert!(e.to_string().contains("No sessions"), "wrong error: {e}"),
        }

        // Seed a session into the override dir → continue picks it up.
        let id = "s_override_777";
        let file = override_dir.path().join(format!("{id}.jsonl"));
        let header = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"{id}\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            cwd.display()
        );
        std::fs::write(&file, header).unwrap();
        let opened =
            SessionManager::continue_recent_in(cwd, Some(override_dir.path())).expect("opens");
        assert_eq!(opened.id(), id);
    }

    #[test]
    fn default_session_dir_with_base_routes_through_provided_root() {
        // base_dir override: sessions land under <base>/sessions/<flattened-cwd>/.
        // No HAND_HOME mutation needed — base override should bypass the env-var
        // and home-dir lookup entirely.
        let cwd = std::path::PathBuf::from("/tmp/projx");
        let base = std::path::PathBuf::from("/var/app-data/hand");
        let dir = SessionManager::default_session_dir_with_base(Some(&base), &cwd);

        assert!(
            dir.starts_with(&base),
            "session dir should be rooted under base, got {dir:?}"
        );
        assert!(
            dir.to_string_lossy().contains("/sessions/"),
            "session dir should nest under 'sessions/', got {dir:?}"
        );
        // Flattened cwd marker (leading + trailing `--`) must appear so the
        // path is unambiguously a cwd-encoded subdir, not the literal `projx`.
        let last = dir.file_name().expect("dir has a final component");
        assert!(
            last.to_string_lossy().starts_with('-') && last.to_string_lossy().ends_with("--"),
            "final component should be a flattened-cwd marker, got {last:?}"
        );
    }

    /// Issue #27: `--fork <id>` previously only probed the legacy
    /// `<cwd>/.hand/sessions` directory, so an ID-prefix that referred
    /// to a session living under the modern home-based layout failed
    /// with `No session found matching '...'`. `resolve_session_source`
    /// must consult the home-based dir first (the writer's location),
    /// support both exact-ID-with-.jsonl and ID-prefix matching, and
    /// still tolerate the legacy layout as a fallback.
    #[test]
    fn resolve_session_source_matches_id_prefix_in_home_based_dir() {
        let home = TempDir::new().unwrap();
        let _g = scoped_hand_home(home.path());
        let cwd = TempDir::new().unwrap();
        let cwd_path = cwd.path();

        let home_dir = SessionManager::default_session_dir_with_base(None, cwd_path);
        std::fs::create_dir_all(&home_dir).unwrap();
        let full_id = "s_19e5db89020_0";
        let session_file = home_dir.join(format!("{full_id}.jsonl"));
        // The resolver only needs the file to exist on disk for the
        // prefix-match branch, so an empty file is enough.
        std::fs::write(&session_file, "").unwrap();

        // Exact ID (no extension).
        let resolved = SessionManager::resolve_session_source(None, cwd_path, full_id);
        assert_eq!(resolved, session_file, "exact ID must resolve");

        // ID-prefix — the issue's exact failure case.
        let resolved = SessionManager::resolve_session_source(None, cwd_path, "s_19e5db89020");
        assert_eq!(resolved, session_file, "ID prefix must resolve");

        // Literal absolute path still works (#25 invariant).
        let resolved =
            SessionManager::resolve_session_source(None, cwd_path, session_file.to_str().unwrap());
        assert_eq!(resolved, session_file, "literal path must resolve verbatim");

        // Bogus input falls through to PathBuf::from(source) so the
        // caller's error message carries the user's raw text.
        let bogus = SessionManager::resolve_session_source(None, cwd_path, "s_not_a_thing");
        assert_eq!(bogus, PathBuf::from("s_not_a_thing"));
    }

    /// Regression for #77: `--fork <id> --session-dir <X>` must
    /// resolve ids stored in `<X>` (same plumbing #58 added for
    /// `--continue`). Seed a session under an explicit dir that is
    /// NOT the home-based default, and assert
    /// `resolve_session_source_in` finds it by both exact id and
    /// prefix, while the home-based-only resolver does not.
    #[test]
    fn resolve_session_source_in_honours_explicit_session_dir() {
        let home = TempDir::new().unwrap();
        let _g = scoped_hand_home(home.path());
        let cwd = TempDir::new().unwrap();
        let cwd_path = cwd.path();

        // Use a session_dir that is NOT the home-based default so a
        // false positive (resolving from home) cannot pass.
        let override_dir = TempDir::new().unwrap();
        let override_dir = override_dir.path();
        let full_id = "s_19e72011771_0_8c29f66da9ec33e3";
        let session_file = override_dir.join(format!("{full_id}.jsonl"));
        std::fs::write(&session_file, "").unwrap();

        // Without the override, the home-based default doesn't see it.
        let resolved = SessionManager::resolve_session_source(None, cwd_path, full_id);
        assert_ne!(
            resolved, session_file,
            "without --session-dir the resolver must not magically find override-dir files"
        );

        // With the override, both exact and prefix match.
        let resolved =
            SessionManager::resolve_session_source_in(Some(override_dir), None, cwd_path, full_id);
        assert_eq!(
            resolved, session_file,
            "exact id must resolve under --session-dir"
        );

        let resolved = SessionManager::resolve_session_source_in(
            Some(override_dir),
            None,
            cwd_path,
            "s_19e72011771",
        );
        assert_eq!(
            resolved, session_file,
            "id prefix must resolve under --session-dir"
        );
    }

    #[test]
    fn default_session_dir_with_base_none_matches_default() {
        // base=None must behave identically to the legacy entry point.
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let cwd = std::path::PathBuf::from("/tmp/projy");

        let via_helper = SessionManager::default_session_dir_with_base(None, &cwd);
        let via_default = SessionManager::default_session_dir(&cwd);
        assert_eq!(via_helper, via_default);
    }

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
        let _g = scoped_hand_home(dir.path());
        let mgr = SessionManager::create(dir.path()).unwrap();
        let id = mgr.id().to_string();
        let path = mgr.path().to_path_buf();

        let opened = SessionManager::open(&path).unwrap();
        assert_eq!(opened.id(), id);
    }

    /// A fully-written malformed line is corruption the store reports
    /// with the offending line number. `open` must surface that number
    /// — pre-fix it was replaced by "No session header found", sending
    /// the reader to inspect the one part of the file that parses.
    #[test]
    fn open_corrupt_line_reports_the_line_not_a_missing_header() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        let path = mgr.path().to_path_buf();

        // Header (line 1) and the appended message (line 2) stay valid;
        // line 3 is malformed and fully written.
        let mut contents = std::fs::read_to_string(&path).unwrap();
        contents.push_str("not json\n");
        std::fs::write(&path, contents).unwrap();

        let err = match SessionManager::open(&path) {
            Ok(_) => panic!("a fully-written malformed line must fail the open"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("line 3"), "should name the bad line: {err}");
        assert!(
            !err.contains("No session header found"),
            "header is intact; must not blame it: {err}"
        );
    }

    /// The historical wording is preserved for a file that genuinely
    /// carries no header. Note the store reports this as `Corrupt` with
    /// [`NO_HEADER_DETAIL`], not `NotFound` — `load_err` maps both.
    #[test]
    fn open_header_less_file_still_reports_no_session_header() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let path = dir.path().join("s_headerless.jsonl");
        std::fs::write(&path, "").unwrap();

        let err = match SessionManager::open(&path) {
            Ok(_) => panic!("a header-less file must fail the open"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("No session header found"), "{err}");
    }

    /// A torn last line is the crash shape that actually occurs, and it
    /// stays tolerated: the complete prefix loads.
    #[test]
    fn open_torn_last_line_loads_the_complete_prefix() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        let path = mgr.path().to_path_buf();

        // A partial write: malformed AND missing its trailing newline.
        let mut contents = std::fs::read_to_string(&path).unwrap();
        contents.push_str("{\"type\":\"mes");
        std::fs::write(&path, contents).unwrap();

        let opened = SessionManager::open(&path).expect("torn tail must be tolerated");
        assert_eq!(opened.message_count(), 1);
    }

    #[test]
    fn test_session_persist_messages() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
        let older = SessionManager::create(dir.path()).unwrap();
        // Tiny sleep so mtimes differ even on coarse filesystems.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = SessionManager::create(dir.path()).unwrap();

        let found = find_most_recent_session(&SessionManager::default_session_dir(dir.path()))
            .expect("should find a session");
        assert_eq!(found, newer.path());
        assert_ne!(found, older.path());
    }

    #[test]
    fn test_find_most_recent_session_skips_invalid_files() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let mgr = SessionManager::create(dir.path()).unwrap();
        let session_dir = SessionManager::default_session_dir(dir.path());

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

    fn header_line_for(id: &str, cwd: &str) -> String {
        serde_json::to_string(&SessionEntry::Session(SessionHeader {
            version: CURRENT_SESSION_VERSION,
            id: id.into(),
            timestamp: 1,
            cwd: cwd.into(),
            parent_session: None,
        }))
        .unwrap()
    }

    /// Discovery must validate candidates from a bounded header read,
    /// not a full-file load: a valid header followed by a megabyte of
    /// invalid tail is still a session, and a header line longer than
    /// a small fixed probe (the old 512-byte one-shot read) still
    /// parses.
    #[test]
    fn header_scan_accepts_long_header_and_invalid_tail() {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();

        let mut content = header_line_for("s_long_header", &format!("/deep/{}", "x".repeat(700)));
        assert!(content.len() > 512, "header must exceed a small probe");
        content.push('\n');
        // ~1 MiB of lines that never parse as session entries.
        for _ in 0..20_000 {
            content.push_str("tail junk that is definitely not a session entry\n");
        }
        let path = session_dir.join("big.jsonl");
        std::fs::write(&path, &content).unwrap();

        assert_eq!(find_most_recent_session(&session_dir), Some(path));
    }

    /// Header discovery tolerates the same line noise as
    /// [`parse_session_entries`]: blank / malformed lines before the
    /// header are skipped, while a file whose first parseable entry is
    /// not a header stays invalid — exactly the files
    /// [`load_entries_from_file`] loads and rejects respectively, so
    /// discovery and open can never disagree on what counts as a
    /// session.
    #[test]
    fn header_scan_matches_loader_line_tolerance() {
        let dir = TempDir::new().unwrap();

        // Junk-then-header: valid to the full loader, so the bounded
        // scan must agree.
        let junk_then_header = dir.path().join("junk-then-header");
        std::fs::create_dir_all(&junk_then_header).unwrap();
        let file = junk_then_header.join("a.jsonl");
        let header_line = header_line_for("s_after_junk", "/x");
        std::fs::write(&file, format!("\n   \nnot json\n{header_line}\n")).unwrap();
        assert!(matches!(
            load_entries_from_file(&file).unwrap().first(),
            Some(SessionEntry::Session(_))
        ));
        assert_eq!(find_most_recent_session(&junk_then_header), Some(file));

        // Message-first: the loader treats it as a non-session, so
        // discovery must skip it too.
        let message_first = dir.path().join("message-first");
        std::fs::create_dir_all(&message_first).unwrap();
        let file = message_first.join("b.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"message\",\"data\":{\"id\":\"e_1\",\"message\":{\"role\":\"user\",\"content\":\"hi\"},\"timestamp\":1}}\n",
        )
        .unwrap();
        assert!(load_entries_from_file(&file).unwrap().is_empty());
        assert!(find_most_recent_session(&message_first).is_none());
    }

    /// Pins the "bounded" in bounded header scan: a header buried past
    /// [`MAX_SESSION_HEADER_SCAN_BYTES`] of junk is not discovered even
    /// though a full-file load would find it — proving discovery never
    /// falls back to reading whole session bodies.
    #[test]
    fn header_scan_stops_at_byte_cap() {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();

        let junk_line = "leading junk before the real header\n";
        let junk_lines = (MAX_SESSION_HEADER_SCAN_BYTES as usize / junk_line.len()) + 2;
        let mut content = junk_line.repeat(junk_lines);
        content.push_str(&header_line_for("s_beyond_cap", "/x"));
        content.push('\n');
        let path = session_dir.join("buried.jsonl");
        std::fs::write(&path, &content).unwrap();

        // The full loader tolerates any amount of leading junk...
        assert!(matches!(
            load_entries_from_file(&path).unwrap().first(),
            Some(SessionEntry::Session(_))
        ));
        // ...but the bounded scan gives up at the cap, so discovery
        // (deliberately) does not see this pathological file.
        assert!(find_most_recent_session(&session_dir).is_none());
    }

    /// `most_recent_session_path` honours the `--session-dir` override
    /// and returns `None` (not an error) when nothing is there — the
    /// discovery half of what `continue_recent_in` pins end-to-end.
    #[test]
    fn most_recent_session_path_honours_override_dir() {
        let cwd = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();

        assert!(
            SessionManager::most_recent_session_path(cwd.path(), Some(override_dir.path()))
                .is_none()
        );

        let file = override_dir.path().join("s_ovr.jsonl");
        std::fs::write(&file, header_line_for("s_ovr", "/x") + "\n").unwrap();

        assert_eq!(
            SessionManager::most_recent_session_path(cwd.path(), Some(override_dir.path())),
            Some(file)
        );
    }

    #[test]
    fn test_fork_from_preserves_all_non_header_entries() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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

    /// Regression for #77 part 2: `fork_from_in` with an explicit
    /// session_dir must write the new fork file UNDER that dir, not
    /// the home-based default. Without this, `hand --fork <id>
    /// --session-dir <X>` lands the fork in ~/.hand/... and the
    /// subsequent --session-dir-aware resume lookup fails with
    /// "Session <new-id> not found".
    #[test]
    fn fork_from_in_with_session_dir_writes_under_override() {
        let cwd = TempDir::new().unwrap();
        let _g = scoped_hand_home(cwd.path());
        let mut source = SessionManager::create(cwd.path()).unwrap();
        source
            .append_message(Message::User(UserMessage::new_text("seed")))
            .unwrap();
        let source_path = source.path().to_path_buf();

        // Forks must land under override_dir, NOT the home-based default.
        let override_dir_tmp = TempDir::new().unwrap();
        let override_dir = override_dir_tmp.path().to_path_buf();
        let forked = SessionManager::fork_from_in(&source_path, cwd.path(), Some(&override_dir))
            .expect("fork_from_in");
        let forked_path = forked.path().to_path_buf();

        assert!(
            forked_path.starts_with(&override_dir),
            "fork file must live under --session-dir override: got {forked_path:?}, expected under {override_dir:?}"
        );
        assert_eq!(forked.session_dir(), override_dir);
        // Sanity: actually on disk under the override.
        assert!(
            forked_path.exists(),
            "fork file not flushed: {forked_path:?}"
        );
    }

    /// `fork_from` (no session_dir override) preserves the historic
    /// behaviour of writing under the home-based default. Pin the
    /// no-regression path explicitly.
    #[test]
    fn fork_from_without_session_dir_writes_under_home_default() {
        let dir = TempDir::new().unwrap();
        let _g = scoped_hand_home(dir.path());
        let mut source = SessionManager::create(dir.path()).unwrap();
        source
            .append_message(Message::User(UserMessage::new_text("seed")))
            .unwrap();
        let source_path = source.path().to_path_buf();

        let target_dir = TempDir::new().unwrap();
        let forked = SessionManager::fork_from(&source_path, target_dir.path()).unwrap();
        let expected_dir = SessionManager::default_session_dir(target_dir.path());
        assert!(
            forked.path().starts_with(&expected_dir),
            "fork must land under home-based default: got {:?}, expected under {expected_dir:?}",
            forked.path()
        );
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
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
        let _g = scoped_hand_home(dir.path());
        let session_dir = SessionManager::default_session_dir(dir.path());
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
        let _g = scoped_hand_home(dir.path());
        let session_dir = SessionManager::default_session_dir(dir.path());
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
        let _g = scoped_hand_home(dir.path());
        SessionManager::create(dir.path()).unwrap();
        let session_dir = SessionManager::default_session_dir(dir.path());
        std::fs::write(session_dir.join("garbage.jsonl"), "not json\n").unwrap();

        let listed = SessionManager::list(dir.path()).unwrap();
        assert_eq!(listed.len(), 1, "corrupted file should be skipped");
    }

    #[test]
    fn test_list_all_finds_sessions_across_projects() {
        let root = TempDir::new().unwrap();
        let _g = scoped_hand_home(root.path());

        let proj_a = root.path().join("a");
        std::fs::create_dir_all(&proj_a).unwrap();
        let _a = SessionManager::create(&proj_a).unwrap();

        let proj_b = root.path().join("b");
        std::fs::create_dir_all(&proj_b).unwrap();
        let _b = SessionManager::create(&proj_b).unwrap();

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
        // Pin HAND_HOME so `create` writes (and `list_all` reads)
        // under root rather than the user's real ~/.hand/.
        let _g = scoped_hand_home(root.path());
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

    /// UC-sm-002 — `create_in(cwd, custom_dir)` is the agent-dir
    /// override surface: callers that need to redirect persistence
    /// away from `~/.hand/sessions` pass an explicit `session_dir`.
    /// The session file lands inside that directory verbatim.
    #[test]
    fn create_in_persists_under_explicit_session_dir() {
        let cwd = TempDir::new().unwrap();
        let custom = TempDir::new().unwrap();
        let custom_dir = custom.path().join("sessions");
        let sm = SessionManager::create_in(cwd.path(), &custom_dir).expect("create_in");
        assert!(
            sm.path().starts_with(&custom_dir),
            "session file must live under the custom dir: {:?} vs {:?}",
            sm.path(),
            custom_dir
        );
        assert!(
            sm.path().exists(),
            "session file should have been written: {:?}",
            sm.path()
        );
        assert_eq!(sm.session_dir(), custom_dir);
    }

    /// Regression for #76: many concurrent `create_in` calls against the
    /// same `--session-dir` must each land on its own JSONL file. The
    /// pre-fix id format `s_<ms>_<counter>` collided whenever N
    /// processes minted ids in the same millisecond -- they all
    /// produced `s_<ts>_0` and then clobbered each other's session
    /// state. The new shape `s_<ms>_<counter>_<rand64>` plus a
    /// belt-and-braces collision check at `mint_unique_session_path`
    /// keeps every concurrent session isolated.
    ///
    /// Eight in-process threads stand in for eight cross-process
    /// invocations -- they share the same atomic counter (so the
    /// per-thread counter slots are even more crowded than two
    /// genuinely independent processes would be) and they all hit
    /// the same dir. If unique ids were not guaranteed across the
    /// shared counter+ms bucket the test would fail by producing
    /// fewer than 8 distinct files.
    #[test]
    fn create_in_is_collision_free_under_concurrent_invocations() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let session_dir = session_dir.path().to_path_buf();
        std::fs::create_dir_all(&session_dir).unwrap();

        let cwd_path = cwd.path().to_path_buf();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cwd_path = cwd_path.clone();
            let session_dir = session_dir.clone();
            handles.push(std::thread::spawn(move || {
                SessionManager::create_in(&cwd_path, &session_dir)
                    .expect("concurrent create_in")
                    .path()
                    .to_path_buf()
            }));
        }
        let mut paths: Vec<std::path::PathBuf> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();
        paths.sort();
        let unique = {
            let mut p = paths.clone();
            p.dedup();
            p
        };
        assert_eq!(
            paths.len(),
            unique.len(),
            "concurrent create_in produced duplicate session files: {paths:?}"
        );
        for path in &paths {
            assert!(path.exists(), "session file not on disk: {path:?}");
        }
    }

    /// Regression for #76 part 2: the helper itself must never hand
    /// back an existing path. Pre-create a file at the candidate
    /// location's exact name and assert `mint_unique_session_path`
    /// re-rolls onto a different id rather than returning the
    /// already-occupied one.
    #[test]
    fn mint_unique_session_path_rerolls_when_candidate_exists() {
        let session_dir = TempDir::new().unwrap();
        // First call: get a fresh (id, path).
        let (occupied_id, occupied_path) =
            mint_unique_session_path(session_dir.path()).expect("first mint");
        std::fs::write(&occupied_path, b"prior session content").unwrap();
        // Second call: must NOT hand back the same path.
        let (next_id, next_path) =
            mint_unique_session_path(session_dir.path()).expect("second mint");
        assert_ne!(occupied_id, next_id, "ids must differ across mints");
        assert_ne!(
            occupied_path, next_path,
            "second mint reused an occupied path: {next_path:?}"
        );
        assert!(
            !next_path.exists(),
            "second mint handed back an existing path"
        );
    }

    /// UC-sm-003 — a SessionManager handed to the runtime stays the
    /// same instance — no shadow copy is made. We pin this by
    /// reading the header out of the opened manager and checking the
    /// path matches the on-disk file we constructed.
    #[test]
    fn open_preserves_session_manager_identity() {
        let cwd = TempDir::new().unwrap();
        let _g = scoped_hand_home(cwd.path());
        let sm = SessionManager::create(cwd.path()).expect("create");
        let path_at_creation = sm.path().to_path_buf();
        let id_at_creation = sm.id().to_string();

        let reopened = SessionManager::open(&path_at_creation).expect("open");
        // Same on-disk identity, same path. The reopened manager is a
        // fresh struct but references the same file the caller passed
        // in — no shadow rewrite to a different dir.
        assert_eq!(reopened.path(), path_at_creation);
        assert_eq!(reopened.id(), id_at_creation);
    }

    /// UC-sm-005 — a persisted session whose stored cwd is missing on
    /// disk surfaces as a `SessionCwdIssue` from
    /// `get_missing_session_cwd_issue`. We supply a `SessionCwdSource`
    /// adapter pointing at the on-disk session and a fallback cwd.
    #[test]
    fn missing_cwd_detection_returns_issue_when_stored_cwd_is_gone() {
        use crate::core::session_cwd::{SessionCwdSource, get_missing_session_cwd_issue};

        let dir = TempDir::new().unwrap();
        let session_path = dir.path().join("session.jsonl");
        let bad_cwd = "/definitely-not-here-uc-sm-005";
        let header = format!(
            "{{\"type\":\"session\",\"data\":{{\"version\":3,\"id\":\"uc-sm-005\",\"timestamp\":0,\"cwd\":\"{}\"}}}}\n",
            bad_cwd
        );
        std::fs::write(&session_path, header).unwrap();

        struct StubSource {
            cwd: PathBuf,
            session_file: PathBuf,
        }
        impl SessionCwdSource for StubSource {
            fn cwd(&self) -> Option<PathBuf> {
                Some(self.cwd.clone())
            }
            fn session_file(&self) -> Option<PathBuf> {
                Some(self.session_file.clone())
            }
        }
        let src = StubSource {
            cwd: PathBuf::from(bad_cwd),
            session_file: session_path,
        };
        let fallback = dir.path().to_path_buf();
        let issue = get_missing_session_cwd_issue(&src, &fallback)
            .expect("missing cwd must surface an issue");
        assert_eq!(issue.session_cwd, PathBuf::from(bad_cwd));
        assert_eq!(issue.fallback_cwd, fallback);
    }

    /// The store-mediated writer must be format-identical to the
    /// previous direct writer: every on-disk line equals the typed
    /// enum's own serde serialization, byte for byte, across every
    /// append surface.
    #[test]
    fn store_writer_is_format_identical_to_typed_serde() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create_in(cwd.path(), session_dir.path()).unwrap();
        let msg_id = mgr
            .append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        mgr.append_model_change("acme", "acme-large").unwrap();
        mgr.append_compaction("rolled-up", &msg_id).unwrap();
        mgr.append_label("pinned").unwrap();

        let expected: String = mgr
            .entries()
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        let on_disk = std::fs::read_to_string(mgr.path()).unwrap();
        assert_eq!(
            on_disk, expected,
            "on-disk JSONL must be byte-identical to typed-enum serde"
        );
    }

    /// `create` persists the header immediately through the store: the
    /// file exists with exactly the header line and nothing else — no
    /// separate flush step involved.
    #[test]
    fn create_writes_exactly_the_header_line() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let mgr = SessionManager::create_in(cwd.path(), session_dir.path()).unwrap();

        let content = std::fs::read_to_string(mgr.path()).unwrap();
        let expected =
            serde_json::to_string(&SessionEntry::Session(mgr.header().clone())).unwrap() + "\n";
        assert_eq!(content, expected);
    }

    /// An in-memory manager performs zero disk IO for a create+append
    /// sequence: the HAND_HOME-rooted session tree is never created.
    #[test]
    fn in_memory_manager_does_no_disk_io() {
        let home = TempDir::new().unwrap();
        let _g = scoped_hand_home(home.path());

        let mut mgr = SessionManager::in_memory();
        mgr.append_message(Message::User(UserMessage::new_text("hi")))
            .unwrap();
        mgr.append_model_change("acme", "acme-large").unwrap();
        mgr.append_label("ephemeral").unwrap();

        assert!(mgr.is_in_memory());
        assert!(mgr.path().as_os_str().is_empty());
        assert_eq!(mgr.message_count(), 1);
        assert!(
            !home.path().join(".hand").exists(),
            "in-memory session must not touch the session tree"
        );
    }

    // ------------------------------------------------------------------
    // sqlite backend
    // ------------------------------------------------------------------

    #[test]
    fn sqlite_create_append_reopen_by_id() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();
        assert_eq!(mgr.backend(), SessionBackend::Sqlite);
        assert!(mgr.path().ends_with(SQLITE_DB_FILENAME));
        assert!(session_dir.path().join(SQLITE_DB_FILENAME).exists());

        let id = mgr.id().to_string();
        mgr.append_message(Message::User(UserMessage::new_text("hello")))
            .unwrap();
        mgr.append_label("named").unwrap();

        let reopened =
            SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), &id).unwrap();
        assert_eq!(reopened.id(), id);
        assert_eq!(reopened.message_count(), 1);
        assert_eq!(reopened.label(), Some("named"));

        // Storage is the database — no .jsonl file materialises.
        let jsonl_count = std::fs::read_dir(session_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
            .count();
        assert_eq!(jsonl_count, 0);
    }

    #[test]
    fn sqlite_continue_recent_picks_newest() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let mut older = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();

        let key = SessionManager::most_recent_session_key_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            Some(session_dir.path()),
        )
        .expect("sessions present");
        assert_eq!(key, newer.id());

        // Appending to the older session (later entry timestamp) makes
        // it the most recent again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        older
            .append_message(Message::User(UserMessage::new_text("bump")))
            .unwrap();
        let key = SessionManager::most_recent_session_key_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            Some(session_dir.path()),
        )
        .expect("sessions present");
        assert_eq!(key, older.id());
    }

    #[test]
    fn sqlite_prefix_resolution_unique_and_ambiguous() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let a = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();
        let _b = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();
        let a_id = a.id().to_string();

        // Unique prefix (full id minus the random tail's last chars).
        let prefix = &a_id[..a_id.len() - 2];
        let opened =
            SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), prefix)
                .unwrap();
        assert_eq!(opened.id(), a_id);

        // "s_" matches both sessions — ambiguous.
        let err =
            match SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), "s_") {
                Err(e) => e,
                Ok(_) => panic!("ambiguous prefix must error"),
            };
        assert!(err.to_string().contains("ambiguous"), "got: {err}");

        // No match at all.
        let err = match SessionManager::open_by_id_in(
            SessionBackend::Sqlite,
            session_dir.path(),
            "zzz",
        ) {
            Err(e) => e,
            Ok(_) => panic!("unknown id must error"),
        };
        assert!(
            err.to_string().contains("No session found matching"),
            "got: {err}"
        );
    }

    #[test]
    fn sqlite_fork_preserves_entries_and_provenance() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let mut source = SessionManager::create_in_with_backend(
            SessionBackend::Sqlite,
            cwd.path(),
            session_dir.path(),
        )
        .unwrap();
        let msg_id = source
            .append_message(Message::User(UserMessage::new_text("first")))
            .unwrap();
        source.append_model_change("acme", "acme-large").unwrap();
        source.append_label("named").unwrap();
        let source_id = source.id().to_string();

        let forked =
            SessionManager::fork_in_sqlite(cwd.path(), Some(session_dir.path()), &source_id)
                .unwrap();
        assert_ne!(forked.id(), source_id);
        assert_eq!(forked.backend(), SessionBackend::Sqlite);
        assert_eq!(
            forked.header().parent_session.as_deref(),
            Some(source_id.as_str())
        );
        // Body entries preserved with original ids.
        assert!(
            forked
                .entries()
                .iter()
                .any(|e| matches!(e, SessionEntry::Message { id, .. } if id == &msg_id))
        );
        assert!(forked.entries().iter().any(
            |e| matches!(e, SessionEntry::ModelChange { provider, .. } if provider == "acme")
        ));

        // The fork is durable: reopen by its id.
        let reopened =
            SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), forked.id())
                .unwrap();
        assert_eq!(reopened.message_count(), 1);
    }

    #[test]
    fn sqlite_adopts_existing_jsonl_sessions_once_and_leaves_files_alone() {
        let cwd = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();

        // Seed a jsonl-backed session with one message.
        let mut jsonl_mgr = SessionManager::create_in(cwd.path(), session_dir.path()).unwrap();
        jsonl_mgr
            .append_message(Message::User(UserMessage::new_text("adopted")))
            .unwrap();
        let jsonl_id = jsonl_mgr.id().to_string();
        let jsonl_path = jsonl_mgr.path().to_path_buf();
        let bytes_before = std::fs::read(&jsonl_path).unwrap();

        // First sqlite open imports it.
        let adopted =
            SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), &jsonl_id)
                .unwrap();
        assert_eq!(adopted.id(), jsonl_id);
        assert_eq!(adopted.message_count(), 1);

        // Second open: idempotent — nothing duplicated.
        let again =
            SessionManager::open_by_id_in(SessionBackend::Sqlite, session_dir.path(), &jsonl_id)
                .unwrap();
        assert_eq!(again.message_count(), 1);

        // The source .jsonl file is byte-for-byte untouched.
        assert_eq!(std::fs::read(&jsonl_path).unwrap(), bytes_before);
    }

    #[test]
    fn sqlite_list_populates_session_info_fields() {
        let home = TempDir::new().unwrap();
        let _g = scoped_hand_home(home.path());
        let cwd = TempDir::new().unwrap();

        let mut mgr =
            SessionManager::create_with_backend(SessionBackend::Sqlite, cwd.path()).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("hello world")))
            .unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("second")))
            .unwrap();
        mgr.append_label("My Project").unwrap();

        let listed = SessionManager::list_with_backend(SessionBackend::Sqlite, cwd.path()).unwrap();
        assert_eq!(listed.len(), 1);
        let info = &listed[0];
        assert_eq!(info.id, mgr.id());
        assert_eq!(info.message_count, 2);
        assert_eq!(info.first_message, "hello world");
        assert_eq!(info.name.as_deref(), Some("My Project"));
        assert!(info.all_messages_text.contains("second"));
        assert_eq!(info.parent_session_path, None);
        // Under sqlite the path column carries the database path.
        assert!(info.path.ends_with(SQLITE_DB_FILENAME));
    }
}
