//! Hand TUI — Terminal UI framework with differential rendering.
//!
//! Provides a component-based terminal UI system with:
//! - Differential rendering (only changed lines are written)
//! - Component model with render/input handling
//! - Built-in components: Text, Box, Input, Editor, Markdown, SelectList, Loader,
//!   StatusBar, ProgressBar, Toast, Autocomplete
//! - Theme system with ANSI colors and styles
//! - Keyboard handling with Kitty protocol support
//! - ANSI-aware text utilities (width, wrapping, truncation)

pub mod components;
pub mod keys;
pub mod render;
pub mod terminal;
pub mod theme;
pub mod tui;
pub mod utils;

// Re-export commonly used items
pub use components::{
    AutocompleteComponent, BoxComponent, EditorComponent, InputComponent, LoaderComponent,
    MarkdownComponent, ProgressBarComponent, SelectItem, SelectListComponent, SpacerComponent,
    StatusBarComponent, Suggestion, TextComponent, ToastComponent, ToastLevel,
    TruncatedTextComponent,
};
pub use keys::{Key, KeyModifiers, parse_key};
pub use render::DiffRenderer;
pub use terminal::{Terminal, TerminalCapabilities};
pub use theme::{Color, NamedColor, Style, Theme};
pub use tui::{Component, Container, Focusable, Tui};
pub use utils::{truncate_to_width, visible_width, wrap_text};
