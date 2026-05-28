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
pub mod terminal_image;
pub mod theme;
pub mod tui;
pub mod utils;

// Re-export commonly used items
pub use components::{
    AutocompleteComponent, AutocompleteContext, AutocompleteFuture, AutocompleteItem,
    AutocompleteItemKind, AutocompleteProvider, AutocompleteState, AutocompleteTrigger,
    BoxComponent, CancellableLoaderComponent, CombinedAutocompleteProvider,
    DEFAULT_INDICATOR_INTERVAL_MS, DEFAULT_PRIMARY_COLUMN_WIDTH, DEFAULT_SPINNER_FRAMES,
    EditorComponent, ImageComponent, ImageOptions, ImageProtocol, ImageTheme, InputComponent,
    LoaderComponent, LoaderIndicatorOptions, MarkdownComponent, PasteContent,
    PathAutocompleteProvider, ProgressBarComponent, SelectItem, SelectListComponent,
    SelectListLayoutOptions, SelectListTheme, SettingEntry, SettingValue, SettingsListComponent,
    SettingsListTheme, SlashCommand, SlashCommandProvider, SpacerComponent, StatusBarComponent,
    Suggestion, TextComponent, ToastComponent, ToastLevel, TruncatedTextComponent, UndoEntry,
    UndoOp,
};
pub use error::{TuiError, TuiResult};
pub use fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};
pub use keybindings::{
    Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingsConfig, KeybindingsManager,
    TUI_KEYBINDINGS, get_keybindings, set_keybindings,
};
pub use keys::{
    Key, KeyEventType, KeyId, KeyModifiers, KeyName, decode_kitty_printable, decode_printable_key,
    is_key_release, is_key_repeat, is_kitty_protocol_active, key_to_canonical_bytes, matches_key,
    parse_key, parse_key_id, set_kitty_protocol_active,
};
pub use kill_ring::KillRing;
pub use overlay::{
    Overlay, OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, OverlayPosition,
    compose_overlays, render_with_overlay,
};
pub use render::DiffRenderer;
pub use resize::watch_resizes;
pub use stdin_buffer::{StdinBuffer, StdinBufferEvent, StdinBufferOptions};
pub use terminal::{
    ProcessTerminal, Terminal, TerminalCapabilities, TestTerminal, run_stdin_reader,
};
pub use terminal_image::{
    CellDimensions, ImageDimensions, ImageRenderOptions, TerminalImageCapabilities,
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_webp_dimensions, hyperlink, image_fallback, render_image, reset_capabilities_cache,
    set_capabilities, set_cell_dimensions,
};
pub use theme::{Color, NamedColor, Style, Theme};
pub use tui::{
    CURSOR_MARKER, Component, ComponentId, Container, Focusable, HandleResult, InputEvent,
    InputListener, ListenerId, ListenerResult, OverlayMountError, OverlayMountRequest,
    OverlayMounter, Tui, input_event_from_str,
};
pub use utils::{truncate_to_width, visible_width, wrap_text};
