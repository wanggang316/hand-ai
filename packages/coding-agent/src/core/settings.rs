//! Settings management — global + project settings with deep merge.

use crate::core::error::CodingAgentError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Application settings (merged from global + project).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_command_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_startup: Option<bool>,
}

/// Compaction settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u32,
    pub keep_recent_tokens: u32,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        }
    }
}

/// Retry settings for API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30000,
        }
    }
}

/// Manages loading and saving of settings from global and project files.
pub struct SettingsManager {
    global_path: PathBuf,
    project_path: Option<PathBuf>,
    merged: Settings,
}

impl SettingsManager {
    /// Create a new settings manager.
    pub fn new(cwd: &Path) -> Self {
        let global_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".hand")
            .join("agent")
            .join("settings.json");

        let project_path = Some(cwd.join(".hand").join("settings.json"));

        let mut mgr = Self {
            global_path,
            project_path,
            merged: Settings::default(),
        };
        mgr.reload();
        mgr
    }

    /// Create with in-memory defaults (no file I/O).
    pub fn in_memory() -> Self {
        Self {
            global_path: PathBuf::new(),
            project_path: None,
            merged: Settings::default(),
        }
    }

    /// Reload settings from disk.
    pub fn reload(&mut self) {
        let global = Self::load_file(&self.global_path);
        let project = self
            .project_path
            .as_ref()
            .map(|p| Self::load_file(p))
            .unwrap_or_default();

        self.merged = Self::merge(global, project);
    }

    /// Get the merged settings.
    pub fn settings(&self) -> &Settings {
        &self.merged
    }

    /// Get compaction settings with defaults.
    pub fn compaction_settings(&self) -> CompactionSettings {
        self.merged.compaction.clone().unwrap_or_default()
    }

    /// Get retry settings with defaults.
    pub fn retry_settings(&self) -> RetrySettings {
        self.merged.retry.clone().unwrap_or_default()
    }

    /// Get shell path.
    pub fn shell_path(&self) -> &str {
        self.merged.shell_path.as_deref().unwrap_or("/bin/bash")
    }

    /// Save a setting to the global file.
    pub fn set_global(&mut self, key: &str, value: &str) -> Result<(), CodingAgentError> {
        let mut global = Self::load_file(&self.global_path);

        match key {
            "default_provider" => global.default_provider = Some(value.to_string()),
            "default_model" => global.default_model = Some(value.to_string()),
            "theme" => global.theme = Some(value.to_string()),
            "shell_path" => global.shell_path = Some(value.to_string()),
            _ => {
                return Err(CodingAgentError::Settings(format!(
                    "Unknown setting: {}",
                    key
                )));
            }
        }

        Self::save_file(&self.global_path, &global)?;
        self.reload();
        Ok(())
    }

    fn load_file(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn save_file(path: &Path, settings: &Settings) -> Result<(), CodingAgentError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(settings)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Merge global and project settings (project overrides global).
    fn merge(global: Settings, project: Settings) -> Settings {
        Settings {
            default_provider: project.default_provider.or(global.default_provider),
            default_model: project.default_model.or(global.default_model),
            default_thinking_level: project
                .default_thinking_level
                .or(global.default_thinking_level),
            shell_path: project.shell_path.or(global.shell_path),
            shell_command_prefix: project.shell_command_prefix.or(global.shell_command_prefix),
            theme: project.theme.or(global.theme),
            compaction: project.compaction.or(global.compaction),
            retry: project.retry.or(global.retry),
            quiet_startup: project.quiet_startup.or(global.quiet_startup),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_default() {
        let settings = Settings::default();
        assert!(settings.default_provider.is_none());
        assert!(settings.default_model.is_none());
    }

    #[test]
    fn test_compaction_settings_default() {
        let cs = CompactionSettings::default();
        assert!(cs.enabled);
        assert_eq!(cs.reserve_tokens, 16384);
    }

    #[test]
    fn test_retry_settings_default() {
        let rs = RetrySettings::default();
        assert!(rs.enabled);
        assert_eq!(rs.max_retries, 3);
    }

    #[test]
    fn test_settings_merge() {
        let global = Settings {
            default_provider: Some("openai".into()),
            default_model: Some("gpt-4".into()),
            ..Default::default()
        };
        let project = Settings {
            default_model: Some("claude-3".into()),
            theme: Some("dark".into()),
            ..Default::default()
        };
        let merged = SettingsManager::merge(global, project);
        assert_eq!(merged.default_provider.as_deref(), Some("openai"));
        assert_eq!(merged.default_model.as_deref(), Some("claude-3")); // Project wins
        assert_eq!(merged.theme.as_deref(), Some("dark"));
    }

    #[test]
    fn test_settings_in_memory() {
        let mgr = SettingsManager::in_memory();
        assert!(mgr.settings().default_provider.is_none());
        assert_eq!(mgr.shell_path(), "/bin/bash");
    }

    #[test]
    fn test_settings_serialization() {
        let settings = Settings {
            default_provider: Some("anthropic".into()),
            compaction: Some(CompactionSettings::default()),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.default_provider.as_deref(), Some("anthropic"));
    }
}
