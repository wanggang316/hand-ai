//! A flat, render-ready colour palette derived from the active [`Theme`].
//!
//! The rt render modules paint through `ratatui::style::Color`. Historically
//! each module carried its own hard-coded `Color::` constants, so a user's
//! custom `~/.hand/themes/*.json` loaded but never coloured the UI: the theme
//! seam ([`Theme::ratatui_style`]) existed, but nothing consumed it in the
//! render path. [`ThemePalette`] is that consumer — a small, `Copy` bundle of
//! the *resolved* `ratatui` colours the user-visible surfaces need (message
//! bubble, thinking text, error / status lines, chrome accent, tool boxes,
//! custom-message summary, bash banner, selector highlights).
//!
//! # The default-palette invariant
//!
//! The critical rule is **the default look must not change**: with no custom
//! theme active, every colour is byte-for-byte what the modules hard-coded. So
//! [`ThemePalette::default`] returns exactly those historical constants, and a
//! **built-in** theme (`dark` / `light`, which carry no `source_path`) also
//! renders the default palette — the built-ins were never separately themed on
//! the rt surface, and pinning them to the historical constants keeps the
//! default appearance identical.
//!
//! A **custom** theme (loaded from a file, so `source_path().is_some()`)
//! derives its palette from the theme's resolved slots via the
//! [`ratatui_style`](super::ratatui_style) bridge, so the custom palette
//! actually colours the UI (VAL-COMPAT-004). A slot that resolves to the
//! terminal default ([`Color::Reset`]) falls back to the historical constant,
//! so a custom theme that leaves a slot empty (`""`) keeps a sensible,
//! readable colour rather than washing to the terminal default mid-tint.

use ratatui::style::Color;

use super::core::{Theme, ThemeBg, ThemeColor};

/// A render-ready bundle of resolved `ratatui` colours for the user-visible
/// rt surfaces. `Copy` so a render function can take it by value cheaply.
///
/// Built from the active [`Theme`] via [`ThemePalette::from_theme`]; a driver
/// with no theme seeded falls back to [`ThemePalette::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    // --- messages ---------------------------------------------------------
    /// User-bubble background tint.
    pub user_message_bg: Color,
    /// User-bubble foreground.
    pub user_message_text: Color,
    /// Dimmed thinking-block text.
    pub thinking_text: Color,
    /// Error footnote / banner foreground.
    pub error: Color,
    /// Status / warning line foreground.
    pub warning: Color,

    // --- chrome / selectors ----------------------------------------------
    /// Header product-name accent, and the selector highlight / cursor accent.
    pub accent: Color,
    /// Dim chrome (version / model, hint keys and actions) and muted selector
    /// rows / labels.
    pub dim: Color,
    /// Success accent (selector "current" / "enabled" marks).
    pub success: Color,

    // --- tools ------------------------------------------------------------
    /// In-flight tool-box background tint.
    pub tool_pending_bg: Color,
    /// Successful tool-box background tint.
    pub tool_success_bg: Color,
    /// Failed tool-box background tint.
    pub tool_error_bg: Color,
    /// Tool-box title (tool name) foreground.
    pub tool_title: Color,
    /// Tool-box body (args / result) foreground.
    pub tool_output: Color,
    /// Diff added-line foreground.
    pub diff_added: Color,
    /// Diff removed-line foreground.
    pub diff_removed: Color,
    /// Diff context-line foreground.
    pub diff_context: Color,

    // --- summary (custom message box) -------------------------------------
    /// Custom-message box background tint.
    pub custom_message_bg: Color,
    /// Custom-message box label foreground.
    pub custom_message_label: Color,
    /// Custom-message box body foreground.
    pub custom_message_text: Color,

    // --- bash / selectors -------------------------------------------------
    /// Bash-mode banner accent.
    pub bash_mode: Color,
    /// Selector highlight background (selected row).
    pub selected_bg: Color,
    /// Selector / panel border.
    pub border: Color,
}

