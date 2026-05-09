//! `Theme` type and its serializable schema.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/theme/theme.ts`.
//!
//! The `Theme` struct holds pre-rendered ANSI escape sequences for every
//! semantic colour slot, so render-hot paths only do a `HashMap` lookup +
//! string concat. Helpers for bold / italic / etc. emit standard SGR
//! sequences (matching `chalk`'s output for `chalk.bold`, etc.).
//!
//! What's intentionally *not* yet ported in this unit:
//!
//! - The cli-highlight syntax theme bridge (`buildCliHighlightTheme`,
//!   `highlightCode`). The Rust workspace doesn't depend on a syntax
//!   highlighter yet; when that lands, add a separate unit.
//! - Markdown / select-list / editor / settings-list theme adapters
//!   (`getMarkdownTheme()` etc.) — `hand-tui` exposes its own
//!   `MarkdownTheme` / `SelectListTheme` shapes, and the bridge will be a
//!   small follow-up unit consumed by the components that need it.
//! - The global proxy `theme` (TS uses `globalThis` to share across module
//!   loaders). In Rust the controller will pass `&Theme` explicitly or use
//!   an `Arc<Theme>` owned by the interactive driver.
//!
// TODO(parity): port markdown / select-list / editor theme adapters once a
// component port consumes them.
// TODO(parity): port the syntax-highlight theme bridge when a Rust
// highlighter is wired in.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::color::{
    ColorError, ColorMode, ColorValue, ResolvedColor, bg_ansi, detect_color_mode, fg_ansi,
};

/// Raw colour-value entry as read from a theme JSON file. Either a string
/// (hex, var-ref, or `""` for default) or an integer (palette index).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RawColorValue {
    String(String),
    Index(u8),
}

/// Serialisable theme schema, mirroring the TS `ThemeJsonSchema`.
///
/// The colour set is intentionally exhaustive (and validated by the loader)
/// because every slot drives a fixed UI element and a missing slot would
/// surface as a runtime "unknown theme color" error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeJson {
    /// Optional `$schema` URI — captured for round-tripping but unused.
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    pub name: String,
    /// Named colour aliases; can be referenced by entries in `colors`.
    #[serde(default)]
    pub vars: HashMap<String, RawColorValue>,
    pub colors: ThemeColors,
    /// Optional HTML-export overrides — preserved here for round-tripping
    /// even though the export pipeline isn't yet ported.
    #[serde(default)]
    pub export: Option<ThemeExport>,
}

/// Every colour slot referenced by the interactive UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    // Core UI
    pub accent: RawColorValue,
    pub border: RawColorValue,
    pub border_accent: RawColorValue,
    pub border_muted: RawColorValue,
    pub success: RawColorValue,
    pub error: RawColorValue,
    pub warning: RawColorValue,
    pub muted: RawColorValue,
    pub dim: RawColorValue,
    pub text: RawColorValue,
    pub thinking_text: RawColorValue,
    // Backgrounds & content text
    pub selected_bg: RawColorValue,
    pub user_message_bg: RawColorValue,
    pub user_message_text: RawColorValue,
    pub custom_message_bg: RawColorValue,
    pub custom_message_text: RawColorValue,
    pub custom_message_label: RawColorValue,
    pub tool_pending_bg: RawColorValue,
    pub tool_success_bg: RawColorValue,
    pub tool_error_bg: RawColorValue,
    pub tool_title: RawColorValue,
    pub tool_output: RawColorValue,
    // Markdown
    pub md_heading: RawColorValue,
    pub md_link: RawColorValue,
    pub md_link_url: RawColorValue,
    pub md_code: RawColorValue,
    pub md_code_block: RawColorValue,
    pub md_code_block_border: RawColorValue,
    pub md_quote: RawColorValue,
    pub md_quote_border: RawColorValue,
    pub md_hr: RawColorValue,
    pub md_list_bullet: RawColorValue,
    // Tool diffs
    pub tool_diff_added: RawColorValue,
    pub tool_diff_removed: RawColorValue,
    pub tool_diff_context: RawColorValue,
    // Syntax highlighting
    pub syntax_comment: RawColorValue,
    pub syntax_keyword: RawColorValue,
    pub syntax_function: RawColorValue,
    pub syntax_variable: RawColorValue,
    pub syntax_string: RawColorValue,
    pub syntax_number: RawColorValue,
    pub syntax_type: RawColorValue,
    pub syntax_operator: RawColorValue,
    pub syntax_punctuation: RawColorValue,
    // Thinking-level borders
    pub thinking_off: RawColorValue,
    pub thinking_minimal: RawColorValue,
    pub thinking_low: RawColorValue,
    pub thinking_medium: RawColorValue,
    pub thinking_high: RawColorValue,
    pub thinking_xhigh: RawColorValue,
    // Bash mode
    pub bash_mode: RawColorValue,
}

