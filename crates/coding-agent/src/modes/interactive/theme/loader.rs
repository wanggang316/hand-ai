//! Theme loader: file IO + custom-theme directory discovery.
//!
//! Custom themes live in `~/.hand/themes/`.
//!
//! Not yet implemented:
//! - In-memory registry for plugin-injected themes — to add when an
//!   extension API needs it.
//! - File watching for hot-reload — follow-up once a consumer needs it.
//
// TODO: theme registry (for plugin-injected themes).
// TODO: hot-reload watcher.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::built_in::{BUILTIN_THEME_NAMES, builtin_theme_json};
use super::color::ColorMode;
use super::core::{Theme, ThemeError, ThemeJson};

/// Errors the loader can surface to a caller.
#[derive(Debug, Error)]
pub enum ThemeLoadError {
    #[error("theme not found: {0}")]
    NotFound(String),
    #[error("failed to read theme {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse theme {label}: {source}")]
    Parse {
        label: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Theme(#[from] ThemeError),
    #[error("home directory unavailable; cannot resolve custom theme dir")]
    NoHomeDir,
}

/// Theme entry returned by `available_themes_with_paths`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    pub path: Option<PathBuf>,
}

/// Default custom-theme directory (`~/.hand/themes`).
pub fn default_custom_themes_dir() -> Result<PathBuf, ThemeLoadError> {
    let home = dirs::home_dir().ok_or(ThemeLoadError::NoHomeDir)?;
    Ok(home.join(".hand").join("themes"))
}

/// Sorted union of built-in theme names and `<name>.json` files found in
/// `custom_themes_dir`. Built-in names always appear; missing dir is fine.
pub fn available_themes(custom_themes_dir: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for n in BUILTIN_THEME_NAMES {
        names.insert((*n).to_string());
    }
    if let Ok(entries) = fs::read_dir(custom_themes_dir) {
        for entry in entries.flatten() {
            if let Some(stem) = json_stem(&entry.path()) {
                names.insert(stem);
            }
        }
    }
    names.into_iter().collect()
}

/// Like `available_themes`, but pairs each name with its source path.
/// Built-ins return `None` for the path (they're embedded).
pub fn available_themes_with_paths(custom_themes_dir: &Path) -> Vec<ThemeInfo> {
    let mut out: Vec<ThemeInfo> = BUILTIN_THEME_NAMES
        .iter()
        .map(|n| ThemeInfo {
            name: (*n).to_string(),
            path: None,
        })
        .collect();
    if let Ok(entries) = fs::read_dir(custom_themes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(stem) = json_stem(&path)
                && !out.iter().any(|t| t.name == stem)
            {
                out.push(ThemeInfo {
                    name: stem,
                    path: Some(path),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse a theme JSON file straight from disk and build a `Theme`.
pub fn load_theme_from_path(
    theme_path: &Path,
    mode: Option<ColorMode>,
) -> Result<Theme, ThemeLoadError> {
    let content = fs::read_to_string(theme_path).map_err(|source| ThemeLoadError::Io {
        path: theme_path.to_path_buf(),
        source,
    })?;
    let json = parse_theme_json_content(&theme_path.display().to_string(), &content)?;
    Ok(Theme::from_json_with_path(
        &json,
        mode,
        Some(theme_path.display().to_string()),
    )?)
}

/// Resolve a theme by its short name. Built-ins come from the embedded
/// JSON; otherwise loads `<custom_themes_dir>/<name>.json`.
pub fn load_theme(
    name: &str,
    custom_themes_dir: &Path,
    mode: Option<ColorMode>,
) -> Result<Theme, ThemeLoadError> {
    if let Some(json) = builtin_theme_json(name) {
        return Ok(Theme::from_json(json, mode)?);
    }
    let path = custom_themes_dir.join(format!("{}.json", name));
    if !path.exists() {
        return Err(ThemeLoadError::NotFound(name.to_string()));
    }
    load_theme_from_path(&path, mode)
}

/// Convenience wrapper that swallows errors and returns `None` — useful
/// for the "fall back to dark on any failure" pattern in the driver.
pub fn theme_by_name(
    name: &str,
    custom_themes_dir: &Path,
    mode: Option<ColorMode>,
) -> Option<Theme> {
    load_theme(name, custom_themes_dir, mode).ok()
}

/// The theme the rt driver falls back to when a configured theme cannot be
/// resolved. `dark` is baked into the binary and always constructs, so this
/// is infallible.
///
/// `expect` here is a genuine invariant, not error handling: the embedded
/// `dark.json` is validated by a `built_in` unit test, so a failure would be
/// a build-time regression, never a user-facing condition.
#[must_use]
pub fn default_theme(mode: Option<ColorMode>) -> Theme {
    super::built_in::dark_theme(mode).expect("built-in dark theme must construct")
}

/// Outcome of [`resolve_theme_or_default`]: the resolved theme plus, when a
/// fallback happened, a human-readable reason the caller can surface (e.g. a
/// startup diagnostic line).
#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    /// The theme to render with. Never a hard failure — always at least the
    /// built-in default.
    pub theme: Theme,
    /// `Some(reason)` when the requested theme could not be loaded and the
    /// default was substituted; `None` on a clean resolve.
    pub fallback_reason: Option<String>,
}

/// Resolve a configured theme name to a concrete [`Theme`], **never failing**.
///
/// This is the compatibility / tolerance seam for the rt driver:
///
/// - a **known** theme (built-in or a valid custom `<name>.json`) loads as-is;
/// - `"system"` follows the detected terminal background to `dark` / `light`
///   (both built-in, so this always succeeds);
/// - an **unknown** name (e.g. a `high-contrast` setting with no matching
///   custom file, or a `/theme bogus` typo) falls back to the default palette;
/// - a **corrupt / partial** custom JSON (malformed, or a missing colour slot)
///   fails to parse or build and likewise falls back to the default palette.
///
/// In every fallback case the returned [`ResolvedTheme::fallback_reason`] is
/// populated so the caller can land a diagnostic without the session breaking.
#[must_use]
pub fn resolve_theme_or_default(
    name: &str,
    custom_themes_dir: &Path,
    mode: Option<ColorMode>,
) -> ResolvedTheme {
    // `system` is not itself a theme file — resolve it to the built-in that
    // matches the terminal background, then load that.
    let requested = if name.eq_ignore_ascii_case("system") {
        detect_terminal_background().to_string()
    } else {
        name.to_string()
    };

    match load_theme(&requested, custom_themes_dir, mode) {
        Ok(theme) => ResolvedTheme {
            theme,
            fallback_reason: None,
        },
        Err(err) => {
            let reason = match &err {
                ThemeLoadError::NotFound(n) => {
                    format!("unknown theme \"{n}\"; using default")
                }
                ThemeLoadError::Parse { label, .. } => {
                    format!("theme \"{requested}\" is malformed ({label}); using default")
                }
                ThemeLoadError::Theme(_) => {
                    format!("theme \"{requested}\" has an invalid colour slot; using default")
                }
                other => format!("could not load theme \"{requested}\": {other}; using default"),
            };
            ResolvedTheme {
                theme: default_theme(mode),
                fallback_reason: Some(reason),
            }
        }
    }
}

/// Detect the terminal background ("dark" / "light") from `COLORFGBG`.
/// Defaults to `"dark"` when the variable is missing or malformed.
pub fn detect_terminal_background() -> &'static str {
    detect_terminal_background_from_env(std::env::var("COLORFGBG").ok().as_deref())
}

/// Test seam for `detect_terminal_background`.
pub(crate) fn detect_terminal_background_from_env(colorfgbg: Option<&str>) -> &'static str {
    let Some(value) = colorfgbg else {
        return "dark";
    };
    let parts: Vec<&str> = value.split(';').collect();
    if parts.len() < 2 {
        return "dark";
    }
    match parts[1].parse::<u32>() {
        Ok(bg) if bg < 8 => "dark",
        Ok(_) => "light",
        Err(_) => "dark",
    }
}

fn parse_theme_json_content(label: &str, content: &str) -> Result<ThemeJson, ThemeLoadError> {
    serde_json::from_str::<ThemeJson>(content).map_err(|source| ThemeLoadError::Parse {
        label: label.to_string(),
        source,
    })
}

fn json_stem(path: &Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return None;
    }
    path.file_stem()?.to_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn write_theme(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(format!("{}.json", name));
        fs::write(&path, contents).unwrap();
        path
    }

    fn minimal_theme_json(name: &str) -> String {
        // Use the dark theme content but rename it.
        let dark = super::super::built_in::dark_theme_json_str();
        dark.replace("\"name\": \"dark\"", &format!("\"name\": \"{}\"", name))
    }

    #[test]
    fn available_themes_includes_builtins() {
        let dir = TempDir::new().unwrap();
        let names = available_themes(dir.path());
        assert!(names.contains(&"dark".to_string()));
        assert!(names.contains(&"light".to_string()));
    }

    #[test]
    fn available_themes_includes_custom_files() {
        let dir = TempDir::new().unwrap();
        write_theme(dir.path(), "ocean", &minimal_theme_json("ocean"));
        let names = available_themes(dir.path());
        assert!(names.contains(&"ocean".to_string()));
    }

    #[test]
    fn available_themes_with_paths_dedupes_builtin() {
        let dir = TempDir::new().unwrap();
        // Even if a user shadows "dark", built-in entry remains and the
        // custom path is *not* added.
        write_theme(dir.path(), "dark", &minimal_theme_json("dark"));
        let infos = available_themes_with_paths(dir.path());
        let dark_entries: Vec<_> = infos.iter().filter(|t| t.name == "dark").collect();
        assert_eq!(dark_entries.len(), 1);
        assert!(dark_entries[0].path.is_none());
    }

    #[test]
    fn load_theme_resolves_builtin() {
        let dir = TempDir::new().unwrap();
        let theme = load_theme("dark", dir.path(), Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.name(), "dark");
    }

    #[test]
    fn load_theme_resolves_custom_file() {
        let dir = TempDir::new().unwrap();
        write_theme(dir.path(), "ocean", &minimal_theme_json("ocean"));
        let theme = load_theme("ocean", dir.path(), Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.name(), "ocean");
        assert!(theme.source_path().is_some());
    }

    #[test]
    fn load_theme_unknown_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let err = load_theme("missing", dir.path(), None).unwrap_err();
        assert!(matches!(err, ThemeLoadError::NotFound(name) if name == "missing"));
    }

    #[test]
    fn theme_by_name_swallows_errors() {
        let dir = TempDir::new().unwrap();
        assert!(theme_by_name("missing", dir.path(), None).is_none());
        assert!(theme_by_name("dark", dir.path(), Some(ColorMode::Truecolor)).is_some());
    }

    #[test]
    fn detect_terminal_bg_dark_default() {
        assert_eq!(detect_terminal_background_from_env(None), "dark");
        assert_eq!(detect_terminal_background_from_env(Some("invalid")), "dark");
    }

    #[test]
    fn detect_terminal_bg_uses_second_field() {
        assert_eq!(detect_terminal_background_from_env(Some("15;0")), "dark");
        assert_eq!(detect_terminal_background_from_env(Some("0;15")), "light");
    }

    // ------------------------------------------------------------------
    // resolve_theme_or_default — the rt-driver tolerance seam
    // (VAL-COMPAT-004 / 005 / 016)
    // ------------------------------------------------------------------

    #[test]
    fn resolve_known_custom_theme_has_no_fallback() {
        // VAL-COMPAT-004: a valid custom theme JSON loads and colours the UI.
        let dir = TempDir::new().unwrap();
        write_theme(dir.path(), "ocean", &minimal_theme_json("ocean"));
        let resolved = resolve_theme_or_default("ocean", dir.path(), Some(ColorMode::Truecolor));
        assert!(resolved.fallback_reason.is_none());
        assert_eq!(resolved.theme.name(), "ocean");
    }

    #[test]
    fn resolve_builtin_has_no_fallback() {
        let dir = TempDir::new().unwrap();
        let resolved = resolve_theme_or_default("light", dir.path(), Some(ColorMode::Truecolor));
        assert!(resolved.fallback_reason.is_none());
        assert_eq!(resolved.theme.name(), "light");
    }

    #[test]
    fn resolve_unknown_theme_falls_back_to_default() {
        // VAL-COMPAT-005: an unknown theme name (settings typo, or a
        // `high-contrast` setting with no matching custom file) renders the
        // default palette rather than crashing.
        let dir = TempDir::new().unwrap();
        let resolved = resolve_theme_or_default("bogus", dir.path(), Some(ColorMode::Truecolor));
        assert_eq!(resolved.theme.name(), "dark");
        let reason = resolved.fallback_reason.expect("fallback reason present");
        assert!(reason.contains("unknown theme"), "reason: {reason}");
        assert!(reason.contains("bogus"), "reason: {reason}");
    }

    #[test]
    fn resolve_high_contrast_without_file_falls_back() {
        // `high-contrast` is a persistable setting but has no built-in JSON;
        // absent a custom file it must fall back, not error.
        let dir = TempDir::new().unwrap();
        let resolved =
            resolve_theme_or_default("high-contrast", dir.path(), Some(ColorMode::Truecolor));
        assert_eq!(resolved.theme.name(), "dark");
        assert!(resolved.fallback_reason.is_some());
    }

    #[test]
    fn resolve_corrupt_theme_json_falls_back_to_default() {
        // VAL-COMPAT-016: malformed JSON must not crash — fall back cleanly.
        let dir = TempDir::new().unwrap();
        write_theme(dir.path(), "broken", "{ this is not valid json ]");
        let resolved = resolve_theme_or_default("broken", dir.path(), Some(ColorMode::Truecolor));
        assert_eq!(resolved.theme.name(), "dark");
        let reason = resolved.fallback_reason.expect("fallback reason present");
        assert!(reason.contains("malformed"), "reason: {reason}");
    }

    #[test]
    fn resolve_partial_theme_json_falls_back_to_default() {
        // A syntactically valid JSON that is missing a required colour slot
        // fails to deserialize into the exhaustive `ThemeColors`; it must
        // fall back rather than error.
        let dir = TempDir::new().unwrap();
        write_theme(
            dir.path(),
            "partial",
            r##"{ "name": "partial", "colors": { "accent": "#ff0000" } }"##,
        );
        let resolved = resolve_theme_or_default("partial", dir.path(), Some(ColorMode::Truecolor));
        assert_eq!(resolved.theme.name(), "dark");
        assert!(resolved.fallback_reason.is_some());
    }

    #[test]
    fn resolve_system_follows_terminal_background() {
        // `system` is not a file — it resolves to a built-in via the detected
        // background, so it always succeeds without a fallback reason.
        let dir = TempDir::new().unwrap();
        let resolved = resolve_theme_or_default("system", dir.path(), Some(ColorMode::Truecolor));
        assert!(resolved.fallback_reason.is_none());
        // The detected background picks dark or light; both are built-ins.
        assert!(matches!(resolved.theme.name(), "dark" | "light"));
    }

    #[test]
    fn default_theme_is_dark_and_infallible() {
        assert_eq!(default_theme(Some(ColorMode::Truecolor)).name(), "dark");
    }
}
