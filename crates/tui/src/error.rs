//! Error types for the TUI runtime.

use thiserror::Error;

/// Errors that can surface from `Tui::run` and the supporting machinery.
#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("terminal raw mode unavailable: {0}")]
    RawMode(String),
    #[error("stdin reader exited unexpectedly")]
    StdinClosed,
    #[error("internal channel error: {0}")]
    Channel(String),
}

/// Convenient `Result` alias used throughout the runtime.
pub type TuiResult<T> = Result<T, TuiError>;
