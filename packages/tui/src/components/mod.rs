//! Built-in UI components.

pub mod autocomplete;
pub mod box_component;
pub mod cancellable_loader;
pub mod editor;
pub mod image;
pub mod input;
pub mod loader;
pub mod markdown;
pub mod progress_bar;
pub mod select_list;
pub mod settings_list;
pub mod spacer;
pub mod status_bar;
pub mod text;
pub mod toast;
pub mod truncated_text;

pub use autocomplete::{
    AutocompleteComponent, AutocompleteContext, AutocompleteFuture, AutocompleteItem,
    AutocompleteItemKind, AutocompleteProvider, AutocompleteTrigger, CombinedAutocompleteProvider,
    SlashCommand, SlashCommandProvider, Suggestion,
};
pub use box_component::BoxComponent;
pub use cancellable_loader::CancellableLoaderComponent;
pub use editor::{AutocompleteState, EditorComponent, PasteContent, UndoEntry, UndoOp};
pub use image::{ImageComponent, ImageProtocol};
pub use input::InputComponent;
pub use loader::{
    DEFAULT_INDICATOR_INTERVAL_MS, DEFAULT_SPINNER_FRAMES, LoaderComponent,
    LoaderIndicatorOptions,
};
pub use markdown::MarkdownComponent;
pub use progress_bar::ProgressBarComponent;
pub use select_list::{SelectItem, SelectListComponent};
pub use settings_list::{SettingEntry, SettingValue, SettingsListComponent};
pub use spacer::SpacerComponent;
pub use status_bar::StatusBarComponent;
pub use text::TextComponent;
pub use toast::{ToastComponent, ToastLevel};
pub use truncated_text::TruncatedTextComponent;