// The historical hard-coded constants, kept here as the single source of truth
// for the default palette so the "default look unchanged" invariant is pinned
// in one place. Each mirrors the constant the corresponding render module used
// before it consumed the palette.
const DEFAULT_USER_MESSAGE_BG: Color = Color::Rgb(52, 53, 65);
const DEFAULT_USER_MESSAGE_TEXT: Color = Color::Rgb(230, 230, 230);
const DEFAULT_THINKING_TEXT: Color = Color::DarkGray;
const DEFAULT_ERROR: Color = Color::Red;
const DEFAULT_WARNING: Color = Color::Yellow;
const DEFAULT_ACCENT: Color = Color::Cyan;
const DEFAULT_DIM: Color = Color::DarkGray;
const DEFAULT_SUCCESS: Color = Color::Green;
const DEFAULT_TOOL_PENDING_BG: Color = Color::Rgb(40, 40, 50);
const DEFAULT_TOOL_SUCCESS_BG: Color = Color::Rgb(40, 50, 40);
const DEFAULT_TOOL_ERROR_BG: Color = Color::Rgb(60, 40, 40);
const DEFAULT_TOOL_TITLE: Color = Color::Rgb(120, 220, 220);
const DEFAULT_TOOL_OUTPUT: Color = Color::Rgb(220, 220, 220);
const DEFAULT_DIFF_ADDED: Color = Color::Green;
const DEFAULT_DIFF_REMOVED: Color = Color::Red;
const DEFAULT_DIFF_CONTEXT: Color = Color::DarkGray;
const DEFAULT_CUSTOM_MESSAGE_BG: Color = Color::Rgb(95, 0, 95);
const DEFAULT_CUSTOM_MESSAGE_LABEL: Color = Color::Rgb(255, 120, 255);
const DEFAULT_CUSTOM_MESSAGE_TEXT: Color = Color::Rgb(238, 238, 238);
const DEFAULT_BASH_MODE: Color = Color::Cyan;
const DEFAULT_SELECTED_BG: Color = Color::Rgb(58, 58, 74);
const DEFAULT_BORDER: Color = Color::DarkGray;

impl Default for ThemePalette {
    /// The historical hard-coded palette: the default look, unchanged.
    fn default() -> Self {
        Self {
            user_message_bg: DEFAULT_USER_MESSAGE_BG,
            user_message_text: DEFAULT_USER_MESSAGE_TEXT,
            thinking_text: DEFAULT_THINKING_TEXT,
            error: DEFAULT_ERROR,
            warning: DEFAULT_WARNING,
            accent: DEFAULT_ACCENT,
            dim: DEFAULT_DIM,
            success: DEFAULT_SUCCESS,
            tool_pending_bg: DEFAULT_TOOL_PENDING_BG,
            tool_success_bg: DEFAULT_TOOL_SUCCESS_BG,
            tool_error_bg: DEFAULT_TOOL_ERROR_BG,
            tool_title: DEFAULT_TOOL_TITLE,
            tool_output: DEFAULT_TOOL_OUTPUT,
            diff_added: DEFAULT_DIFF_ADDED,
            diff_removed: DEFAULT_DIFF_REMOVED,
            diff_context: DEFAULT_DIFF_CONTEXT,
            custom_message_bg: DEFAULT_CUSTOM_MESSAGE_BG,
            custom_message_label: DEFAULT_CUSTOM_MESSAGE_LABEL,
            custom_message_text: DEFAULT_CUSTOM_MESSAGE_TEXT,
            bash_mode: DEFAULT_BASH_MODE,
            selected_bg: DEFAULT_SELECTED_BG,
            border: DEFAULT_BORDER,
        }
    }
}

impl ThemePalette {
    /// Derive the render palette from the active theme.
    ///
    /// A **built-in** theme (`dark` / `light`, `source_path().is_none()`)
    /// yields the default palette unchanged, preserving the historical look.
    /// A **custom** theme resolves each slot through the
    /// [`ratatui_style`](super::ratatui_style) bridge so the user's palette
    /// actually colours the UI; a slot that resolves to the terminal default
    /// ([`Color::Reset`]) keeps its historical constant so an empty slot stays
    /// readable rather than washing out mid-tint.
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        // Built-ins are never separately themed on the rt surface — pin them to
        // the historical constants so the default look is byte-identical.
        if theme.source_path().is_none() {
            return Self::default();
        }

        let default = Self::default();
        let fg = |slot: ThemeColor, fallback: Color| resolve_fg(theme, slot, fallback);
        let bg = |slot: ThemeBg, fallback: Color| resolve_bg(theme, slot, fallback);