/// Optional HTML-export overrides (page background, card background, info
/// callout background). Captured for round-tripping; not consumed yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeExport {
    #[serde(default)]
    pub page_bg: Option<RawColorValue>,
    #[serde(default)]
    pub card_bg: Option<RawColorValue>,
    #[serde(default)]
    pub info_bg: Option<RawColorValue>,
}

/// Foreground (and pure-text) colour slots a renderer can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeColor {
    Accent,
    Border,
    BorderAccent,
    BorderMuted,
    Success,
    Error,
    Warning,
    Muted,
    Dim,
    Text,
    ThinkingText,
    UserMessageText,
    CustomMessageText,
    CustomMessageLabel,
    ToolTitle,
    ToolOutput,
    MdHeading,
    MdLink,
    MdLinkUrl,
    MdCode,
    MdCodeBlock,
    MdCodeBlockBorder,
    MdQuote,
    MdQuoteBorder,
    MdHr,
    MdListBullet,
    ToolDiffAdded,
    ToolDiffRemoved,
    ToolDiffContext,
    SyntaxComment,
    SyntaxKeyword,
    SyntaxFunction,
    SyntaxVariable,
    SyntaxString,
    SyntaxNumber,
    SyntaxType,
    SyntaxOperator,
    SyntaxPunctuation,
    ThinkingOff,
    ThinkingMinimal,
    ThinkingLow,
    ThinkingMedium,
    ThinkingHigh,
    ThinkingXhigh,
    BashMode,
}

/// Background colour slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeBg {
    SelectedBg,
    UserMessageBg,
    CustomMessageBg,
    ToolPendingBg,
    ToolSuccessBg,
    ToolErrorBg,
}

/// Six discrete reasoning-effort levels that drive border colouring of the
/// thinking pane. Mirrors the TS `level` parameter of
/// `getThinkingBorderColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

/// Errors surfaced when constructing a theme.
#[derive(Debug, Error)]
pub enum ThemeError {
    #[error(transparent)]
    Color(#[from] ColorError),
    #[error("unknown theme color: {0:?}")]
    UnknownColor(ThemeColor),
    #[error("unknown theme background color: {0:?}")]
    UnknownBg(ThemeBg),
}

/// A fully-resolved theme: the JSON colour entries have been expanded
/// against `vars`, quantised for the active `ColorMode`, and pre-rendered
/// to ANSI escape sequences.
#[derive(Debug, Clone)]
pub struct Theme {
    name: String,
    source_path: Option<String>,
    fg: HashMap<ThemeColor, String>,
    bg: HashMap<ThemeBg, String>,
    mode: ColorMode,
}

impl Theme {
    /// Construct a theme directly from a parsed `ThemeJson` and a
    /// (possibly-detected) colour mode.
    pub fn from_json(json: &ThemeJson, mode: Option<ColorMode>) -> Result<Self, ThemeError> {
        Self::from_json_with_path(json, mode, None)
    }

    /// Construct a theme and remember the source path it loaded from
    /// (used by the watcher and diagnostics in pi-mono).
    pub fn from_json_with_path(
        json: &ThemeJson,
        mode: Option<ColorMode>,
        source_path: Option<String>,
    ) -> Result<Self, ThemeError> {
        let mode = mode.unwrap_or_else(detect_color_mode);
        let mut fg: HashMap<ThemeColor, String> = HashMap::new();
        let mut bg: HashMap<ThemeBg, String> = HashMap::new();

        let vars = parse_vars(&json.vars);

        for (color, raw) in fg_entries(&json.colors) {
            let resolved = resolve_color(raw, &vars)?;
            fg.insert(color, fg_ansi(&resolved, mode));
        }
        for (bg_color, raw) in bg_entries(&json.colors) {
            let resolved = resolve_color(raw, &vars)?;
            bg.insert(bg_color, bg_ansi(&resolved, mode));
        }

        Ok(Theme {
            name: json.name.clone(),
            source_path,
            fg,
            bg,
            mode,
        })
    }

