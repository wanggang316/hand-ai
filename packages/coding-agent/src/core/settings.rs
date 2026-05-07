//! User and project settings, merged from layered YAML files.
//!
//! Resolution order: project (`<cwd>/.hand/settings.yaml`) > global
//! (`~/.hand/agent/settings.yaml`) > defaults. The first level that supplies
//! a field wins; otherwise the default is used.
//!
//! All scalar fields are `Option<T>` so merging is mechanical (project's
//! `Some` shadows base). Accessor methods on the sub-structs return concrete
//! values with defaults applied — call sites that need the merged value of a
//! sub-struct field should go through the accessor rather than reading the
//! raw `Option` directly.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Top-level settings shape.
///
/// Both YAML layers deserialize into this same struct. Field-by-field merge
/// is performed in [`Settings::merge`].
///
/// The derived `Default` produces an "all-`None`" instance so that merging
/// layered files preserves the "field was not specified in this layer"
/// signal. The user-facing default values live in [`Settings::defaults`] and
/// are applied as the lowest layer in [`Settings::load`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct Settings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    pub shell_path: Option<PathBuf>,
    pub shell_command_prefix: Option<String>,
    pub theme: Option<ThemeSetting>,
    pub compaction: CompactionSettings,
    pub retry: RetrySettings,
    pub quiet_startup: Option<bool>,
}

/// Compaction tuning, in YAML-parse shape.
///
/// **Read effective values via the accessor methods** ([`enabled`](Self::enabled),
/// [`threshold`](Self::threshold), [`keep_recent_tokens`](Self::keep_recent_tokens),
/// [`max_context_tokens`](Self::max_context_tokens)) — direct field access yields
/// `Option<T>` because the struct represents the raw YAML layer (all fields are
/// optional so layered merging via [`Settings::merge`] works mechanically). The
/// accessors apply the documented defaults when a field is `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct CompactionSettings {
    pub enabled: Option<bool>,
    pub threshold: Option<f32>,
    pub keep_recent_tokens: Option<u32>,
    pub max_context_tokens: Option<u32>,
}

impl CompactionSettings {
    /// Build the populated default form (all `Some`) — the lowest layer
    /// beneath user-supplied YAML.
    pub fn with_defaults() -> Self {
        Self {
            enabled: Some(true),
            threshold: Some(0.8),
            keep_recent_tokens: Some(32_000),
            max_context_tokens: Some(200_000),
        }
    }

    /// Whether auto-compaction is enabled. Default: `true`.
    //
    // The literal default here is intentionally duplicated with
    // `with_defaults()`: it covers both `Settings::default()` callers (whose
    // sub-struct is all-`None`) and accessor-direct callers that never went
    // through layered merging. Same pattern applies to the sibling accessors.
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Fraction of `max_context_tokens` at which compaction kicks in.
    /// Default: `0.8`.
    pub fn threshold(&self) -> f32 {
        self.threshold.unwrap_or(0.8)
    }

    /// Token budget reserved for the most recent messages (which are kept
    /// verbatim during compaction). Default: `32_000`.
    pub fn keep_recent_tokens(&self) -> u32 {
        self.keep_recent_tokens.unwrap_or(32_000)
    }

    /// Total context window assumed for the model. Default: `200_000`.
    pub fn max_context_tokens(&self) -> u32 {
        self.max_context_tokens.unwrap_or(200_000)
    }

    /// Merge `project` on top of `base`: project's `Some` wins per field.
    fn merge(base: Self, project: Self) -> Self {
        Self {
            enabled: project.enabled.or(base.enabled),
            threshold: project.threshold.or(base.threshold),
            keep_recent_tokens: project.keep_recent_tokens.or(base.keep_recent_tokens),
            max_context_tokens: project.max_context_tokens.or(base.max_context_tokens),
        }
    }
}

/// Retry tuning for API calls, in YAML-parse shape.
///
/// **Read effective values via the accessor methods** ([`enabled`](Self::enabled),
/// [`max_retries`](Self::max_retries), [`initial_delay_ms`](Self::initial_delay_ms),
/// [`max_delay_ms`](Self::max_delay_ms)) — direct field access yields `Option<T>`
/// because the struct represents the raw YAML layer (all fields are optional so
/// layered merging via [`Settings::merge`] works mechanically). The accessors
/// apply the documented defaults when a field is `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct RetrySettings {
    pub enabled: Option<bool>,
    pub max_retries: Option<u32>,
    pub initial_delay_ms: Option<u32>,
    pub max_delay_ms: Option<u32>,
}

impl RetrySettings {
    /// Build the populated default form (all `Some`).
    pub fn with_defaults() -> Self {
        Self {
            enabled: Some(true),
            max_retries: Some(3),
            initial_delay_ms: Some(1_000),
            max_delay_ms: Some(30_000),
        }
    }