        Self {
            user_message_bg: bg(ThemeBg::UserMessageBg, default.user_message_bg),
            user_message_text: fg(ThemeColor::UserMessageText, default.user_message_text),
            thinking_text: fg(ThemeColor::ThinkingText, default.thinking_text),
            error: fg(ThemeColor::Error, default.error),
            warning: fg(ThemeColor::Warning, default.warning),
            accent: fg(ThemeColor::Accent, default.accent),
            dim: fg(ThemeColor::Dim, default.dim),
            success: fg(ThemeColor::Success, default.success),
            tool_pending_bg: bg(ThemeBg::ToolPendingBg, default.tool_pending_bg),
            tool_success_bg: bg(ThemeBg::ToolSuccessBg, default.tool_success_bg),
            tool_error_bg: bg(ThemeBg::ToolErrorBg, default.tool_error_bg),
            tool_title: fg(ThemeColor::ToolTitle, default.tool_title),
            tool_output: fg(ThemeColor::ToolOutput, default.tool_output),
            diff_added: fg(ThemeColor::ToolDiffAdded, default.diff_added),
            diff_removed: fg(ThemeColor::ToolDiffRemoved, default.diff_removed),
            diff_context: fg(ThemeColor::ToolDiffContext, default.diff_context),
            custom_message_bg: bg(ThemeBg::CustomMessageBg, default.custom_message_bg),
            custom_message_label: fg(ThemeColor::CustomMessageLabel, default.custom_message_label),
            custom_message_text: fg(ThemeColor::CustomMessageText, default.custom_message_text),
            bash_mode: fg(ThemeColor::BashMode, default.bash_mode),
            selected_bg: bg(ThemeBg::SelectedBg, default.selected_bg),
            border: fg(ThemeColor::Border, default.border),
        }
    }

    /// Derive the palette from an optional theme, falling back to the default
    /// palette when no theme is seeded (the test-constructor / pre-seed case).
    #[must_use]
    pub fn from_optional(theme: Option<&Theme>) -> Self {
        theme.map_or_else(Self::default, Self::from_theme)
    }
}

/// Resolve a foreground slot to a `ratatui` colour, keeping `fallback` when the
/// slot resolves to the terminal default (so an empty slot stays readable) or
/// when the slot is somehow unknown (which the loader prevents).
fn resolve_fg(theme: &Theme, slot: ThemeColor, fallback: Color) -> Color {
    match theme.ratatui_fg(slot) {
        Ok(Color::Reset) | Err(_) => fallback,
        Ok(color) => color,
    }
}

