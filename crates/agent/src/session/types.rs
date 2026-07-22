//! Data types shared by every session-storage backend.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// On-disk session format version. Matches the on-disk format version
/// written by the hand session manager, so files produced by either
/// side stay mutually readable.
pub const SESSION_FORMAT_VERSION: u32 = 3;

/// One line of a session log. On disk this is the tagged envelope
/// `{"type": <kind>, "data": <payload>}`; the header line uses kind
/// `"session"`. Kinds are open-ended strings so embedders can define
/// their own entry types without this crate enumerating them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "data")]
    pub payload: serde_json::Value,
}

impl SessionEntry {
    /// Build an entry from a kind and its payload.
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }

    /// Entry id, read from `payload["id"]`. The payload is the single
    /// source of truth — there is no duplicated field on the envelope.
    pub fn id(&self) -> Option<&str> {
        self.payload.get("id").and_then(|v| v.as_str())
    }

    /// Parent entry id, read from `payload["parent_id"]`.
    pub fn parent_id(&self) -> Option<&str> {
        self.payload.get("parent_id").and_then(|v| v.as_str())
    }

    /// Entry timestamp (millis since epoch), read from
    /// `payload["timestamp"]`.
    pub fn timestamp(&self) -> Option<i64> {
        self.payload.get("timestamp").and_then(|v| v.as_i64())
    }

    /// Whether this entry is the session header envelope.
    pub fn is_header(&self) -> bool {
        self.kind == "session"
    }
}

/// Session header — the payload of the first line of a session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub version: u32,
    pub id: String,
    pub timestamp: i64,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// Unknown header fields survive round-trips.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Listing row: header plus backend bookkeeping, no entries.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub header: SessionHeader,
    /// Milliseconds since epoch of the last append (backend-defined).
    pub updated_ms: i64,
}

/// Errors surfaced by [`crate::session::SessionStore`] backends.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    /// No session exists under the given id.
    #[error("Session not found: {0}")]
    NotFound(String),

    /// The stored log cannot be interpreted as a session.
    #[error("Corrupt session '{session}': {detail}")]
    Corrupt { session: String, detail: String },

    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Invalid request (e.g. creating a session id that already exists).
    #[error("Invalid: {0}")]
    Invalid(String),

    /// Backend-specific failure (e.g. a database error).
    #[error("Backend error: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entry_accessors_read_payload_fields() {
        let entry = SessionEntry::new(
            "message",
            json!({"id": "e_1", "parent_id": "e_0", "timestamp": 42}),
        );
        assert_eq!(entry.id(), Some("e_1"));
        assert_eq!(entry.parent_id(), Some("e_0"));
        assert_eq!(entry.timestamp(), Some(42));
        assert!(!entry.is_header());
    }

    #[test]
    fn entry_accessors_absent_fields_are_none() {
        let entry = SessionEntry::new("session", json!({"cwd": "/tmp"}));
        assert_eq!(entry.id(), None);
        assert_eq!(entry.parent_id(), None);
        assert_eq!(entry.timestamp(), None);
        assert!(entry.is_header());
    }

    #[test]
    fn entry_envelope_serializes_as_type_and_data() {
        let entry = SessionEntry::new("label", json!({"id": "e_9"}));
        let line = serde_json::to_string(&entry).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "label");
        assert_eq!(value["data"]["id"], "e_9");
    }

    #[test]
    fn header_round_trip_preserves_unknown_fields() {
        let line = r#"{"version":3,"id":"s_1","timestamp":1,"cwd":"/tmp","future_field":true}"#;
        let header: SessionHeader = serde_json::from_str(line).unwrap();
        assert_eq!(header.extra["future_field"], json!(true));

        let reserialized = serde_json::to_string(&header).unwrap();
        let round: SessionHeader = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(round, header);
        assert!(reserialized.contains("future_field"));
    }

    #[test]
    fn header_without_parent_session_omits_the_field() {
        let header = SessionHeader {
            version: SESSION_FORMAT_VERSION,
            id: "s_1".into(),
            timestamp: 1,
            cwd: "/tmp".into(),
            parent_session: None,
            extra: serde_json::Map::new(),
        };
        let line = serde_json::to_string(&header).unwrap();
        assert!(!line.contains("parent_session"));
    }
}
