//! Diagnostics emitted alongside assistant messages.
//!
//! Replaces the M1 stub with the structured form: a `kind` enum (kebab-case
//! on the wire), a free-form `message`, optional `details` JSON payload, and
//! a millisecond Unix timestamp serialized as `timestampMs`.

use serde::{Deserialize, Serialize};

/// Kind of diagnostic. Serialized as kebab-case strings to match the TS shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticKind {
    /// A retry was attempted after a recoverable error.
    Retry,
    /// A previous error was recovered from.
    Recovered,
    /// A tool call was emitted without a signature where one was expected.
    UnsignedToolCall,
    /// The outbound payload was downgraded (e.g. dropped unsupported fields).
    PayloadDowngraded,
    /// An event of an unknown type was received from the provider.
    UnknownEvent,
    /// The provider returned an error.
    ProviderError,
}

/// Structured diagnostic attached to an `AssistantMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    /// Categorical kind of the diagnostic.
    pub kind: DiagnosticKind,
    /// Human-readable message describing what happened.
    pub message: String,
    /// Optional structured payload with extra context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Millisecond Unix timestamp at which the diagnostic was recorded.
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: u64,
}

impl AssistantMessageDiagnostic {
    /// Construct a new diagnostic, stamping the current time.
    pub fn new(kind: DiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
            timestamp_ms: now_ms(),
        }
    }

    /// Attach a structured details payload, returning the modified diagnostic.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