/// Resolve a background slot to a `ratatui` colour, with the same
/// terminal-default fallback as [`resolve_fg`].
fn resolve_bg(theme: &Theme, slot: ThemeBg, fallback: Color) -> Color {
    match theme.ratatui_bg(slot) {
        Ok(Color::Reset) | Err(_) => fallback,
        Ok(color) => color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::built_in::dark_theme_json_str;
    use crate::modes::interactive::theme::color::ColorMode;
    use crate::modes::interactive::theme::core::ThemeJson;

    fn dark() -> Theme {
        let json: ThemeJson = serde_json::from_str(dark_theme_json_str()).unwrap();
        Theme::from_json(&json, Some(ColorMode::Truecolor)).unwrap()
    }

    fn custom_neon() -> Theme {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/tui/themes/custom-neon.json");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let json: ThemeJson = serde_json::from_str(&content).unwrap();
        // A custom theme carries its source path — the signal `from_theme` uses
        // to distinguish "custom, colour the UI" from "built-in, keep default".
        Theme::from_json_with_path(&json, Some(ColorMode::Truecolor), Some(path.display().to_string()))
            .unwrap()
    }

    #[test]
    fn default_palette_matches_historical_constants() {
        // The default look is pinned: every field is the exact constant the
        // render modules used before consuming the palette. A regression here
        // would silently change the default appearance.
        let p = ThemePalette::default();
        assert_eq!(p.user_message_bg, Color::Rgb(52, 53, 65));
        assert_eq!(p.user_message_text, Color::Rgb(230, 230, 230));
        assert_eq!(p.thinking_text, Color::DarkGray);
        assert_eq!(p.error, Color::Red);
        assert_eq!(p.warning, Color::Yellow);
        assert_eq!(p.accent, Color::Cyan);
        assert_eq!(p.dim, Color::DarkGray);
        assert_eq!(p.success, Color::Green);
        assert_eq!(p.tool_pending_bg, Color::Rgb(40, 40, 50));
        assert_eq!(p.tool_success_bg, Color::Rgb(40, 50, 40));
        assert_eq!(p.tool_error_bg, Color::Rgb(60, 40, 40));
        assert_eq!(p.tool_title, Color::Rgb(120, 220, 220));
        assert_eq!(p.tool_output, Color::Rgb(220, 220, 220));
        assert_eq!(p.diff_added, Color::Green);
        assert_eq!(p.diff_removed, Color::Red);
        assert_eq!(p.diff_context, Color::DarkGray);
        assert_eq!(p.custom_message_bg, Color::Rgb(95, 0, 95));
        assert_eq!(p.custom_message_label, Color::Rgb(255, 120, 255));
        assert_eq!(p.custom_message_text, Color::Rgb(238, 238, 238));
        assert_eq!(p.bash_mode, Color::Cyan);
        assert_eq!(p.selected_bg, Color::Rgb(58, 58, 74));
        assert_eq!(p.border, Color::DarkGray);
    }

    #[test]
    fn builtin_dark_theme_yields_the_default_palette() {
        // The built-in `dark` theme carries no source path, so the rt surface
        // keeps the default look unchanged (the invariant: no custom theme →
        // default palette byte-for-byte).
        assert_eq!(ThemePalette::from_theme(&dark()), ThemePalette::default());
    }

    #[test]
    fn none_theme_falls_back_to_default() {
        assert_eq!(ThemePalette::from_optional(None), ThemePalette::default());
    }

    #[test]
    fn custom_theme_recolours_the_primary_slots() {
        // A custom theme drives the palette: the neon fixture's slots resolve to
        // their RGB values, visibly different from the default palette
        // (VAL-COMPAT-004).
        let p = ThemePalette::from_theme(&custom_neon());
        let default = ThemePalette::default();
        assert_ne!(p, default, "custom theme must change the palette");
        // accent -> neonPink (#ff00ff)
        assert_eq!(p.accent, Color::Rgb(0xff, 0x00, 0xff));
        // error -> #ff003c
        assert_eq!(p.error, Color::Rgb(0xff, 0x00, 0x3c));
        // warning -> neonYellow (#ffea00)
        assert_eq!(p.warning, Color::Rgb(0xff, 0xea, 0x00));
        // userMessageBg -> deepBg (#12001f)
        assert_eq!(p.user_message_bg, Color::Rgb(0x12, 0x00, 0x1f));
        // toolTitle -> neonCyan (#00ffff)
        assert_eq!(p.tool_title, Color::Rgb(0x00, 0xff, 0xff));
        // selectedBg -> panelBg (#1a0033)
        assert_eq!(p.selected_bg, Color::Rgb(0x1a, 0x00, 0x33));
        // bashMode -> neonGreen (#39ff14)
        assert_eq!(p.bash_mode, Color::Rgb(0x39, 0xff, 0x14));
    }

    #[test]
    fn custom_theme_empty_slot_keeps_readable_fallback() {
        // A custom theme whose `text`-family slot resolves to the terminal
        // default keeps the historical constant instead of washing to Reset.
        // The neon fixture sets userMessageText explicitly, so build a variant
        // with an empty slot to exercise the fallback.
        let mut json: ThemeJson = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/tui/themes/custom-neon.json"),
            )
            .unwrap(),
        )
        .unwrap();
        json.colors.tool_output = super::super::core::RawColorValue::String(String::new());
        let theme = Theme::from_json_with_path(
            &json,
            Some(ColorMode::Truecolor),
            Some("custom.json".to_string()),
        )
        .unwrap();
        let p = ThemePalette::from_theme(&theme);
        // tool_output resolved to Reset → falls back to the historical constant.
        assert_eq!(p.tool_output, Color::Rgb(220, 220, 220));
        // Other slots still recolour.
        assert_eq!(p.accent, Color::Rgb(0xff, 0x00, 0xff));
    }
}
