//! Error types for the coding agent.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodingAgentError {
    #[error("Agent error: {0}")]
    Agent(#[from] hand_agent::AgentError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    // pi prints session-related errors as a bare message (e.g.
    // "No session found matching 'foo'"); the `Session error:` prefix
    // we used to emit leaked an internal variant tag into user-facing
    // output. Mirror pi's bare form so error messages stay clean.
    #[error("{0}")]
    Session(String),

    #[error("Settings error: {0}")]
    Settings(String),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("Model error: {0}")]
    Model(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
