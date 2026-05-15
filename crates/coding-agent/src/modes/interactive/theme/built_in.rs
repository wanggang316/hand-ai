//! Built-in themes (`dark`, `light`).
//!
//! The JSON payloads ship with the binary via `include_str!`. Loaded
//! eagerly because the data is tiny.

use std::sync::OnceLock;

use super::color::ColorMode;
use super::core::{Theme, ThemeError, ThemeJson};

/// Raw `dark.json` source, embedded at compile time.
pub fn dark_theme_json_str() -> &'static str {
    include_str!("dark.json")
}

/// Raw `light.json` source, embedded at compile time.
pub fn light_theme_json_str() -> &'static str {
    include_str!("light.json")
}

/// Names of every theme baked into the binary.
pub const BUILTIN_THEME_NAMES: &[&str] = &["dark", "light"];

fn dark_json() -> &'static ThemeJson {
    static CELL: OnceLock<ThemeJson> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::from_str(dark_theme_json_str())
            .expect("built-in dark.json must be valid theme JSON")
    })
}

fn light_json() -> &'static ThemeJson {
    static CELL: OnceLock<ThemeJson> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::from_str(light_theme_json_str())
            .expect("built-in light.json must be valid theme JSON")
    })
}

/// Look up a built-in theme JSON by name. Returns `None` for unknown names.
pub fn builtin_theme_json(name: &str) -> Option<&'static ThemeJson> {
    match name {
        "dark" => Some(dark_json()),
        "light" => Some(light_json()),
        _ => None,
    }
}

/// Build the `dark` theme using the supplied (or auto-detected) colour mode.
pub fn dark_theme(mode: Option<ColorMode>) -> Result<Theme, ThemeError> {
    Theme::from_json(dark_json(), mode)
}

/// Build the `light` theme using the supplied (or auto-detected) colour mode.
pub fn light_theme(mode: Option<ColorMode>) -> Result<Theme, ThemeError> {
    Theme::from_json(light_json(), mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_json_parses() {
        let json = dark_json();
        assert_eq!(json.name, "dark");
    }

    #[test]
    fn light_json_parses() {
        let json = light_json();
        assert_eq!(json.name, "light");
    }

    #[test]
    fn builtin_lookup_known_and_unknown() {
        assert!(builtin_theme_json("dark").is_some());
        assert!(builtin_theme_json("light").is_some());
        assert!(builtin_theme_json("nonexistent").is_none());
    }

    #[test]
    fn dark_and_light_themes_construct() {
        let _ = dark_theme(Some(ColorMode::Truecolor)).unwrap();
        let _ = light_theme(Some(ColorMode::Color256)).unwrap();
    }

    #[test]
    fn builtin_theme_names_listed() {
        assert!(BUILTIN_THEME_NAMES.contains(&"dark"));
        assert!(BUILTIN_THEME_NAMES.contains(&"light"));
    }
}
