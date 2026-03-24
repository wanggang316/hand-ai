//! Hand TUI — Terminal UI framework with differential rendering.
//!
//! Provides a component-based terminal UI system with:
//! - Differential rendering (only changed lines are written)
//! - Component model with render/input handling
//! - Built-in components: Text, Box, Input, Editor, Markdown, SelectList, Loader
//! - Keyboard handling with Kitty protocol support
//! - ANSI-aware text utilities (width, wrapping, truncation)

pub mod components;
pub mod keys;
pub mod render;
pub mod terminal;
pub mod tui;
pub mod utils;

// Re-export commonly used items
pub use components::{
    BoxComponent, EditorComponent, InputComponent, LoaderComponent, MarkdownComponent,
    SelectItem, SelectListComponent, SpacerComponent, TextComponent, TruncatedTextComponent,
};
pub use keys::{Key, KeyModifiers, parse_key};
pub use render::DiffRenderer;
pub use terminal::{Terminal, TerminalCapabilities};
pub use tui::{Component, Container, Focusable, Tui};
pub use utils::{truncate_to_width, visible_width, wrap_text};
