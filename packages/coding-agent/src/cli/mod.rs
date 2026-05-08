//! CLI surface for the `hand` binary.

pub mod args;
pub mod config_selector;
pub mod file_processor;
pub mod initial_message;
pub mod list_models;
pub mod session_picker;

pub use args::Args;
pub use config_selector::{
    ConfigSelectorCliError, ConfigSelectorOutcome, ToggleRecord, select_config,
};
pub use file_processor::{FileProcessorError, ProcessedFiles, process_file_arguments};
pub use initial_message::{InitialMessageInput, InitialMessageResult, build_initial_message};
pub use list_models::list_models as print_model_list;
pub use session_picker::{SessionPickerError, select_session};
