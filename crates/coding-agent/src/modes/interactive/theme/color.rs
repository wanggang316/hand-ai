//! Color utilities for the interactive theme system.
//!
//! Handles:
//!
//! - Detection of the terminal's color capability (`truecolor` vs `256color`).
//! - Hex string parsing and 256-color quantisation for limited terminals.
//! - ANSI foreground / background escape generation.
//!
//! HTML export helpers (`ansi256ToHex`, `getResolvedThemeColors`,
//! `getThemeExportColors`, `isLightTheme`) are intentionally *not* ported in
//! this unit; they belong to the export pipeline which has not been wired up
//! in the Rust workspace yet. See
//! `docs/exec-plans/parity-completion.md` §A1.
//
// TODO(parity): port HTML export helpers when the export pipeline lands.

use std::env;

use thiserror::Error;

/// Terminal color capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit truecolor (`\x1b[38;2;R;G;Bm`).
    Truecolor,
    /// 256-color palette (`\x1b[38;5;Im`).
    Color256,
}

/// A theme color value: either a hex string (`#rrggbb`), an empty string
/// (means "default terminal foreground / background"), or a 256-palette
/// index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorValue {
    /// Hex string like `"#ff8800"`.
    Hex(String),
    /// Variable reference (e.g. `"accent"` -> looked up in the theme `vars`).
    VarRef(String),
    /// Empty string: keep the terminal default colour.
    Default,
    /// Explicit 256-palette index (0-255).
    Index(u8),
}

/// Errors returned by the colour parser.
#[derive(Debug, Error)]
pub enum ColorError {
    #[error("invalid hex color: {0}")]
    InvalidHex(String),
    #[error("invalid color value: {0}")]
    InvalidColor(String),
    #[error("circular variable reference: {0}")]
    CircularVarRef(String),
    #[error("variable reference not found: {0}")]
    UnknownVarRef(String),
}

/// Detect the terminal's colour capability from environment variables.
///
/// Heuristics:
/// - `COLORTERM=truecolor` / `24bit` -> truecolor
/// - Windows Terminal (`WT_SESSION`) -> truecolor
/// - `TERM` empty / `dumb` / `linux` -> 256
/// - Apple Terminal (`TERM_PROGRAM=Apple_Terminal`) -> 256
/// - GNU screen (`screen*`) -> 256 unless explicit `COLORTERM`
/// - everything else assumes truecolor (modern terminals).
pub fn detect_color_mode() -> ColorMode {
    detect_color_mode_from_env(EnvReader::process())
}

/// Test seam: detect colour mode with an injected environment reader.
pub(crate) fn detect_color_mode_from_env(env: EnvReader<'_>) -> ColorMode {
    let colorterm = env.get("COLORTERM").unwrap_or_default();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorMode::Truecolor;
    }
    if env.get("WT_SESSION").is_some() {
        return ColorMode::Truecolor;
    }
    let term = env.get("TERM").unwrap_or_default();
    if term.is_empty() || term == "dumb" || term == "linux" {
        return ColorMode::Color256;
    }
    if env.get("TERM_PROGRAM").as_deref() == Some("Apple_Terminal") {
        return ColorMode::Color256;
    }
    if term == "screen" || term.starts_with("screen-") || term.starts_with("screen.") {
        return ColorMode::Color256;
    }
    ColorMode::Truecolor
}

/// Function signature backing [`EnvReader`].
type EnvLookup<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;

/// Tiny abstraction over `std::env::var` so detection logic can be unit
/// tested without mutating process-global state.
pub(crate) struct EnvReader<'a> {
    lookup: EnvLookup<'a>,
}

impl<'a> EnvReader<'a> {
    /// Reader bound to the live process environment.
    pub(crate) fn process() -> EnvReader<'static> {
        EnvReader {
            lookup: Box::new(|key| env::var(key).ok()),
        }
    }

    /// Reader backed by an explicit `(key, value)` slice — used in tests.
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &'a [(&'a str, &'a str)]) -> Self {
        EnvReader {
            lookup: Box::new(move |key| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_string())
            }),
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        (self.lookup)(key)
    }
}

