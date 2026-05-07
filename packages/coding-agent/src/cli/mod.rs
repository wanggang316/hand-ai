//! CLI surface for the `hand` binary.

pub mod args;
pub mod initial_message;

pub use args::Args;
pub use initial_message::{InitialMessageInput, InitialMessageResult, build_initial_message};