    /// Whether retries are enabled. Default: `true`.
    //
    // The literal default here is intentionally duplicated with
    // `with_defaults()`: it covers both `Settings::default()` callers (whose
    // sub-struct is all-`None`) and accessor-direct callers that never went
    // through layered merging. Same pattern applies to the sibling accessors.
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn max_retries(&self) -> u32 {
        self.max_retries.unwrap_or(3)
    }

    pub fn initial_delay_ms(&self) -> u32 {
        self.initial_delay_ms.unwrap_or(1_000)
    }

    pub fn max_delay_ms(&self) -> u32 {
        self.max_delay_ms.unwrap_or(30_000)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            enabled: project.enabled.or(base.enabled),
            max_retries: project.max_retries.or(base.max_retries),
            initial_delay_ms: project.initial_delay_ms.or(base.initial_delay_ms),
            max_delay_ms: project.max_delay_ms.or(base.max_delay_ms),
        }
    }
}

/// UI theme.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeSetting {
    #[default]
    Dark,
    Light,
    HighContrast,
    System,
}

/// Default reasoning effort for thinking-capable models.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingLevelSetting {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

/// Errors raised by [`Settings::load`].
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("I/O error reading {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML parse error in {path}: {source}", path = .path.display())]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
}

impl Settings {
    /// User-facing defaults — the values documented in the README. Used as
    /// the lowest layer beneath the global and project YAML files.
    ///
    /// Distinct from [`Settings::default`] (derived) which produces an
    /// "all-`None`" instance so that layer merging can tell apart "not
    /// specified" from "explicitly the default value".
    pub fn defaults() -> Self {
        Self {
            default_provider: Some("anthropic".into()),
            default_model: Some("claude-sonnet-4-20250514".into()),
            default_thinking_level: None,
            shell_path: None,
            shell_command_prefix: None,
            theme: Some(ThemeSetting::Dark),
            compaction: CompactionSettings::with_defaults(),
            retry: RetrySettings::with_defaults(),
            quiet_startup: Some(false),
        }
    }

    /// Effective theme — the merged value or [`ThemeSetting::Dark`] if unset.
    pub fn theme(&self) -> ThemeSetting {
        self.theme.unwrap_or_default()
    }

    /// Effective `quiet_startup` flag — defaults to `false` if unset.
    pub fn quiet_startup(&self) -> bool {
        self.quiet_startup.unwrap_or(false)
    }

    /// Merge `project` on top of `base` (typically the global layer or
    /// [`Settings::default`]). Pure: produces a new `Settings`, mutates
    /// nothing. Field-by-field — for `Option<T>` fields a `Some` in
    /// `project` wins; sub-structs merge recursively per the same rule.
    pub fn merge(base: Self, project: Self) -> Self {
        Self {
            default_provider: project.default_provider.or(base.default_provider),
            default_model: project.default_model.or(base.default_model),
            default_thinking_level: project
                .default_thinking_level
                .or(base.default_thinking_level),
            shell_path: project.shell_path.or(base.shell_path),
            shell_command_prefix: project.shell_command_prefix.or(base.shell_command_prefix),
            theme: project.theme.or(base.theme),
            compaction: CompactionSettings::merge(base.compaction, project.compaction),
            retry: RetrySettings::merge(base.retry, project.retry),
            quiet_startup: project.quiet_startup.or(base.quiet_startup),
        }
    }

    /// Load global and project layers from disk and merge them on top of
    /// [`Settings::default`].
    ///
    /// A path that points to a non-existent file is treated as "not
    /// configured" (no error). YAML parse errors and other I/O errors are
    /// surfaced as [`SettingsError`].
    ///
    /// Unknown top-level YAML keys are ignored with a `tracing::warn!`
    /// diagnostic — matches the TS implementation in `pi-mono`.
    pub fn load(
        global_path: Option<&Path>,
        project_path: Option<&Path>,
    ) -> Result<Self, SettingsError> {
        let global = match global_path {
            Some(p) => load_yaml_layer(p)?.unwrap_or_default(),
            None => Settings::default(),
        };
        let project = match project_path {
            Some(p) => load_yaml_layer(p)?.unwrap_or_default(),
            None => Settings::default(),
        };
        // Order: defaults < global < project. Each layer above only
        // contributes the fields it explicitly set (everything else is
        // `None` and falls through).
        let with_global = Settings::merge(Settings::defaults(), global);
        Ok(Settings::merge(with_global, project))
    }
}

