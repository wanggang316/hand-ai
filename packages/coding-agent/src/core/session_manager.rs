//! Session management — JSONL-based session persistence.

use crate::core::error::CodingAgentError;
use chrono::Utc;
use model::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
        message: Message,
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
            version: 3,
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
        let content = std::fs::read_to_string(path)
            .map_err(|e| CodingAgentError::Session(format!("Failed to read session: {}", e)))?;

        let mut entries = Vec::new();
        let mut header = None;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<SessionEntry>(line) {
                if let SessionEntry::Session(h) = &entry {
                    header = Some(h.clone());
                }
                entries.push(entry);
            }
        }

        let header =
            header.ok_or_else(|| CodingAgentError::Session("No session header found".into()))?;

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
            version: 3,
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
            message,
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
                messages.push(message.clone());
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
            version: 3,
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

        sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
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
}
