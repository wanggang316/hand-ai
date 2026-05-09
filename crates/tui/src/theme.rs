//! Theme system — colors, styles, and theme definitions.
//!
//! Provides ANSI-based theming for all TUI components.

use serde::{Deserialize, Serialize};

/// ANSI color representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Color {
    /// Named ANSI color.
    Named(NamedColor),
    /// 256-color index.
    Index(u8),
    /// RGB color.
    Rgb { r: u8, g: u8, b: u8 },
    /// Hex color string (e.g., "#7c3aed").
    Hex(String),
}

/// Standard ANSI named colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Default,
}

impl Color {
    /// Convert to ANSI foreground escape sequence.
    pub fn to_fg_ansi(&self) -> String {
        match self {
            Color::Named(c) => format!("\x1b[{}m", c.fg_code()),
            Color::Index(i) => format!("\x1b[38;5;{}m", i),
            Color::Rgb { r, g, b } => format!("\x1b[38;2;{};{};{}m", r, g, b),
            Color::Hex(hex) => {
                let (r, g, b) = parse_hex(hex);
                format!("\x1b[38;2;{};{};{}m", r, g, b)
            }
        }
    }

    /// Convert to ANSI background escape sequence.
    pub fn to_bg_ansi(&self) -> String {
        match self {
            Color::Named(c) => format!("\x1b[{}m", c.bg_code()),
            Color::Index(i) => format!("\x1b[48;5;{}m", i),
            Color::Rgb { r, g, b } => format!("\x1b[48;2;{};{};{}m", r, g, b),
            Color::Hex(hex) => {
                let (r, g, b) = parse_hex(hex);
                format!("\x1b[48;2;{};{};{}m", r, g, b)
            }
        }
    }
}

impl NamedColor {
    fn fg_code(self) -> u8 {
        match self {
            NamedColor::Black => 30,
            NamedColor::Red => 31,
            NamedColor::Green => 32,
            NamedColor::Yellow => 33,
            NamedColor::Blue => 34,
            NamedColor::Magenta => 35,
            NamedColor::Cyan => 36,
            NamedColor::White => 37,
            NamedColor::BrightBlack => 90,
            NamedColor::BrightRed => 91,
            NamedColor::BrightGreen => 92,
            NamedColor::BrightYellow => 93,
            NamedColor::BrightBlue => 94,
            NamedColor::BrightMagenta => 95,
            NamedColor::BrightCyan => 96,
            NamedColor::BrightWhite => 97,
            NamedColor::Default => 39,
        }
    }

    fn bg_code(self) -> u8 {
        match self {
            NamedColor::Black => 40,
            NamedColor::Red => 41,
            NamedColor::Green => 42,
            NamedColor::Yellow => 43,
            NamedColor::Blue => 44,
            NamedColor::Magenta => 45,
            NamedColor::Cyan => 46,
            NamedColor::White => 47,
            NamedColor::BrightBlack => 100,
            NamedColor::BrightRed => 101,
            NamedColor::BrightGreen => 102,
            NamedColor::BrightYellow => 103,
            NamedColor::BrightBlue => 104,
            NamedColor::BrightMagenta => 105,
            NamedColor::BrightCyan => 106,
            NamedColor::BrightWhite => 107,
            NamedColor::Default => 49,
        }
    }
}

fn parse_hex(hex: &str) -> (u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        (r, g, b)
    } else {
        (0, 0, 0)
    }
}

/// Text style attributes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Style {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub dim: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
}

impl Style {
    /// Create a style with just a foreground color.
    pub fn fg(color: Color) -> Self {
        Self {
            fg: Some(color),
            ..Default::default()
        }
    }

    /// Create a style with bold text.
    pub fn bold() -> Self {
        Self {
            bold: true,
            ..Default::default()
        }
    }

    /// Convert to ANSI escape sequence prefix.
    pub fn to_ansi(&self) -> String {
        let mut parts = Vec::new();
        if self.bold {
            parts.push("\x1b[1m".to_string());
        }
        if self.dim {
            parts.push("\x1b[2m".to_string());
        }
        if self.italic {
            parts.push("\x1b[3m".to_string());
        }
        if self.underline {
            parts.push("\x1b[4m".to_string());
        }
        if self.strikethrough {
            parts.push("\x1b[9m".to_string());
        }
        if let Some(ref fg) = self.fg {
            parts.push(fg.to_fg_ansi());
        }
        if let Some(ref bg) = self.bg {
            parts.push(bg.to_bg_ansi());
        }
        parts.join("")
    }

    /// Apply this style to text, returning styled string with reset at end.
    pub fn apply(&self, text: &str) -> String {
        let prefix = self.to_ansi();
        if prefix.is_empty() {
            text.to_string()
        } else {
            format!("{}{}\x1b[0m", prefix, text)
        }
    }
}

