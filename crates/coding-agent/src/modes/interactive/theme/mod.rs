//! Interactive-mode theme system.
//!
//! Phase-4 surface: a fully-resolved [`Theme`] with semantic colour slots
//! ([`ThemeColor`] / [`ThemeBg`]), JSON loader (built-in `dark` / `light`
//! plus `<name>.json` files in `~/.hand/themes/`), and colour-mode
//! detection. The cli-highlight syntax bridge, markdown / select-list
//! adapters, HTML export helpers and the file-watcher hot-reload are
//! tracked as `TODO(parity)` and will land in follow-up units alongside
//! the components that consume them.

pub mod built_in;
pub mod color;
pub mod core;
pub mod loader;

pub use built_in::{
    BUILTIN_THEME_NAMES, builtin_theme_json, dark_theme, dark_theme_json_str, light_theme,
    light_theme_json_str,
};
pub use color::{
    ColorError, ColorMode, ColorValue, ResolvedColor, bg_ansi, detect_color_mode, fg_ansi,
    hex_to_256, hex_to_rgb, rgb_to_256,
};
pub use core::{
    RawColorValue, Theme, ThemeBg, ThemeColor, ThemeColors, ThemeError, ThemeExport, ThemeJson,
    ThinkingLevel,
};
pub use loader::{
    ThemeInfo, ThemeLoadError, available_themes, available_themes_with_paths,
    default_custom_themes_dir, detect_terminal_background, load_theme, load_theme_from_path,
    theme_by_name,
};
