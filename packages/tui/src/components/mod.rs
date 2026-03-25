//! Built-in UI components.

pub mod autocomplete;
pub mod box_component;
pub mod editor;
pub mod input;
pub mod loader;
pub mod markdown;
pub mod progress_bar;
pub mod select_list;
pub mod spacer;
pub mod status_bar;
pub mod text;
pub mod toast;
pub mod truncated_text;

pub use autocomplete::{AutocompleteComponent, Suggestion};
pub use box_component::BoxComponent;
pub use editor::EditorComponent;
pub use input::InputComponent;
pub use loader::LoaderComponent;
pub use markdown::MarkdownComponent;
pub use progress_bar::ProgressBarComponent;
pub use select_list::{SelectItem, SelectListComponent};
pub use spacer::SpacerComponent;
pub use status_bar::StatusBarComponent;
pub use text::TextComponent;
pub use toast::{ToastComponent, ToastLevel};
pub use truncated_text::TruncatedTextComponent;