    /// Theme name as declared in JSON.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Path the theme was loaded from, if any.
    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }

    /// Colour mode this theme was resolved against.
    pub fn color_mode(&self) -> ColorMode {
        self.mode
    }

    /// Wrap `text` in the foreground escape for `color`. The wrapper resets
    /// only the foreground (`\x1b[39m`) so callers can compose with bold /
    /// italic / background spans without bleeding state.
    pub fn fg(&self, color: ThemeColor, text: &str) -> Result<String, ThemeError> {
        let ansi = self.fg.get(&color).ok_or(ThemeError::UnknownColor(color))?;
        Ok(format!("{}{}\x1b[39m", ansi, text))
    }

    /// Wrap `text` in the background escape for `bg`.
    pub fn bg(&self, bg: ThemeBg, text: &str) -> Result<String, ThemeError> {
        let ansi = self.bg.get(&bg).ok_or(ThemeError::UnknownBg(bg))?;
        Ok(format!("{}{}\x1b[49m", ansi, text))
    }

    /// Borrow the foreground ANSI escape (no reset). Useful when callers
    /// need to compose multiple spans.
    pub fn fg_ansi(&self, color: ThemeColor) -> Result<&str, ThemeError> {
        self.fg
            .get(&color)
            .map(String::as_str)
            .ok_or(ThemeError::UnknownColor(color))
    }

    /// Borrow the background ANSI escape (no reset).
    pub fn bg_ansi(&self, bg: ThemeBg) -> Result<&str, ThemeError> {
        self.bg
            .get(&bg)
            .map(String::as_str)
            .ok_or(ThemeError::UnknownBg(bg))
    }

    /// Bold SGR wrapper (`\x1b[1m … \x1b[22m`).
    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{}\x1b[22m", text)
    }

    /// Italic SGR wrapper (`\x1b[3m … \x1b[23m`).
    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{}\x1b[23m", text)
    }

    /// Underline SGR wrapper (`\x1b[4m … \x1b[24m`).
    pub fn underline(&self, text: &str) -> String {
        format!("\x1b[4m{}\x1b[24m", text)
    }

    /// Inverse-video SGR wrapper (`\x1b[7m … \x1b[27m`).
    pub fn inverse(&self, text: &str) -> String {
        format!("\x1b[7m{}\x1b[27m", text)
    }

    /// Strikethrough SGR wrapper (`\x1b[9m … \x1b[29m`).
    pub fn strikethrough(&self, text: &str) -> String {
        format!("\x1b[9m{}\x1b[29m", text)
    }

    /// Map a thinking level to the matching colour slot.
    pub fn thinking_color(&self, level: ThinkingLevel) -> ThemeColor {
        match level {
            ThinkingLevel::Off => ThemeColor::ThinkingOff,
            ThinkingLevel::Minimal => ThemeColor::ThinkingMinimal,
            ThinkingLevel::Low => ThemeColor::ThinkingLow,
            ThinkingLevel::Medium => ThemeColor::ThinkingMedium,
            ThinkingLevel::High => ThemeColor::ThinkingHigh,
            ThinkingLevel::XHigh => ThemeColor::ThinkingXhigh,
        }
    }
}

// ============================================================================
// Internal helpers — colour resolution
// ============================================================================

fn parse_vars(raw_vars: &HashMap<String, RawColorValue>) -> HashMap<String, ColorValue> {
    raw_vars
        .iter()
        .map(|(k, v)| (k.clone(), parse_color_value(v)))
        .collect()
}

fn parse_color_value(raw: &RawColorValue) -> ColorValue {
    match raw {
        RawColorValue::Index(i) => ColorValue::Index(*i),
        RawColorValue::String(s) => {
            if s.is_empty() {
                ColorValue::Default
            } else if s.starts_with('#') {
                ColorValue::Hex(s.clone())
            } else {
                ColorValue::VarRef(s.clone())
            }
        }
    }
}

