//! Theme loader: file IO + custom-theme directory discovery.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/theme/theme.ts`
//! (`getAvailableThemes`, `getAvailableThemesWithPaths`, `loadThemeFromPath`,
//! `loadTheme`, `getThemeByName`).
//!
//! Differences from pi-mono:
//!
//! - Custom themes live in `~/.hand/themes/` (matching the rest of the
//!   `hand` workspace), not `~/.config/pi/themes/`.
//! - There's no in-memory `registeredThemes` registry yet — pi-mono uses it
//!   for plugin-injected themes; we'll add it back when an extension API
//!   needs it.
//! - File watching (`startThemeWatcher`) is intentionally *not* ported;
//!   that's a follow-up unit once a consumer needs hot-reload.
//
// TODO(parity): port theme registry (`setRegisteredThemes`).
// TODO(parity): port hot-reload watcher (`startThemeWatcher`).

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
        // custom path is *not* added (matches pi-mono).
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
}
