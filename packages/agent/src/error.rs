//! Error types for the agent crate.

use thiserror::Error;

/// Errors that can occur during agent execution.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The model client returned an error.
    #[error("Client error: {0}")]
    Client(#[from] model::ClientError),

    /// The agent was aborted.
    #[error("Agent aborted")]
    Aborted,

    /// A tool was not found.
    #[error("Tool not found: {name}")]
    ToolNotFound { name: String },

    /// Tool argument validation failed.
    #[error("Tool argument validation failed for {tool_name}: {message}")]
    ToolValidationFailed { tool_name: String, message: String },

    /// The agent loop ended unexpectedly.
    #[error("Agent loop ended without result")]
    LoopEndedWithoutResult,

    /// Cannot continue: invalid state.
    #[error("Cannot continue: {0}")]
    InvalidState(String),

    /// The stream ended with an error.
    #[error("Stream error: {0}")]
    StreamError(String),
}
