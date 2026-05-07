//! CLI surface for the `hand` binary.

pub mod args;
pub mod file_processor;
pub mod initial_message;
pub mod list_models;

pub use args::Args;
pub use file_processor::{FileProcessorError, ProcessedFiles, process_file_arguments};
pub use initial_message::{InitialMessageInput, InitialMessageResult, build_initial_message};
pub use list_models::list_models as print_model_list;
