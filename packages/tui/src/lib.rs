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
pub mod error;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod kill_ring;
pub mod overlay;
pub mod render;
pub mod resize;
pub mod stdin_buffer;
pub mod terminal;
pub mod theme;
pub mod tui;
pub mod utils;

// Re-export commonly used items
pub use components::{
    AutocompleteComponent, AutocompleteContext, AutocompleteFuture, AutocompleteItem,
    AutocompleteItemKind, AutocompleteProvider, AutocompleteTrigger, BoxComponent,
    CancellableLoaderComponent, CombinedAutocompleteProvider, EditorComponent, ImageComponent,
    ImageProtocol, InputComponent, LoaderComponent, MarkdownComponent, ProgressBarComponent,
    SelectItem, SelectListComponent, SettingEntry, SettingValue, SettingsListComponent,
    SlashCommand, SlashCommandProvider, SpacerComponent, StatusBarComponent, Suggestion,
    TextComponent, ToastComponent, ToastLevel, TruncatedTextComponent,
};
pub use fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};
pub use keybindings::{
    Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingsConfig, KeybindingsManager,
    TUI_KEYBINDINGS, get_keybindings, set_keybindings,
};
pub use keys::{
    Key, KeyEventType, KeyId, KeyModifiers, KeyName, decode_kitty_printable, decode_printable_key,
    is_key_release, is_key_repeat, is_kitty_protocol_active, matches_key, parse_key, parse_key_id,
    set_kitty_protocol_active,
};
pub use kill_ring::KillRing;
pub use overlay::{
    Overlay, OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, OverlayPosition,
    compose_overlays, render_with_overlay,
};
pub use render::DiffRenderer;
pub use resize::watch_resizes;
pub use stdin_buffer::{StdinBuffer, StdinBufferEvent, StdinBufferOptions};
pub use terminal::{ProcessTerminal, Terminal, TerminalCapabilities, run_stdin_reader};
pub use theme::{Color, NamedColor, Style, Theme};
pub use error::{TuiError, TuiResult};
pub use tui::{
    Component, ComponentId, Container, Focusable, HandleResult, InputEvent, InputListener,
    ListenerId, ListenerResult, Tui, input_event_from_str,
};
pub use utils::{truncate_to_width, visible_width, wrap_text};
