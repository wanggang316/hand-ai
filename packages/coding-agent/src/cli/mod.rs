//! CLI surface for the `hand` binary.

pub mod args;
pub mod initial_message;
pub mod list_models;

pub use args::Args;
pub use initial_message::{InitialMessageInput, InitialMessageResult, build_initial_message};
pub use list_models::list_models as print_model_list;