/// Parse `#rrggbb` into `(r, g, b)`. Accepts 6-hex-char strings with or
/// without a leading `#`.
pub fn hex_to_rgb(hex: &str) -> Result<(u8, u8, u8), ColorError> {
    let cleaned = hex.strip_prefix('#').unwrap_or(hex);
    if cleaned.len() != 6 {
        return Err(ColorError::InvalidHex(hex.to_string()));
    }
    let r = u8::from_str_radix(&cleaned[0..2], 16)
        .map_err(|_| ColorError::InvalidHex(hex.to_string()))?;
    let g = u8::from_str_radix(&cleaned[2..4], 16)
        .map_err(|_| ColorError::InvalidHex(hex.to_string()))?;
    let b = u8::from_str_radix(&cleaned[4..6], 16)
        .map_err(|_| ColorError::InvalidHex(hex.to_string()))?;
    Ok((r, g, b))
}

/// 6x6x6 colour cube channel values (palette indices 16..231).
const CUBE_VALUES: [u32; 6] = [0, 95, 135, 175, 215, 255];

/// Grayscale ramp values (palette indices 232..255).
fn gray_value(idx: usize) -> u32 {
    8 + idx as u32 * 10
}

fn closest_cube_index(value: u32) -> usize {
    let mut min_dist = u32::MAX;
    let mut min_idx = 0usize;
    for (i, c) in CUBE_VALUES.iter().enumerate() {
        let dist = value.abs_diff(*c);
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

fn closest_gray_index(gray: u32) -> usize {
    let mut min_dist = u32::MAX;
    let mut min_idx = 0usize;
    for i in 0..24 {
        let dist = gray.abs_diff(gray_value(i));
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

/// Weighted Euclidean distance favouring green.
fn color_distance(r1: u32, g1: u32, b1: u32, r2: u32, g2: u32, b2: u32) -> f64 {
    let dr = r1 as f64 - r2 as f64;
    let dg = g1 as f64 - g2 as f64;
    let db = b1 as f64 - b2 as f64;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

/// Quantise `(r, g, b)` to the closest 256-palette index, preferring the
/// 6x6x6 cube unless the colour is essentially neutral.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let r_u = r as u32;
    let g_u = g as u32;
    let b_u = b as u32;

    let r_idx = closest_cube_index(r_u);
    let g_idx = closest_cube_index(g_u);
    let b_idx = closest_cube_index(b_u);
    let cube_r = CUBE_VALUES[r_idx];
    let cube_g = CUBE_VALUES[g_idx];
    let cube_b = CUBE_VALUES[b_idx];
    let cube_index = 16 + 36 * r_idx + 6 * g_idx + b_idx;
    let cube_dist = color_distance(r_u, g_u, b_u, cube_r, cube_g, cube_b);

    let gray = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round() as u32;
    let gray_idx = closest_gray_index(gray);
    let gray_val = gray_value(gray_idx);
    let gray_index = 232 + gray_idx;
    let gray_dist = color_distance(r_u, g_u, b_u, gray_val, gray_val, gray_val);

    let max_c = r_u.max(g_u).max(b_u);
    let min_c = r_u.min(g_u).min(b_u);
    let spread = max_c - min_c;

    if spread < 10 && gray_dist < cube_dist {
        gray_index as u8
    } else {
        cube_index as u8
    }
}

/// Quantise a `#rrggbb` string to the closest 256-palette index.
pub fn hex_to_256(hex: &str) -> Result<u8, ColorError> {
    let (r, g, b) = hex_to_rgb(hex)?;
    Ok(rgb_to_256(r, g, b))
}

/// Build the foreground ANSI escape for a resolved colour value (no
/// trailing reset).
pub fn fg_ansi(value: &ResolvedColor, mode: ColorMode) -> String {
    match value {
        ResolvedColor::Default => "\x1b[39m".to_string(),
        ResolvedColor::Index(i) => format!("\x1b[38;5;{}m", i),
        ResolvedColor::Hex(hex) => match mode {
            ColorMode::Truecolor => {
                let (r, g, b) = hex_to_rgb(hex).unwrap_or((0, 0, 0));
                format!("\x1b[38;2;{};{};{}m", r, g, b)
            }
            ColorMode::Color256 => {
                let idx = hex_to_256(hex).unwrap_or(0);
                format!("\x1b[38;5;{}m", idx)
            }
        },
    }
}

/// Build the background ANSI escape for a resolved colour value (no
/// trailing reset).
pub fn bg_ansi(value: &ResolvedColor, mode: ColorMode) -> String {
    match value {
        ResolvedColor::Default => "\x1b[49m".to_string(),
        ResolvedColor::Index(i) => format!("\x1b[48;5;{}m", i),
        ResolvedColor::Hex(hex) => match mode {
            ColorMode::Truecolor => {
                let (r, g, b) = hex_to_rgb(hex).unwrap_or((0, 0, 0));
                format!("\x1b[48;2;{};{};{}m", r, g, b)
            }
            ColorMode::Color256 => {
                let idx = hex_to_256(hex).unwrap_or(0);
                format!("\x1b[48;5;{}m", idx)
            }
        },
    }
}

/// A `ColorValue` after variable substitution: hex, palette index, or
/// terminal default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedColor {
    Hex(String),
    Index(u8),
    Default,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_truecolor_via_colorterm() {
        let mode = detect_color_mode_from_env(EnvReader::from_pairs(&[("COLORTERM", "truecolor")]));
        assert_eq!(mode, ColorMode::Truecolor);
    }

    #[test]
    fn detect_256_via_apple_terminal() {
        let mode = detect_color_mode_from_env(EnvReader::from_pairs(&[
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("TERM", "xterm-256color"),
        ]));
        assert_eq!(mode, ColorMode::Color256);
    }

    #[test]
    fn detect_256_under_screen() {
        let mode =
            detect_color_mode_from_env(EnvReader::from_pairs(&[("TERM", "screen-256color")]));
        assert_eq!(mode, ColorMode::Color256);
    }

    #[test]
    fn detect_truecolor_default() {
        // Modern terminal with neither blacklist trigger nor explicit COLORTERM.
        let mode = detect_color_mode_from_env(EnvReader::from_pairs(&[("TERM", "xterm-256color")]));
        assert_eq!(mode, ColorMode::Truecolor);
    }

    #[test]
    fn parses_hex_with_and_without_hash() {
        assert_eq!(hex_to_rgb("#ff8800").unwrap(), (0xff, 0x88, 0x00));
        assert_eq!(hex_to_rgb("00ffaa").unwrap(), (0x00, 0xff, 0xaa));
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(hex_to_rgb("#fff").is_err());
        assert!(hex_to_rgb("zz0000").is_err());
    }

    #[test]
    fn quantises_pure_red_to_cube() {
        // pure red => cube index 196 (16 + 5*36 + 0 + 0)
        assert_eq!(rgb_to_256(255, 0, 0), 196);
    }

    #[test]
    fn quantises_neutral_gray_to_grayscale_ramp() {
        // 0x80 = 128 — neutral, closest gray is index 232 + 12 = 244.
        let idx = rgb_to_256(0x80, 0x80, 0x80);
        assert!(
            (232..=255).contains(&idx),
            "expected grayscale ramp, got {}",
            idx
        );
    }

    #[test]
    fn fg_truecolor_for_hex() {
        let s = fg_ansi(&ResolvedColor::Hex("#ff8800".into()), ColorMode::Truecolor);
        assert_eq!(s, "\x1b[38;2;255;136;0m");
    }

    #[test]
    fn fg_256_for_hex() {
        let s = fg_ansi(&ResolvedColor::Hex("#ff0000".into()), ColorMode::Color256);
        assert_eq!(s, "\x1b[38;5;196m");
    }

    #[test]
    fn fg_default_for_empty() {
        let s = fg_ansi(&ResolvedColor::Default, ColorMode::Truecolor);
        assert_eq!(s, "\x1b[39m");
    }

    #[test]
    fn bg_default_for_empty() {
        let s = bg_ansi(&ResolvedColor::Default, ColorMode::Truecolor);
        assert_eq!(s, "\x1b[49m");
    }

    #[test]
    fn bg_index_passthrough() {
        let s = bg_ansi(&ResolvedColor::Index(123), ColorMode::Truecolor);
        assert_eq!(s, "\x1b[48;5;123m");
    }
}