fn resolve_color(
    raw: &RawColorValue,
    vars: &HashMap<String, ColorValue>,
) -> Result<ResolvedColor, ColorError> {
    let value = parse_color_value(raw);
    resolve_value(&value, vars, &mut Vec::new())
}

fn resolve_value(
    value: &ColorValue,
    vars: &HashMap<String, ColorValue>,
    visited: &mut Vec<String>,
) -> Result<ResolvedColor, ColorError> {
    match value {
        ColorValue::Hex(s) => Ok(ResolvedColor::Hex(s.clone())),
        ColorValue::Index(i) => Ok(ResolvedColor::Index(*i)),
        ColorValue::Default => Ok(ResolvedColor::Default),
        ColorValue::VarRef(name) => {
            if visited.iter().any(|v| v == name) {
                return Err(ColorError::CircularVarRef(name.clone()));
            }
            let next = vars
                .get(name)
                .ok_or_else(|| ColorError::UnknownVarRef(name.clone()))?;
            visited.push(name.clone());
            let r = resolve_value(next, vars, visited);
            visited.pop();
            r
        }
    }
}

/// Iterator over `(ThemeColor, &RawColorValue)` for every foreground slot.
fn fg_entries(c: &ThemeColors) -> Vec<(ThemeColor, &RawColorValue)> {
    use ThemeColor::*;
    vec![
        (Accent, &c.accent),
        (Border, &c.border),
        (BorderAccent, &c.border_accent),
        (BorderMuted, &c.border_muted),
        (Success, &c.success),
        (Error, &c.error),
        (Warning, &c.warning),
        (Muted, &c.muted),
        (Dim, &c.dim),
        (Text, &c.text),
        (ThinkingText, &c.thinking_text),
        (UserMessageText, &c.user_message_text),
        (CustomMessageText, &c.custom_message_text),
        (CustomMessageLabel, &c.custom_message_label),
        (ToolTitle, &c.tool_title),
        (ToolOutput, &c.tool_output),
        (MdHeading, &c.md_heading),
        (MdLink, &c.md_link),
        (MdLinkUrl, &c.md_link_url),
        (MdCode, &c.md_code),
        (MdCodeBlock, &c.md_code_block),
        (MdCodeBlockBorder, &c.md_code_block_border),
        (MdQuote, &c.md_quote),
        (MdQuoteBorder, &c.md_quote_border),
        (MdHr, &c.md_hr),
        (MdListBullet, &c.md_list_bullet),
        (ToolDiffAdded, &c.tool_diff_added),
        (ToolDiffRemoved, &c.tool_diff_removed),
        (ToolDiffContext, &c.tool_diff_context),
        (SyntaxComment, &c.syntax_comment),
        (SyntaxKeyword, &c.syntax_keyword),
        (SyntaxFunction, &c.syntax_function),
        (SyntaxVariable, &c.syntax_variable),
        (SyntaxString, &c.syntax_string),
        (SyntaxNumber, &c.syntax_number),
        (SyntaxType, &c.syntax_type),
        (SyntaxOperator, &c.syntax_operator),
        (SyntaxPunctuation, &c.syntax_punctuation),
        (ThinkingOff, &c.thinking_off),
        (ThinkingMinimal, &c.thinking_minimal),
        (ThinkingLow, &c.thinking_low),
        (ThinkingMedium, &c.thinking_medium),
        (ThinkingHigh, &c.thinking_high),
        (ThinkingXhigh, &c.thinking_xhigh),
        (BashMode, &c.bash_mode),
    ]
}