/// Read one layer from disk. Returns `Ok(None)` if the file does not exist;
/// `Ok(Some(settings))` if it loaded (even if it was empty); a
/// [`SettingsError`] for any other I/O or parse failure.
fn load_yaml_layer(path: &Path) -> Result<Option<Settings>, SettingsError> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    parse_yaml_with_warning(path, &content).map(Some)
}

/// Parse a YAML string into [`Settings`]. Unknown top-level keys emit a
/// `tracing::warn!` but do not error.
fn parse_yaml_with_warning(path: &Path, content: &str) -> Result<Settings, SettingsError> {
    if content.trim().is_empty() {
        return Ok(Settings::default());
    }

    // First pass: parse as a generic mapping so we can detect unknown
    // top-level keys for the warning. `serde(default)` on `Settings`
    // already silently ignores unknown keys, so the second pass is the
    // one whose result we keep.
    if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(content)
    {
        let known: &[&str] = &[
            "default-provider",
            "default-model",
            "default-thinking-level",
            "shell-path",
            "shell-command-prefix",
            "theme",
            "compaction",
            "retry",
            "quiet-startup",
        ];
        for (k, _) in map.iter() {
            if let Some(key) = k.as_str().filter(|k| !known.contains(k)) {
                tracing::warn!(
                    path = %path.display(),
                    key = %key,
                    "ignoring unknown settings key",
                );
            }
        }
    }

    serde_yaml::from_str::<Settings>(content).map_err(|source| SettingsError::Yaml {
        path: path.to_path_buf(),
        source,
    })
}

/// Manages loading settings from the standard hand layout.
///
/// Holds the merged [`Settings`] together with the resolved layer paths.
/// Persistence (writing back to disk) is intentionally out of scope here —
/// the TUI does not edit YAML, and a watcher-driven reload story lands in a
/// later task.
pub struct SettingsManager {
    settings: Settings,
    project_path: Option<PathBuf>,
    global_path: Option<PathBuf>,
}

impl SettingsManager {
    /// Construct from the standard hand paths:
    /// - global: `~/.hand/agent/settings.yaml`
    /// - project: `<cwd>/.hand/settings.yaml`
    ///
    /// Either layer being absent is fine — defaults are used. YAML parse
    /// errors propagate.
    pub fn from_cwd(cwd: &Path) -> Result<Self, SettingsError> {
        let global_path = dirs::home_dir().map(|h| h.join(".hand/agent/settings.yaml"));
        let project_path = Some(cwd.join(".hand/settings.yaml"));

        let settings = Settings::load(global_path.as_deref(), project_path.as_deref())?;
        Ok(Self {
            settings,
            project_path,
            global_path,
        })
    }

    /// Construct with in-memory defaults — no disk I/O. Intended for tests.
    pub fn in_memory() -> Self {
        Self {
            settings: Settings::defaults(),
            project_path: None,
            global_path: None,
        }
    }

    /// Borrow the merged settings.
    pub fn current(&self) -> &Settings {
        &self.settings
    }

    /// Backward-compatible alias for [`Self::current`]. Some call sites use
    /// `mgr.settings()` from the previous JSON-backed API.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Effective compaction settings. The returned value is a clone — call
    /// sites that need scalar fields should go through the accessor methods
    /// (e.g. `mgr.compaction_settings().keep_recent_tokens()`).
    pub fn compaction_settings(&self) -> CompactionSettings {
        self.settings.compaction.clone()
    }

    /// Effective retry settings.
    pub fn retry_settings(&self) -> RetrySettings {
        self.settings.retry.clone()
    }

    /// Resolved `shell-path` setting if configured.
    pub fn shell_path(&self) -> Option<&Path> {
        self.settings.shell_path.as_deref()
    }

    /// Resolved `shell-command-prefix` setting if configured.
    pub fn shell_command_prefix(&self) -> Option<&str> {
        self.settings.shell_command_prefix.as_deref()
    }

    /// Path of the global layer, if known.
    pub fn global_path(&self) -> Option<&Path> {
        self.global_path.as_deref()
    }

    /// Path of the project layer, if known.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_yaml(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_match_readme_table() {
        let s = Settings::defaults();
        assert_eq!(s.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(s.default_model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert!(s.default_thinking_level.is_none());
        assert!(s.shell_path.is_none());
        assert!(s.shell_command_prefix.is_none());
        assert_eq!(s.theme(), ThemeSetting::Dark);
        assert!(s.compaction.enabled());
        assert!((s.compaction.threshold() - 0.8).abs() < f32::EPSILON);
        assert_eq!(s.compaction.keep_recent_tokens(), 32_000);
        assert_eq!(s.compaction.max_context_tokens(), 200_000);
        assert!(s.retry.enabled());
        assert_eq!(s.retry.max_retries(), 3);
        assert_eq!(s.retry.initial_delay_ms(), 1_000);
        assert_eq!(s.retry.max_delay_ms(), 30_000);
        assert!(!s.quiet_startup());
    }

    #[test]
    fn empty_yaml_round_trips_to_defaults() {
        let dir = TempDir::new().unwrap();
        let p = write_yaml(&dir, "settings.yaml", "");
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s, Settings::defaults());
    }