/// Complete theme definition for the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Primary accent color (prompts, highlights).
    pub primary: Color,
    /// Secondary accent color (subtle highlights).
    pub secondary: Color,
    /// Error/danger color.
    pub error: Color,
    /// Warning color.
    pub warning: Color,
    /// Success color.
    pub success: Color,
    /// Muted/dim text color.
    pub muted: Color,
    /// User input prompt style.
    pub prompt: Style,
    /// Assistant text style.
    pub assistant: Style,
    /// Tool name style.
    pub tool: Style,
    /// Thinking/reasoning text style.
    pub thinking: Style,
    /// Code/pre block style.
    pub code: Style,
    /// Diff added line style.
    pub diff_add: Style,
    /// Diff removed line style.
    pub diff_remove: Style,
    /// Status bar style.
    pub status_bar: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// Dark theme (default).
    pub fn dark() -> Self {
        Self {
            name: "dark".into(),
            primary: Color::Hex("#7c3aed".into()),
            secondary: Color::Hex("#06b6d4".into()),
            error: Color::Named(NamedColor::Red),
            warning: Color::Named(NamedColor::Yellow),
            success: Color::Named(NamedColor::Green),
            muted: Color::Named(NamedColor::BrightBlack),
            prompt: Style {
                fg: Some(Color::Hex("#7c3aed".into())),
                bold: true,
                ..Default::default()
            },
            assistant: Style::default(),
            tool: Style {
                fg: Some(Color::Named(NamedColor::Cyan)),
                ..Default::default()
            },
            thinking: Style {
                dim: true,
                ..Default::default()
            },
            code: Style {
                fg: Some(Color::Named(NamedColor::Green)),
                ..Default::default()
            },
            diff_add: Style {
                fg: Some(Color::Named(NamedColor::Green)),
                ..Default::default()
            },
            diff_remove: Style {
                fg: Some(Color::Named(NamedColor::Red)),
                ..Default::default()
            },
            status_bar: Style {
                fg: Some(Color::Named(NamedColor::White)),
                bg: Some(Color::Hex("#1a1a2e".into())),
                ..Default::default()
            },
        }
    }

    /// Light theme.
    pub fn light() -> Self {
        Self {
            name: "light".into(),
            primary: Color::Hex("#6d28d9".into()),
            secondary: Color::Hex("#0891b2".into()),
            error: Color::Named(NamedColor::Red),
            warning: Color::Hex("#d97706".into()),
            success: Color::Hex("#059669".into()),
            muted: Color::Named(NamedColor::BrightBlack),
            prompt: Style {
                fg: Some(Color::Hex("#6d28d9".into())),
                bold: true,
                ..Default::default()
            },
            assistant: Style::default(),
            tool: Style {
                fg: Some(Color::Hex("#0891b2".into())),
                ..Default::default()
            },
            thinking: Style {
                dim: true,
                italic: true,
                ..Default::default()
            },
            code: Style {
                fg: Some(Color::Hex("#059669".into())),
                ..Default::default()
            },
            diff_add: Style {
                fg: Some(Color::Hex("#059669".into())),
                ..Default::default()
            },
            diff_remove: Style {
                fg: Some(Color::Named(NamedColor::Red)),
                ..Default::default()
            },
            status_bar: Style {
                fg: Some(Color::Named(NamedColor::Black)),
                bg: Some(Color::Hex("#f3f4f6".into())),
                ..Default::default()
            },
        }
    }

    /// Get a built-in theme by name.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            _ => None,
        }
    }

    /// Load a custom theme from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// ANSI reset sequence.
pub const RESET: &str = "\x1b[0m";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_named_color_fg() {
        let c = Color::Named(NamedColor::Red);
        assert_eq!(c.to_fg_ansi(), "\x1b[31m");
    }

    #[test]
    fn test_named_color_bg() {
        let c = Color::Named(NamedColor::Blue);
        assert_eq!(c.to_bg_ansi(), "\x1b[44m");
    }

    #[test]
    fn test_rgb_color() {
        let c = Color::Rgb {
            r: 255,
            g: 128,
            b: 0,
        };
        assert_eq!(c.to_fg_ansi(), "\x1b[38;2;255;128;0m");
    }

    #[test]
    fn test_hex_color() {
        let c = Color::Hex("#ff8000".into());
        assert_eq!(c.to_fg_ansi(), "\x1b[38;2;255;128;0m");
    }

    #[test]
    fn test_index_color() {
        let c = Color::Index(196);
        assert_eq!(c.to_fg_ansi(), "\x1b[38;5;196m");
    }

    #[test]
    fn test_style_apply() {
        let style = Style {
            fg: Some(Color::Named(NamedColor::Red)),
            bold: true,
            ..Default::default()
        };
        let result = style.apply("hello");
        assert!(result.starts_with("\x1b[1m\x1b[31m"));
        assert!(result.ends_with("\x1b[0m"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_style_empty() {
        let style = Style::default();
        assert_eq!(style.apply("text"), "text");
    }

    #[test]
    fn test_theme_dark() {
        let theme = Theme::dark();
        assert_eq!(theme.name, "dark");
    }

    #[test]
    fn test_theme_light() {
        let theme = Theme::light();
        assert_eq!(theme.name, "light");
    }

    #[test]
    fn test_theme_by_name() {
        assert!(Theme::by_name("dark").is_some());
        assert!(Theme::by_name("light").is_some());
        assert!(Theme::by_name("unknown").is_none());
    }

    #[test]
    fn test_theme_json_roundtrip() {
        let theme = Theme::dark();
        let json = serde_json::to_string(&theme).unwrap();
        let parsed = Theme::from_json(&json).unwrap();
        assert_eq!(parsed.name, "dark");
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex("#ff0080"), (255, 0, 128));
        assert_eq!(parse_hex("7c3aed"), (124, 58, 237));
        assert_eq!(parse_hex(""), (0, 0, 0));
    }

    #[test]
    fn test_style_all_attributes() {
        let style = Style {
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            strikethrough: true,
            fg: None,
            bg: None,
        };
        let ansi = style.to_ansi();
        assert!(ansi.contains("\x1b[1m"));
        assert!(ansi.contains("\x1b[2m"));
        assert!(ansi.contains("\x1b[3m"));
        assert!(ansi.contains("\x1b[4m"));
        assert!(ansi.contains("\x1b[9m"));
    }
}
