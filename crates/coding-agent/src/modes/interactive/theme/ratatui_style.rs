//! Bridge the interactive [`Theme`] to `ratatui::style` types.
//!
//! The legacy renderer wraps text in pre-baked ANSI escape sequences (see
//! [`Theme::fg`] / [`Theme::bg`]). The rt driver paints through ratatui's
//! [`Style`], so this module maps each semantic colour slot to a
//! [`ratatui::style::Color`] and offers a `Style` builder for the common
//! `(fg, bg)` pair.
//!
//! **User-invisible compatibility.** The mapping consumes the *same*
//! resolved colour values the ANSI path uses — the user's
//! `~/.hand/themes/*.json` (hex / palette-index / `""`-default) drives both
//! surfaces identically. A theme slot resolved to the terminal default maps
//! to [`Color::Reset`] so ratatui yields to the terminal's own fg/bg rather
//! than forcing black.
//!
//! The [`ColorMode`] the theme was built against is honoured: a truecolour
//! theme keeps its 24-bit RGB, while a `256color` theme's hex slots are
//! quantised to [`Color::Indexed`] exactly as the ANSI path quantises them,
//! so a limited terminal renders the same narrowed palette on both surfaces.

use ratatui::style::{Color as RtColor, Style as RtStyle};

use super::color::{ColorMode, ResolvedColor, hex_to_256, hex_to_rgb};
use super::core::{Theme, ThemeBg, ThemeColor, ThemeError};

/// Map a resolved theme colour to a `ratatui` colour under `mode`.
///
/// - [`ResolvedColor::Default`] → [`Color::Reset`] (keep the terminal's own
///   colour; never force black).
/// - [`ResolvedColor::Index`] → [`Color::Indexed`] (palette pass-through).
/// - [`ResolvedColor::Hex`] under [`ColorMode::Truecolor`] → [`Color::Rgb`];
///   under [`ColorMode::Color256`] → the quantised [`Color::Indexed`].
///
/// A malformed hex string (which the loader would have rejected up-front)
/// degrades to [`Color::Reset`] rather than panicking, so a partially-broken
/// theme that slips through still renders.
#[must_use]
pub fn resolved_to_ratatui(value: &ResolvedColor, mode: ColorMode) -> RtColor {
    match value {
        ResolvedColor::Default => RtColor::Reset,
        ResolvedColor::Index(i) => RtColor::Indexed(*i),
        ResolvedColor::Hex(hex) => match mode {
            ColorMode::Truecolor => match hex_to_rgb(hex) {
                Ok((r, g, b)) => RtColor::Rgb(r, g, b),
                Err(_) => RtColor::Reset,
            },
            ColorMode::Color256 => match hex_to_256(hex) {
                Ok(idx) => RtColor::Indexed(idx),
                Err(_) => RtColor::Reset,
            },
        },
    }
}

impl Theme {
    /// The `ratatui` foreground colour for `color`, honouring this theme's
    /// [`ColorMode`]. Errors only when the slot is unknown (which cannot
    /// happen for a theme built by the loader — every slot is populated).
    pub fn ratatui_fg(&self, color: ThemeColor) -> Result<RtColor, ThemeError> {
        Ok(resolved_to_ratatui(
            self.fg_color(color)?,
            self.color_mode(),
        ))
    }

    /// The `ratatui` background colour for `bg`.
    pub fn ratatui_bg(&self, bg: ThemeBg) -> Result<RtColor, ThemeError> {
        Ok(resolved_to_ratatui(self.bg_color(bg)?, self.color_mode()))
    }