    #[test]
    fn single_field_override_leaves_others_at_default() {
        let dir = TempDir::new().unwrap();
        let p = write_yaml(&dir, "settings.yaml", "default-model: gpt-4o\n");
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("gpt-4o"));
        // Other fields still match defaults
        assert_eq!(s.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(s.theme(), ThemeSetting::Dark);
    }

    #[test]
    fn project_shadows_global() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "default-model: claude-x\n");
        let p = write_yaml(&dir, "project.yaml", "default-model: gpt-4o\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn project_doesnt_shadow_when_absent() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "default-model: claude-x\n");
        let p = write_yaml(&dir, "project.yaml", "theme: light\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("claude-x"));
        assert_eq!(s.theme(), ThemeSetting::Light);
    }

    #[test]
    fn sub_struct_field_merging() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(
            &dir,
            "global.yaml",
            "compaction:\n  threshold: 0.7\n",
        );
        let p = write_yaml(
            &dir,
            "project.yaml",
            "compaction:\n  enabled: false\n",
        );
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        // Project supplied `enabled: false` — wins.
        assert!(!s.compaction.enabled());
        // Global supplied `threshold: 0.7` — survives because project did
        // not override it.
        assert!((s.compaction.threshold() - 0.7).abs() < f32::EPSILON);
        // Untouched sub-fields still default.
        assert_eq!(s.compaction.keep_recent_tokens(), 32_000);
    }

    #[test]
    fn unknown_top_level_key_ignored_without_error() {
        let dir = TempDir::new().unwrap();
        let p = write_yaml(
            &dir,
            "settings.yaml",
            "unknown-key: foo\ndefault-model: gpt-4o\n",
        );
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn malformed_yaml_returns_yaml_error_with_path() {
        let dir = TempDir::new().unwrap();
        let p = write_yaml(&dir, "bad.yaml", "default-model: [unclosed\n");
        let err = Settings::load(Some(&p), None).unwrap_err();
        match err {
            SettingsError::Yaml { path, .. } => assert_eq!(path, p),
            other => panic!("expected Yaml error, got {other:?}"),
        }
    }

    #[test]
    fn missing_global_file_is_ok() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("nonexistent.yaml");
        let s = Settings::load(Some(&nonexistent), None).unwrap();
        assert_eq!(s, Settings::defaults());
    }

    #[test]
    fn missing_project_file_is_ok() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "default-model: claude-x\n");
        let nonexistent = dir.path().join("nonexistent.yaml");
        let s = Settings::load(Some(&g), Some(&nonexistent)).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("claude-x"));
    }

    #[test]
    fn compaction_settings_accessor_returns_usable_shape() {
        let mgr = SettingsManager::in_memory();
        let cs = mgr.compaction_settings();
        // Existing callers use field access on the returned struct via
        // accessor methods — this test pins the API.
        assert!(cs.enabled());
        assert_eq!(cs.keep_recent_tokens(), 32_000);
        assert_eq!(cs.max_context_tokens(), 200_000);
    }

    #[test]
    fn from_cwd_resolves_standard_paths() {
        let dir = TempDir::new().unwrap();
        // No on-disk files needed: from_cwd should still succeed and use
        // defaults. Verify the resolved project path points at the expected
        // location under `<cwd>/.hand/`.
        let mgr = SettingsManager::from_cwd(dir.path()).unwrap();
        let project = mgr.project_path().expect("project path resolved");
        assert!(project.ends_with(".hand/settings.yaml"));
        assert!(project.starts_with(dir.path()));
        // Global path resolution depends on `dirs::home_dir()` — assert it
        // ends with the standard subpath when present.
        if let Some(global) = mgr.global_path() {
            assert!(global.ends_with(".hand/agent/settings.yaml"));
        }
    }

    #[test]
    fn pure_merge_does_not_touch_disk() {
        // Sanity: `Settings::merge` is a pure function over two values.
        let g = Settings {
            default_model: Some("a".into()),
            ..Settings::default()
        };
        let p = Settings {
            default_model: Some("b".into()),
            ..Settings::default()
        };
        let merged = Settings::merge(g.clone(), p.clone());
        assert_eq!(merged.default_model.as_deref(), Some("b"));
        // Inputs are unchanged.
        assert_eq!(g.default_model.as_deref(), Some("a"));
        assert_eq!(p.default_model.as_deref(), Some("b"));
    }
}
