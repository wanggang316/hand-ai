//! Hand TUI — Terminal UI framework built on the ratatui runtime.
//!
//! Provides:
//! - The [`rt`] runtime: session guard, frame scheduler, inline viewport, and
//!   the rt component/widget set (editor, lists, markdown, images, overlays)
//! - Fuzzy matching ([`fuzzy`])
//! - Terminal image protocols ([`terminal_image`])
//! - ANSI-aware text utilities: width, wrapping, truncation ([`utils`])
//! - A canonical [`keys::KeyId`] key-identifier alias used by the rt input pipeline

pub mod fuzzy;
pub mod keys;
pub mod rt;
pub mod terminal_image;
pub mod utils;

// Re-export commonly used items
pub use fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};
pub use keys::KeyId;
pub use terminal_image::{
    CellDimensions, ImageDimensions, ImageRenderOptions, TerminalImageCapabilities,
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_webp_dimensions, hyperlink, image_fallback, render_image, reset_capabilities_cache,
    set_capabilities, set_cell_dimensions,
};
pub use utils::{truncate_to_width, visible_width, wrap_text};