    /// A `ratatui` [`Style`] carrying the foreground for `fg` and, when
    /// supplied, the background for `bg`. A [`ResolvedColor::Default`] slot
    /// becomes [`Color::Reset`], so `Style::default()`-equivalent terminal
    /// colours fall through untouched.
    ///
    /// This is the single seam the rt message / selector / chrome renderers
    /// use to colour a span from the active user theme.
    pub fn ratatui_style(
        &self,
        fg: ThemeColor,
        bg: Option<ThemeBg>,
    ) -> Result<RtStyle, ThemeError> {
        let mut style = RtStyle::default().fg(self.ratatui_fg(fg)?);
        if let Some(bg) = bg {
            style = style.bg(self.ratatui_bg(bg)?);
        }
        Ok(style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::built_in::dark_theme_json_str;
    use crate::modes::interactive::theme::core::ThemeJson;

    fn dark(mode: ColorMode) -> Theme {
        let json: ThemeJson = serde_json::from_str(dark_theme_json_str()).unwrap();
        Theme::from_json(&json, Some(mode)).unwrap()
    }

    #[test]
    fn hex_slot_maps_to_rgb_in_truecolor() {
        let theme = dark(ColorMode::Truecolor);
        // accent var resolves to "#8abeb7".
        assert_eq!(
            theme.ratatui_fg(ThemeColor::Accent).unwrap(),
            RtColor::Rgb(0x8a, 0xbe, 0xb7)
        );
    }

    #[test]
    fn hex_slot_quantises_to_indexed_in_256() {
        let theme = dark(ColorMode::Color256);
        // Same "#8abeb7" accent, but a 256-colour terminal narrows it to a
        // palette index — matching the ANSI path's quantisation.
        let expected = hex_to_256("#8abeb7").unwrap();
        assert_eq!(
            theme.ratatui_fg(ThemeColor::Accent).unwrap(),
            RtColor::Indexed(expected)
        );
    }

    #[test]
    fn empty_slot_maps_to_reset_not_black() {
        let theme = dark(ColorMode::Truecolor);
        // dark.json uses "" for `text` → terminal default → Color::Reset.
        assert_eq!(theme.ratatui_fg(ThemeColor::Text).unwrap(), RtColor::Reset);
    }

    #[test]
    fn palette_index_passes_through() {
        assert_eq!(
            resolved_to_ratatui(&ResolvedColor::Index(123), ColorMode::Truecolor),
            RtColor::Indexed(123)
        );
    }

    #[test]
    fn style_carries_fg_and_optional_bg() {
        let theme = dark(ColorMode::Truecolor);
        let style = theme
            .ratatui_style(ThemeColor::Accent, Some(ThemeBg::SelectedBg))
            .unwrap();
        assert_eq!(style.fg, Some(RtColor::Rgb(0x8a, 0xbe, 0xb7)));
        // selectedBg var resolves to "#3a3a4a".
        assert_eq!(style.bg, Some(RtColor::Rgb(0x3a, 0x3a, 0x4a)));
    }

    #[test]
    fn style_without_bg_leaves_bg_unset() {
        let theme = dark(ColorMode::Truecolor);
        let style = theme.ratatui_style(ThemeColor::Error, None).unwrap();
        assert!(style.fg.is_some());
        assert_eq!(style.bg, None);
    }

    #[test]
    fn malformed_hex_degrades_to_reset() {
        // A hex value that would fail parsing degrades rather than panicking.
        let c = resolved_to_ratatui(&ResolvedColor::Hex("#zzzzzz".into()), ColorMode::Truecolor);
        assert_eq!(c, RtColor::Reset);
    }

    /// End-to-end lock on the `custom-neon` tmux fixture: it must be a valid,
    /// loadable theme whose neon accent (`#ff00ff`) maps to the truecolor RGB
    /// the scenario greps for in the SGR stream (VAL-COMPAT-004). Keeps the
    /// fixture honest in CI, not just in the manual tmux run.
    #[test]
    fn custom_neon_fixture_loads_and_maps_accent_to_rgb() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/tui/themes/custom-neon.json");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let json: ThemeJson = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("custom-neon.json must deserialize: {e}"));
        let theme = Theme::from_json(&json, Some(ColorMode::Truecolor))
            .expect("custom-neon.json must build a Theme");
        assert_eq!(theme.name(), "custom-neon");
        // accent -> neonPink var -> "#ff00ff" -> 38;2;255;0;255 in the SGR stream.
        assert_eq!(
            theme.ratatui_fg(ThemeColor::Accent).unwrap(),
            RtColor::Rgb(0xff, 0x00, 0xff)
        );
    }
}
