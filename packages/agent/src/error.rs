//! Error types for the agent crate.

use thiserror::Error;

/// Errors that can occur during agent execution.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The model client returned an error.
    #[error("Client error: {0}")]
    Client(#[from] model::ClientError),

    /// The agent was aborted via its cancellation token.
    #[error("Agent aborted")]
    Aborted,

    /// Tool argument validation failed against the tool's JSON schema.
    #[error("Tool '{tool_name}' argument validation failed: {message}")]
    SchemaValidation { tool_name: String, message: String },

    /// The stream ended without producing a final assistant message.
    #[error("Agent stream ended without a final assistant message")]
    StreamEndedWithoutResult,

    /// Misuse of the API: invalid state transition.
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Other unrecoverable error (used for lifecycle failures).
    #[error("Agent error: {0}")]
    Other(String),
}