/// Iterator over `(ThemeBg, &RawColorValue)` for every background slot.
fn bg_entries(c: &ThemeColors) -> Vec<(ThemeBg, &RawColorValue)> {
    use ThemeBg::*;
    vec![
        (SelectedBg, &c.selected_bg),
        (UserMessageBg, &c.user_message_bg),
        (CustomMessageBg, &c.custom_message_bg),
        (ToolPendingBg, &c.tool_pending_bg),
        (ToolSuccessBg, &c.tool_success_bg),
        (ToolErrorBg, &c.tool_error_bg),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::built_in::dark_theme_json_str;
    use super::*;

    fn parse_dark() -> ThemeJson {
        serde_json::from_str(dark_theme_json_str()).expect("dark.json parses")
    }

    #[test]
    fn dark_theme_constructs_with_truecolor() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.name(), "dark");
        // accent var resolves to "#8abeb7"
        let accent = theme.fg_ansi(ThemeColor::Accent).unwrap();
        assert_eq!(accent, "\x1b[38;2;138;190;183m");
    }

    #[test]
    fn fg_wraps_with_reset() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        let s = theme.fg(ThemeColor::Accent, "x").unwrap();
        assert!(s.starts_with("\x1b[38;2;138;190;183m"));
        assert!(s.ends_with("\x1b[39m"));
    }

    #[test]
    fn bg_wraps_with_reset() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        let s = theme.bg(ThemeBg::SelectedBg, "x").unwrap();
        assert!(s.starts_with("\x1b[48;"));
        assert!(s.ends_with("\x1b[49m"));
    }

    #[test]
    fn empty_text_color_yields_default_fg() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        // dark.json uses "" for text -> default terminal fg
        assert_eq!(theme.fg_ansi(ThemeColor::Text).unwrap(), "\x1b[39m");
    }

    #[test]
    fn rejects_circular_var_ref() {
        let json: ThemeJson = serde_json::from_str(
            r##"{
                "name": "broken",
                "vars": { "a": "b", "b": "a" },
                "colors": {
                    "accent": "a",
                    "border": "#000000",
                    "borderAccent": "#000000",
                    "borderMuted": "#000000",
                    "success": "#000000",
                    "error": "#000000",
                    "warning": "#000000",
                    "muted": "#000000",
                    "dim": "#000000",
                    "text": "",
                    "thinkingText": "#000000",
                    "selectedBg": "#000000",
                    "userMessageBg": "#000000",
                    "userMessageText": "#000000",
                    "customMessageBg": "#000000",
                    "customMessageText": "#000000",
                    "customMessageLabel": "#000000",
                    "toolPendingBg": "#000000",
                    "toolSuccessBg": "#000000",
                    "toolErrorBg": "#000000",
                    "toolTitle": "#000000",
                    "toolOutput": "#000000",
                    "mdHeading": "#000000",
                    "mdLink": "#000000",
                    "mdLinkUrl": "#000000",
                    "mdCode": "#000000",
                    "mdCodeBlock": "#000000",
                    "mdCodeBlockBorder": "#000000",
                    "mdQuote": "#000000",
                    "mdQuoteBorder": "#000000",
                    "mdHr": "#000000",
                    "mdListBullet": "#000000",
                    "toolDiffAdded": "#000000",
                    "toolDiffRemoved": "#000000",
                    "toolDiffContext": "#000000",
                    "syntaxComment": "#000000",
                    "syntaxKeyword": "#000000",
                    "syntaxFunction": "#000000",
                    "syntaxVariable": "#000000",
                    "syntaxString": "#000000",
                    "syntaxNumber": "#000000",
                    "syntaxType": "#000000",
                    "syntaxOperator": "#000000",
                    "syntaxPunctuation": "#000000",
                    "thinkingOff": "#000000",
                    "thinkingMinimal": "#000000",
                    "thinkingLow": "#000000",
                    "thinkingMedium": "#000000",
                    "thinkingHigh": "#000000",
                    "thinkingXhigh": "#000000",
                    "bashMode": "#000000"
                }
            }"##,
        )
        .unwrap();

        let err = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap_err();
        assert!(matches!(
            err,
            ThemeError::Color(ColorError::CircularVarRef(_))
        ));
    }

    #[test]
    fn thinking_color_maps_levels() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(
            theme.thinking_color(ThinkingLevel::Off),
            ThemeColor::ThinkingOff
        );
        assert_eq!(
            theme.thinking_color(ThinkingLevel::XHigh),
            ThemeColor::ThinkingXhigh
        );
    }

    #[test]
    fn bold_italic_emit_sgr() {
        let json = parse_dark();
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.bold("hi"), "\x1b[1mhi\x1b[22m");
        assert_eq!(theme.italic("hi"), "\x1b[3mhi\x1b[23m");
        assert_eq!(theme.underline("hi"), "\x1b[4mhi\x1b[24m");
    }
}
