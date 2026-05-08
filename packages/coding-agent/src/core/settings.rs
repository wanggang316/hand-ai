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
use std::time::Duration;
use thiserror::Error;
use tokio::sync::broadcast;

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
    /// Most recent changelog version the agent has shown to the user.
    /// Global-only in pi-mono; we keep it on the same struct because every
    /// field is `Option<T>` and the project layer simply leaves it `None`.
    #[serde(alias = "lastChangelogVersion")]
    pub last_changelog_version: Option<String>,
    #[serde(alias = "defaultProvider")]
    pub default_provider: Option<String>,
    #[serde(alias = "defaultModel")]
    pub default_model: Option<String>,
    #[serde(alias = "defaultThinkingLevel")]
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    /// Streaming transport mode. Mirrors pi-mono's `transport`. Default
    /// when unset is [`TransportSetting::Auto`] — read via the accessor on
    /// [`Settings`] rather than the raw field.
    pub transport: Option<TransportSetting>,
    /// Surface mode for the steering queue (`all` vs `one-at-a-time`).
    #[serde(alias = "steeringMode")]
    pub steering_mode: Option<SteeringMode>,
    /// Surface mode for follow-up queue. Same shape as
    /// [`Self::steering_mode`].
    #[serde(alias = "followUpMode")]
    pub follow_up_mode: Option<FollowUpMode>,
    #[serde(alias = "shellPath")]
    pub shell_path: Option<PathBuf>,
    #[serde(alias = "shellCommandPrefix")]
    pub shell_command_prefix: Option<String>,
    pub theme: Option<ThemeSetting>,
    pub compaction: CompactionSettings,
    /// Branch summarisation knobs.
    #[serde(default, alias = "branchSummary")]
    pub branch_summary: BranchSummarySettings,
    pub retry: RetrySettings,
    /// Suppress thinking blocks in the rendered transcript.
    #[serde(alias = "hideThinkingBlock")]
    pub hide_thinking_block: Option<bool>,
    #[serde(alias = "quietStartup")]
    pub quiet_startup: Option<bool>,
    /// argv-style command used for npm package lookup/install.
    #[serde(alias = "npmCommand", skip_serializing_if = "Option::is_none")]
    pub npm_command: Option<Vec<String>>,
    /// Show condensed changelog after an update (full via `/changelog`).
    #[serde(alias = "collapseChangelog")]
    pub collapse_changelog: Option<bool>,
    /// Anonymous install-telemetry attribution headers on outbound provider
    /// calls. Default: `true` (matches the TS reference). Read effective
    /// value via [`Settings::enable_install_telemetry`] — direct field
    /// access is `Option<bool>` so layered merging is mechanical.
    #[serde(alias = "enableInstallTelemetry")]
    pub enable_install_telemetry: Option<bool>,
    /// Pi-extension package sources (npm specs, git URLs, or local paths)
    /// to load extensions/skills/prompts/themes from. Each entry is either
    /// a bare string source or a [`PackageSource::Filtered`] object that
    /// scopes which resource kinds to enable from the package.
    ///
    /// Merge semantics: project layer **replaces** the global list when
    /// supplied (whole-list override, matching the TS reference's
    /// `deepMergeSettings` array behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<PackageSource>>,
    /// Local filesystem paths (files or directories) to load extension
    /// modules from, in addition to those discovered via [`Self::packages`].
    /// Whole-list override on merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    /// Local filesystem paths (files or directories) to load skill markdown
    /// files from. Whole-list override on merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Local filesystem paths (files or directories) to load prompt
    /// templates from. Whole-list override on merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    /// Local filesystem paths (files or directories) to load theme
    /// JSON files from. Whole-list override on merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    /// Whether to register skills as `/skill:name` slash commands.
    #[serde(alias = "enableSkillCommands")]
    pub enable_skill_commands: Option<bool>,
    /// Terminal rendering preferences.
    #[serde(default)]
    pub terminal: TerminalSettings,
    /// Image handling preferences.
    #[serde(default)]
    pub images: ImageSettings,
    /// Patterns for the `--models` cycling flag. Whole-list override on merge.
    #[serde(alias = "enabledModels", skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<Vec<String>>,
    /// Action taken when the user double-presses Escape on an empty editor.
    #[serde(alias = "doubleEscapeAction")]
    pub double_escape_action: Option<DoubleEscapeAction>,
    /// Default filter applied when opening the tree view.
    #[serde(alias = "treeFilterMode")]
    pub tree_filter_mode: Option<TreeFilterMode>,
    /// Custom token budgets per thinking level.
    #[serde(default, alias = "thinkingBudgets")]
    pub thinking_budgets: ThinkingBudgetsSettings,
    /// Horizontal padding for the input editor.
    #[serde(alias = "editorPaddingX")]
    pub editor_padding_x: Option<u32>,
    /// Maximum visible items in the autocomplete dropdown.
    #[serde(alias = "autocompleteMaxVisible")]
    pub autocomplete_max_visible: Option<u32>,
    /// Show the terminal hardware cursor while still positioning it for IME.
    #[serde(alias = "showHardwareCursor")]
    pub show_hardware_cursor: Option<bool>,
    /// Markdown rendering preferences.
    #[serde(default)]
    pub markdown: MarkdownSettings,
    /// Soft warnings the UI surfaces.
    #[serde(default)]
    pub warnings: WarningSettings,
    /// Custom session-storage directory (same format as `--session-dir`).
    #[serde(alias = "sessionDir")]
    pub session_dir: Option<PathBuf>,
}

/// One entry in [`Settings::packages`].
///
/// Mirrors the TS `PackageSource` union: either a bare source string
/// (`"npm:..."`, `"git:..."`, `"github:..."`, `"./local/path"`) that loads
/// every resource kind the package exposes, or a filtered object that
/// scopes the loaded resource kinds.
///
/// Serde shape: untagged. A YAML scalar deserializes as
/// [`PackageSource::Bare`]; a mapping with at least a `source:` key
/// deserializes as [`PackageSource::Filtered`].
///
/// ```yaml
/// packages:
///   - npm:@scope/pkg                # Bare
///   - source: github:owner/repo     # Filtered
///     extensions: ["ext-a"]
///     skills: ["skill-x"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PackageSource {
    /// Bare source spec — load everything the package exposes.
    Bare(String),
    /// Source spec plus per-kind allow-lists. An absent list means "load
    /// every resource of that kind"; an empty list means "load none".
    Filtered {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extensions: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompts: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        themes: Option<Vec<String>>,
    },
}

impl PackageSource {
    /// Source spec as a string slice, regardless of variant.
    pub fn source(&self) -> &str {
        match self {
            PackageSource::Bare(s) => s,
            PackageSource::Filtered { source, .. } => source,
        }
    }
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
    #[serde(alias = "enabled")]
    pub enabled: Option<bool>,
    #[serde(alias = "maxRetries")]
    pub max_retries: Option<u32>,
    /// Base delay for exponential backoff. Aliased to the pi-mono TS field
    /// `baseDelayMs` so JSON-import round-trips cleanly.
    #[serde(alias = "initialDelayMs", alias = "baseDelayMs")]
    pub initial_delay_ms: Option<u32>,
    #[serde(alias = "maxDelayMs")]
    pub max_delay_ms: Option<u32>,
    /// Provider-level retry knobs (SDK request timeout, max retries, max
    /// retry-after delay). Mirrors pi-mono's `RetrySettings.provider`.
    #[serde(default)]
    pub provider: ProviderRetrySettings,
}

impl RetrySettings {
    /// Build the populated default form (all `Some`).
    pub fn with_defaults() -> Self {
        Self {
            enabled: Some(true),
            max_retries: Some(3),
            initial_delay_ms: Some(1_000),
            max_delay_ms: Some(30_000),
            provider: ProviderRetrySettings::default(),
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
            provider: ProviderRetrySettings::merge(base.provider, project.provider),
        }
    }
}

/// Provider-level retry tuning. Mirrors pi-mono's `ProviderRetrySettings`:
/// SDK request timeout, max retry attempts, and the cap on a server-requested
/// `retry-after` delay before we give up.
///
/// Read effective values via the accessor methods — direct field access
/// yields `Option<T>` because the struct represents the raw YAML layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ProviderRetrySettings {
    #[serde(alias = "timeoutMs")]
    pub timeout_ms: Option<u32>,
    #[serde(alias = "maxRetries")]
    pub max_retries: Option<u32>,
    #[serde(alias = "maxRetryDelayMs")]
    pub max_retry_delay_ms: Option<u32>,
}

impl ProviderRetrySettings {
    /// Default cap on server-requested retry delay before giving up.
    /// Matches the TS reference (`60000`).
    pub fn max_retry_delay_ms(&self) -> u32 {
        self.max_retry_delay_ms.unwrap_or(60_000)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            timeout_ms: project.timeout_ms.or(base.timeout_ms),
            max_retries: project.max_retries.or(base.max_retries),
            max_retry_delay_ms: project.max_retry_delay_ms.or(base.max_retry_delay_ms),
        }
    }
}

/// Branch summarisation prompt knobs. Mirrors pi-mono `BranchSummarySettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct BranchSummarySettings {
    #[serde(alias = "reserveTokens")]
    pub reserve_tokens: Option<u32>,
    #[serde(alias = "skipPrompt")]
    pub skip_prompt: Option<bool>,
}

impl BranchSummarySettings {
    /// Tokens reserved for the summarisation prompt + LLM response.
    /// Matches the TS reference default (`16384`).
    pub fn reserve_tokens(&self) -> u32 {
        self.reserve_tokens.unwrap_or(16_384)
    }

    /// When `true`, the "Summarize branch?" prompt is skipped and no
    /// summary is produced. Default: `false`.
    pub fn skip_prompt(&self) -> bool {
        self.skip_prompt.unwrap_or(false)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            reserve_tokens: project.reserve_tokens.or(base.reserve_tokens),
            skip_prompt: project.skip_prompt.or(base.skip_prompt),
        }
    }
}

/// Terminal rendering preferences. Mirrors pi-mono `TerminalSettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct TerminalSettings {
    #[serde(alias = "showImages")]
    pub show_images: Option<bool>,
    #[serde(alias = "imageWidthCells")]
    pub image_width_cells: Option<u32>,
    #[serde(alias = "clearOnShrink")]
    pub clear_on_shrink: Option<bool>,
    #[serde(alias = "showTerminalProgress")]
    pub show_terminal_progress: Option<bool>,
}

impl TerminalSettings {
    /// Whether inline images should be rendered when the terminal supports
    /// them. Default: `true` (pi-mono parity).
    pub fn show_images(&self) -> bool {
        self.show_images.unwrap_or(true)
    }

    /// Preferred inline image width in terminal cells. Default: `60`.
    pub fn image_width_cells(&self) -> u32 {
        self.image_width_cells.unwrap_or(60)
    }

    /// Clear empty rows when content shrinks. Default: `false`.
    pub fn clear_on_shrink(&self) -> bool {
        self.clear_on_shrink.unwrap_or(false)
    }

    /// Emit OSC 9;4 terminal-progress sequences. Default: `false`.
    pub fn show_terminal_progress(&self) -> bool {
        self.show_terminal_progress.unwrap_or(false)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            show_images: project.show_images.or(base.show_images),
            image_width_cells: project.image_width_cells.or(base.image_width_cells),
            clear_on_shrink: project.clear_on_shrink.or(base.clear_on_shrink),
            show_terminal_progress: project
                .show_terminal_progress
                .or(base.show_terminal_progress),
        }
    }
}

/// Image handling. Mirrors pi-mono `ImageSettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ImageSettings {
    #[serde(alias = "autoResize")]
    pub auto_resize: Option<bool>,
    #[serde(alias = "blockImages")]
    pub block_images: Option<bool>,
}

impl ImageSettings {
    /// Resize images to the model-friendly cap before sending. Default: `true`.
    pub fn auto_resize(&self) -> bool {
        self.auto_resize.unwrap_or(true)
    }

    /// When `true`, no images are forwarded to the LLM. Default: `false`.
    pub fn block_images(&self) -> bool {
        self.block_images.unwrap_or(false)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            auto_resize: project.auto_resize.or(base.auto_resize),
            block_images: project.block_images.or(base.block_images),
        }
    }
}

/// Custom token budgets per thinking level. Mirrors pi-mono
/// `ThinkingBudgetsSettings` — every field is optional so missing entries
/// fall back to the provider default for that level.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ThinkingBudgetsSettings {
    pub minimal: Option<u32>,
    pub low: Option<u32>,
    pub medium: Option<u32>,
    pub high: Option<u32>,
}

impl ThinkingBudgetsSettings {
    fn merge(base: Self, project: Self) -> Self {
        Self {
            minimal: project.minimal.or(base.minimal),
            low: project.low.or(base.low),
            medium: project.medium.or(base.medium),
            high: project.high.or(base.high),
        }
    }
}

/// Markdown rendering preferences. Mirrors pi-mono `MarkdownSettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct MarkdownSettings {
    #[serde(alias = "codeBlockIndent")]
    pub code_block_indent: Option<String>,
}

impl MarkdownSettings {
    /// Indent prefix for rendered code blocks. Default: two spaces.
    pub fn code_block_indent(&self) -> &str {
        self.code_block_indent.as_deref().unwrap_or("  ")
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            code_block_indent: project.code_block_indent.or(base.code_block_indent),
        }
    }
}

/// Soft warnings the UI surfaces. Mirrors pi-mono `WarningSettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", default)]
pub struct WarningSettings {
    #[serde(alias = "anthropicExtraUsage")]
    pub anthropic_extra_usage: Option<bool>,
}

impl WarningSettings {
    /// Whether to warn about Anthropic extra-usage costs. Default: `true`.
    pub fn anthropic_extra_usage(&self) -> bool {
        self.anthropic_extra_usage.unwrap_or(true)
    }

    fn merge(base: Self, project: Self) -> Self {
        Self {
            anthropic_extra_usage: project
                .anthropic_extra_usage
                .or(base.anthropic_extra_usage),
        }
    }
}

/// Re-export of [`model::Transport`] under the pi-mono name `TransportSetting`.
/// Settings YAML stores the transport selection as a kebab-case string
/// (`auto`, `sse`, `websocket`, `websocket-cached`).
pub type TransportSetting = model::Transport;

/// Action taken when the user double-presses Escape on an empty editor.
/// Mirrors pi-mono's `doubleEscapeAction`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DoubleEscapeAction {
    Fork,
    #[default]
    Tree,
    None,
}

/// Default filter applied when opening the conversation tree view.
/// Mirrors pi-mono's `treeFilterMode`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TreeFilterMode {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

/// How the steering / follow-up queue surfaces pending items. Mirrors
/// pi-mono's `steeringMode` / `followUpMode` (same value space).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    All,
    #[default]
    OneAtATime,
}

/// Alias for `SteeringMode` — pi-mono uses identical semantics for the
/// follow-up mode setting, so we share the type rather than duplicating it.
pub type FollowUpMode = SteeringMode;

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

/// Which on-disk settings layer a write targets.
///
/// Read-side resolution still prefers project over global; this enum only
/// matters for [`SettingsManager::set_packages`] et al. and the matching
/// [`SettingsManager::save`] call that persists the in-memory state for
/// one layer back to YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    /// User-global layer at `~/.hand/agent/settings.yaml`.
    Global,
    /// Project-local layer at `<cwd>/.hand/settings.yaml`.
    Project,
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
    /// YAML serialisation failure when writing a layer.
    #[error("YAML emit error for {path}: {source}", path = .path.display())]
    YamlEmit {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    /// `save` was called for a scope that has no configured on-disk path.
    #[error("no path configured for {scope:?} settings layer")]
    NoPath { scope: SettingsScope },
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
            last_changelog_version: None,
            default_provider: Some("anthropic".into()),
            default_model: Some("claude-sonnet-4-20250514".into()),
            default_thinking_level: None,
            transport: None,
            steering_mode: None,
            follow_up_mode: None,
            shell_path: None,
            shell_command_prefix: None,
            theme: Some(ThemeSetting::Dark),
            compaction: CompactionSettings::with_defaults(),
            branch_summary: BranchSummarySettings::default(),
            retry: RetrySettings::with_defaults(),
            hide_thinking_block: None,
            quiet_startup: Some(false),
            npm_command: None,
            collapse_changelog: None,
            enable_install_telemetry: Some(true),
            // Resource-side fields default to "not configured" (None) — the
            // accessors below return empty slices in that case. Distinct
            // from `Some(vec![])` which is "explicitly an empty list".
            packages: None,
            extensions: None,
            skills: None,
            prompts: None,
            themes: None,
            enable_skill_commands: None,
            terminal: TerminalSettings::default(),
            images: ImageSettings::default(),
            enabled_models: None,
            double_escape_action: None,
            tree_filter_mode: None,
            thinking_budgets: ThinkingBudgetsSettings::default(),
            editor_padding_x: None,
            autocomplete_max_visible: None,
            show_hardware_cursor: None,
            markdown: MarkdownSettings::default(),
            warnings: WarningSettings::default(),
            session_dir: None,
        }
    }

    /// Effective list of pi-extension package sources. Empty when the
    /// field is unset in both layers.
    pub fn packages(&self) -> &[PackageSource] {
        self.packages.as_deref().unwrap_or(&[])
    }

    /// Effective list of extension paths.
    pub fn extensions(&self) -> &[String] {
        self.extensions.as_deref().unwrap_or(&[])
    }

    /// Effective list of skill paths.
    pub fn skills(&self) -> &[String] {
        self.skills.as_deref().unwrap_or(&[])
    }

    /// Effective list of prompt template paths.
    pub fn prompts(&self) -> &[String] {
        self.prompts.as_deref().unwrap_or(&[])
    }

    /// Effective list of theme paths.
    pub fn themes(&self) -> &[String] {
        self.themes.as_deref().unwrap_or(&[])
    }

    /// Effective theme — the merged value or [`ThemeSetting::Dark`] if unset.
    pub fn theme(&self) -> ThemeSetting {
        self.theme.unwrap_or_default()
    }

    /// Effective `quiet_startup` flag — defaults to `false` if unset.
    pub fn quiet_startup(&self) -> bool {
        self.quiet_startup.unwrap_or(false)
    }

    /// Effective `enable-install-telemetry` flag — defaults to `true` if
    /// unset, matching the TS reference (`enableInstallTelemetry ?? true`).
    pub fn enable_install_telemetry(&self) -> bool {
        self.enable_install_telemetry.unwrap_or(true)
    }

    /// Merge `project` on top of `base` (typically the global layer or
    /// [`Settings::default`]). Pure: produces a new `Settings`, mutates
    /// nothing. Field-by-field — for `Option<T>` fields a `Some` in
    /// `project` wins; sub-structs merge recursively per the same rule.
    pub fn merge(base: Self, project: Self) -> Self {
        Self {
            last_changelog_version: project
                .last_changelog_version
                .or(base.last_changelog_version),
            default_provider: project.default_provider.or(base.default_provider),
            default_model: project.default_model.or(base.default_model),
            default_thinking_level: project
                .default_thinking_level
                .or(base.default_thinking_level),
            transport: project.transport.or(base.transport),
            steering_mode: project.steering_mode.or(base.steering_mode),
            follow_up_mode: project.follow_up_mode.or(base.follow_up_mode),
            shell_path: project.shell_path.or(base.shell_path),
            shell_command_prefix: project.shell_command_prefix.or(base.shell_command_prefix),
            theme: project.theme.or(base.theme),
            compaction: CompactionSettings::merge(base.compaction, project.compaction),
            branch_summary: BranchSummarySettings::merge(
                base.branch_summary,
                project.branch_summary,
            ),
            retry: RetrySettings::merge(base.retry, project.retry),
            hide_thinking_block: project.hide_thinking_block.or(base.hide_thinking_block),
            quiet_startup: project.quiet_startup.or(base.quiet_startup),
            // Whole-list override (matches arrays-not-deep-merged TS rule).
            npm_command: project.npm_command.or(base.npm_command),
            collapse_changelog: project.collapse_changelog.or(base.collapse_changelog),
            enable_install_telemetry: project
                .enable_install_telemetry
                .or(base.enable_install_telemetry),
            // Whole-list override: project's `Some(...)` (even an empty
            // vec) replaces base. This matches the TS reference's
            // `deepMergeSettings`, where arrays are not deep-merged.
            packages: project.packages.or(base.packages),
            extensions: project.extensions.or(base.extensions),
            skills: project.skills.or(base.skills),
            prompts: project.prompts.or(base.prompts),
            themes: project.themes.or(base.themes),
            enable_skill_commands: project
                .enable_skill_commands
                .or(base.enable_skill_commands),
            terminal: TerminalSettings::merge(base.terminal, project.terminal),
            images: ImageSettings::merge(base.images, project.images),
            enabled_models: project.enabled_models.or(base.enabled_models),
            double_escape_action: project
                .double_escape_action
                .or(base.double_escape_action),
            tree_filter_mode: project.tree_filter_mode.or(base.tree_filter_mode),
            thinking_budgets: ThinkingBudgetsSettings::merge(
                base.thinking_budgets,
                project.thinking_budgets,
            ),
            editor_padding_x: project.editor_padding_x.or(base.editor_padding_x),
            autocomplete_max_visible: project
                .autocomplete_max_visible
                .or(base.autocomplete_max_visible),
            show_hardware_cursor: project
                .show_hardware_cursor
                .or(base.show_hardware_cursor),
            markdown: MarkdownSettings::merge(base.markdown, project.markdown),
            warnings: WarningSettings::merge(base.warnings, project.warnings),
            session_dir: project.session_dir.or(base.session_dir),
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
        let (_, _, merged) = Self::load_layers(global_path, project_path)?;
        Ok(merged)
    }

    /// Load each YAML layer separately and return `(global, project, merged)`.
    ///
    /// `global` and `project` are the raw layer values (every field that
    /// was not specified in the YAML is `None`); `merged` is the fully
    /// resolved view including defaults. Useful when callers need to
    /// mutate one layer in isolation — see
    /// [`SettingsManager::global_layer`] / [`SettingsManager::project_layer`].
    pub fn load_layers(
        global_path: Option<&Path>,
        project_path: Option<&Path>,
    ) -> Result<(Self, Self, Self), SettingsError> {
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
        let with_global = Settings::merge(Settings::defaults(), global.clone());
        let merged = Settings::merge(with_global, project.clone());
        Ok((global, project, merged))
    }
}

/// Persist `layer` to `path` as YAML using a tmp-file + rename so a torn
/// write leaves the original untouched. Creates the parent directory if
/// needed. On Unix the post-rename mode is forced to `0o600`.
///
/// On Windows mode-handling is a no-op — the file is created with the
/// process-default ACL.
fn write_yaml_layer_atomic(path: &Path, layer: &Settings) -> Result<(), SettingsError> {
    use std::io::Write as _;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let body = serde_yaml::to_string(layer).map_err(|source| SettingsError::YamlEmit {
        path: path.to_path_buf(),
        source,
    })?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| SettingsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    tmp.write_all(body.as_bytes())
        .map_err(|source| SettingsError::Io {
            path: tmp.path().to_path_buf(),
            source,
        })?;
    tmp.as_file()
        .sync_all()
        .map_err(|source| SettingsError::Io {
            path: tmp.path().to_path_buf(),
            source,
        })?;
    tmp.persist(path).map_err(|e| SettingsError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;

    // Reassert 0o600 on Unix. The tempfile is created with the process
    // umask which is commonly 0644; tighten post-rename so the visible
    // file is never world-readable. No-op on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|source| SettingsError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    Ok(())
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
            "last-changelog-version",
            "default-provider",
            "default-model",
            "default-thinking-level",
            "transport",
            "steering-mode",
            "follow-up-mode",
            "shell-path",
            "shell-command-prefix",
            "theme",
            "compaction",
            "branch-summary",
            "retry",
            "hide-thinking-block",
            "quiet-startup",
            "npm-command",
            "collapse-changelog",
            "enable-install-telemetry",
            "packages",
            "extensions",
            "skills",
            "prompts",
            "themes",
            "enable-skill-commands",
            "terminal",
            "images",
            "enabled-models",
            "double-escape-action",
            "tree-filter-mode",
            "thinking-budgets",
            "editor-padding-x",
            "autocomplete-max-visible",
            "show-hardware-cursor",
            "markdown",
            "warnings",
            "session-dir",
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

/// Event broadcast on settings change. Holds both the previous and the
/// freshly-loaded settings so subscribers can diff fields they care about.
#[derive(Debug, Clone)]
pub struct SettingsChanged {
    pub previous: Settings,
    pub current: Settings,
}

/// Internal handle owned by [`SettingsManager`] when a watcher is active.
/// Holding the `RecommendedWatcher` keeps the OS-level subscription alive;
/// holding the `JoinHandle` keeps the debounce/reload task alive — dropping
/// the handle aborts the task (tokio detaches and reaps it).
struct WatchHandle {
    sender: broadcast::Sender<SettingsChanged>,
    _watcher: notify::RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Best-effort: ensure the task wakes up and observes the dropped
        // notify channel quickly. Aborting is idempotent.
        self.task.abort();
    }
}

/// Manages loading settings from the standard hand layout.
///
/// Holds the merged [`Settings`] together with the resolved layer paths.
/// The two source layers (`global_layer`, `project_layer`) are kept
/// separately so that scope-targeted writes (see
/// [`SettingsManager::set_packages`]) can update one without affecting the
/// other; the merged view is recomputed on every mutation.
///
/// Hot-reload: call [`SettingsManager::watch`] to receive a
/// [`broadcast::Receiver<SettingsChanged>`] that fires whenever the YAML
/// files on disk change to a value different from the in-memory copy. The
/// watcher is started lazily on first call and shared across subsequent
/// callers.
pub struct SettingsManager {
    /// Merged view: `defaults < global < project`. Recomputed by
    /// [`SettingsManager::recompute_merged`] after any layer-targeted
    /// write so existing read accessors keep working unchanged.
    settings: Settings,
    /// Raw global-layer values (everything not specified is `None`).
    global_layer: Settings,
    /// Raw project-layer values (everything not specified is `None`).
    project_layer: Settings,
    project_path: Option<PathBuf>,
    global_path: Option<PathBuf>,
    watch_handle: Option<WatchHandle>,
}

impl SettingsManager {
    /// Construct from the standard hand paths:
    /// - global: `~/.hand/agent/settings.yaml`
    /// - project: `<cwd>/.hand/settings.yaml`
    ///
    /// Either layer being absent is fine — defaults are used. YAML parse
    /// errors propagate.
    ///
    /// Before loading, attempts a one-shot migration of any legacy
    /// `settings.json` next to either expected YAML location. See
    /// [`migrate_legacy_json_settings`].
    pub fn from_cwd(cwd: &Path) -> Result<Self, SettingsError> {
        let global_path = dirs::home_dir().map(|h| h.join(".hand/agent/settings.yaml"));
        let project_path = Some(cwd.join(".hand/settings.yaml"));

        // Best-effort migration of legacy JSON settings, BEFORE load. The
        // base directory for each layer is the parent of its YAML path.
        if let Some(p) = global_path.as_ref().and_then(|p| p.parent()) {
            let _ = migrate_legacy_json_settings(p);
        }
        if let Some(p) = project_path.as_ref().and_then(|p| p.parent()) {
            let _ = migrate_legacy_json_settings(p);
        }

        let (global_layer, project_layer, settings) =
            Settings::load_layers(global_path.as_deref(), project_path.as_deref())?;
        Ok(Self {
            settings,
            global_layer,
            project_layer,
            project_path,
            global_path,
            watch_handle: None,
        })
    }

    /// Construct with in-memory defaults — no disk I/O. Intended for tests.
    pub fn in_memory() -> Self {
        Self {
            settings: Settings::defaults(),
            global_layer: Settings::default(),
            project_layer: Settings::default(),
            project_path: None,
            global_path: None,
            watch_handle: None,
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

    /// Borrow the raw global-layer values. Every field that was not
    /// explicitly set in the global YAML is `None`. Distinct from
    /// [`Self::current`], which folds in the project layer and defaults.
    pub fn global_layer(&self) -> &Settings {
        &self.global_layer
    }

    /// Borrow the raw project-layer values. Every field that was not
    /// explicitly set in the project YAML is `None`. Distinct from
    /// [`Self::current`], which folds in the global layer and defaults.
    pub fn project_layer(&self) -> &Settings {
        &self.project_layer
    }

    /// Borrow the layer matching `scope`.
    pub fn layer(&self, scope: SettingsScope) -> &Settings {
        match scope {
            SettingsScope::Global => &self.global_layer,
            SettingsScope::Project => &self.project_layer,
        }
    }

    fn layer_mut(&mut self, scope: SettingsScope) -> &mut Settings {
        match scope {
            SettingsScope::Global => &mut self.global_layer,
            SettingsScope::Project => &mut self.project_layer,
        }
    }

    /// Replace the `packages` list of one layer in memory. Persist with
    /// [`Self::save`] for the same `scope`. Passing `None` clears the
    /// field (the layer will not contribute a `packages` value to the
    /// merge); passing `Some(vec![])` is "explicitly empty" and shadows
    /// the lower layer's list.
    pub fn set_packages(&mut self, scope: SettingsScope, value: Option<Vec<PackageSource>>) {
        self.layer_mut(scope).packages = value;
        self.recompute_merged();
    }

    /// Replace the `extensions` list of one layer in memory. Same
    /// semantics as [`Self::set_packages`].
    pub fn set_extensions(&mut self, scope: SettingsScope, value: Option<Vec<String>>) {
        self.layer_mut(scope).extensions = value;
        self.recompute_merged();
    }

    /// Replace the `skills` list of one layer in memory. Same semantics
    /// as [`Self::set_packages`].
    pub fn set_skills(&mut self, scope: SettingsScope, value: Option<Vec<String>>) {
        self.layer_mut(scope).skills = value;
        self.recompute_merged();
    }

    /// Replace the `prompts` list of one layer in memory. Same semantics
    /// as [`Self::set_packages`].
    pub fn set_prompts(&mut self, scope: SettingsScope, value: Option<Vec<String>>) {
        self.layer_mut(scope).prompts = value;
        self.recompute_merged();
    }

    /// Replace the `themes` list of one layer in memory. Same semantics
    /// as [`Self::set_packages`].
    pub fn set_themes(&mut self, scope: SettingsScope, value: Option<Vec<String>>) {
        self.layer_mut(scope).themes = value;
        self.recompute_merged();
    }

    /// Persist the in-memory state of `scope` to its YAML path.
    ///
    /// Atomic write semantics: the new content is staged to a sibling
    /// `tempfile::NamedTempFile`, fsynced, then renamed into place. A
    /// torn write therefore either leaves the existing file untouched or
    /// produces the new file in full — never a partial overwrite. On
    /// Unix the post-rename file mode is forced to `0o600`. On Windows
    /// mode handling is a no-op.
    ///
    /// Returns [`SettingsError::NoPath`] when the scope has no
    /// configured path (the in-memory-only constructors).
    pub fn save(&self, scope: SettingsScope) -> Result<(), SettingsError> {
        let path = match scope {
            SettingsScope::Global => self.global_path.as_deref(),
            SettingsScope::Project => self.project_path.as_deref(),
        }
        .ok_or(SettingsError::NoPath { scope })?;
        let layer = self.layer(scope);
        write_yaml_layer_atomic(path, layer)
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

    /// Effective `enable-install-telemetry` flag — defaults to `true` if
    /// unset, matching the TS reference.
    pub fn enable_install_telemetry(&self) -> bool {
        self.settings.enable_install_telemetry()
    }

    /// Test-only constructor: build a manager wrapping a pre-merged
    /// [`Settings`] value with no on-disk paths and no watcher. Used by
    /// unit tests that need to inject specific settings without touching
    /// the filesystem. Both layer views mirror the supplied settings —
    /// callers exercising layer-targeted writes should construct via
    /// [`SettingsManager::from_layers_for_test`] instead.
    #[doc(hidden)]
    pub fn from_raw_for_test(settings: Settings) -> Self {
        Self {
            global_layer: settings.clone(),
            project_layer: Settings::default(),
            settings,
            project_path: None,
            global_path: None,
            watch_handle: None,
        }
    }

    /// Test-only constructor: build a manager from explicit per-layer
    /// values plus on-disk paths. Used by tests that need to exercise
    /// scope-targeted writes round-tripping through YAML.
    #[doc(hidden)]
    pub fn from_layers_for_test(
        global_layer: Settings,
        project_layer: Settings,
        global_path: Option<PathBuf>,
        project_path: Option<PathBuf>,
    ) -> Self {
        let with_global = Settings::merge(Settings::defaults(), global_layer.clone());
        let settings = Settings::merge(with_global, project_layer.clone());
        Self {
            settings,
            global_layer,
            project_layer,
            project_path,
            global_path,
            watch_handle: None,
        }
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

    /// Subscribe to settings changes. Returns a [`broadcast::Receiver`] that
    /// yields a [`SettingsChanged`] event each time the disk YAML files
    /// reload to a value different from the current in-memory copy.
    ///
    /// The watcher task is started lazily on the first call; subsequent
    /// calls return additional subscribers attached to the same task.
    /// Multiple subscribers are supported (broadcast semantics).
    ///
    /// Behaviour notes:
    /// - The watcher debounces filesystem events with a sliding 200ms
    ///   window: events arriving within 200ms of each other coalesce
    ///   into a single reload, regardless of total burst duration.
    /// - On transient errors (file mid-rewrite, permission denied,
    ///   malformed YAML), the watcher logs via `tracing::warn!` and keeps
    ///   running — the next valid write will fire as normal.
    /// - The notify subscription targets the *parent directory* of each
    ///   settings file (not the file itself) so editor save-rename patterns
    ///   are observed.
    /// - Channel capacity is 16. Subscribers that lag receive
    ///   `RecvError::Lagged`; that's a documented signal to re-load and
    ///   resync, not a fatal error.
    /// - Must be called from inside a tokio runtime — the internal task is
    ///   spawned via [`tokio::spawn`].
    pub fn watch(&mut self) -> broadcast::Receiver<SettingsChanged> {
        if let Some(handle) = self.watch_handle.as_ref() {
            return handle.sender.subscribe();
        }

        let (sender, _initial_rx) = broadcast::channel::<SettingsChanged>(16);
        let (notify_tx, mut notify_rx) =
            tokio::sync::mpsc::unbounded_channel::<notify::Result<notify::Event>>();

        // Build the notify watcher. The closure runs on a notify-internal
        // thread; we forward each event into the tokio mpsc for the
        // debounce task to consume.
        let watcher_result = <notify::RecommendedWatcher as notify::Watcher>::new(
            move |res| {
                let _ = notify_tx.send(res);
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        );
        let mut watcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(?e, "failed to create settings watcher; hot-reload disabled");
                // Return a receiver to a never-fired channel; callers can
                // hold it without surprise.
                let rx = sender.subscribe();
                drop(sender);
                return rx;
            }
        };

        // Watch the parent directory of each configured layer path. We
        // dedupe by canonical parent so a single dir is registered once.
        let mut watched_parents: Vec<PathBuf> = Vec::new();
        for path in [self.global_path.as_deref(), self.project_path.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = path.parent() {
                // Create the parent if it doesn't exist — notify can't
                // watch a missing directory and the file may legitimately
                // be created later.
                if !parent.exists()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    tracing::warn!(
                        path = %parent.display(),
                        ?e,
                        "could not create settings parent dir for watcher",
                    );
                    continue;
                }
                let parent_buf = parent.to_path_buf();
                if watched_parents.contains(&parent_buf) {
                    continue;
                }
                if let Err(e) = notify::Watcher::watch(
                    &mut watcher,
                    parent,
                    notify::RecursiveMode::NonRecursive,
                ) {
                    tracing::warn!(
                        path = %parent.display(),
                        ?e,
                        "failed to watch settings parent dir",
                    );
                    continue;
                }
                watched_parents.push(parent_buf);
            }
        }

        // Snapshot of inputs the spawned task needs.
        let global_path = self.global_path.clone();
        let project_path = self.project_path.clone();
        let mut current_settings = self.settings.clone();
        let task_sender = sender.clone();
        // Filter events to ones touching one of our settings files. Build
        // both literal and canonicalised forms — FSEvents on macOS reports
        // canonical paths (e.g. `/private/var/...`) while the configured
        // path may be the symlinked alias (e.g. `/var/...`). Including
        // parent dirs lets us catch coarse "directory changed" events too.
        let target_files: Vec<PathBuf> =
            build_match_targets(self.global_path.as_deref(), self.project_path.as_deref());

        let task = tokio::spawn(async move {
            loop {
                // Block until the first relevant event arrives.
                let first = match notify_rx.recv().await {
                    Some(ev) => ev,
                    None => break, // channel closed — watcher dropped
                };
                if !event_matches_targets(&first, &target_files) {
                    continue;
                }

                // Debounce with a sliding 200ms window: each new event
                // resets the idle timer so a continuous burst coalesces
                // into a single reload regardless of total duration.
                const DEBOUNCE: Duration = Duration::from_millis(200);
                loop {
                    match tokio::time::timeout(DEBOUNCE, notify_rx.recv()).await {
                        Ok(Some(_ev)) => continue, // got another event, reset window
                        // Channel closed mid-debounce — process the batch
                        // we have, then exit the outer loop.
                        Ok(None) => break,
                        Err(_) => break, // 200ms idle elapsed
                    }
                }

                // Re-load settings from disk.
                let new_settings =
                    match Settings::load(global_path.as_deref(), project_path.as_deref()) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(?e, "settings reload failed; keeping previous value");
                            continue;
                        }
                    };

                if new_settings != current_settings {
                    let event = SettingsChanged {
                        previous: current_settings.clone(),
                        current: new_settings.clone(),
                    };
                    // Ignore "no subscribers" — that's a benign state.
                    let _ = task_sender.send(event);
                    current_settings = new_settings;
                }
            }
        });

        let receiver = sender.subscribe();
        self.watch_handle = Some(WatchHandle {
            sender,
            _watcher: watcher,
            task,
        });
        receiver
    }

    /// Recompute the merged view after a layer-targeted mutation.
    fn recompute_merged(&mut self) {
        let with_global = Settings::merge(Settings::defaults(), self.global_layer.clone());
        self.settings = Settings::merge(with_global, self.project_layer.clone());
    }

    /// Stop the watcher (used in tests and on shutdown). Idempotent.
    /// Already-issued [`broadcast::Receiver`] handles will see the channel
    /// close on their next `recv()`.
    pub fn stop_watching(&mut self) {
        // Dropping the handle drops the notify watcher (closing the
        // forward channel) and aborts the spawned task. Dropping the
        // sender closes the broadcast channel for surviving receivers.
        self.watch_handle.take();
    }
}

/// Build the set of paths (literal + canonicalised, plus parent dirs) we
/// will match notify events against. Matching is permissive: we'd rather
/// over-fire and let the post-reload diff filter than miss a real change.
fn build_match_targets(global: Option<&Path>, project: Option<&Path>) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    for p in [global, project].into_iter().flatten() {
        targets.push(p.to_path_buf());
        if let Ok(canon) = p.canonicalize()
            && !targets.contains(&canon)
        {
            targets.push(canon);
        }
        if let Some(parent) = p.parent() {
            let parent_buf = parent.to_path_buf();
            if !targets.contains(&parent_buf) {
                targets.push(parent_buf);
            }
            if let Ok(canon_parent) = parent.canonicalize()
                && !targets.contains(&canon_parent)
            {
                targets.push(canon_parent);
            }
        }
    }
    targets
}

/// Predicate: does this notify event refer to one of our target files
/// (or live inside one of our watched directories)?
///
/// `notify::Event::paths` lists every path the event covers. We match on:
/// - exact equality with a target;
/// - prefix-match against a watched parent dir (FSEvents often reports
///   directory-level events).
///
/// If no paths are populated (rare; some platforms emit a generic
/// "something changed" event) or the event is an error, return `true` so
/// the reload step can decide whether anything actually moved.
fn event_matches_targets(res: &notify::Result<notify::Event>, targets: &[PathBuf]) -> bool {
    let event = match res {
        Ok(e) => e,
        Err(_) => return true,
    };
    if event.paths.is_empty() {
        return true;
    }
    event
        .paths
        .iter()
        .any(|p| targets.iter().any(|t| p == t || p.starts_with(t)))
}

// ---------------------------------------------------------------------------
// Legacy JSON → YAML migration
// ---------------------------------------------------------------------------

/// Result of one [`migrate_legacy_json_settings`] invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationOutcome {
    /// No JSON file at the given base; nothing to do.
    NotApplicable,
    /// YAML already exists; assumed previously migrated.
    AlreadyMigrated,
    /// Successfully migrated. `.bak` left at `<base>/settings.json.bak`.
    Migrated {
        yaml_path: PathBuf,
        backup_path: PathBuf,
    },
    /// JSON exists but couldn't be parsed/converted/written. JSON untouched.
    Failed { reason: String },
}

/// Top-level keys recognised by the new [`Settings`] struct, in kebab-case.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "last-changelog-version",
    "default-provider",
    "default-model",
    "default-thinking-level",
    "transport",
    "steering-mode",
    "follow-up-mode",
    "shell-path",
    "shell-command-prefix",
    "theme",
    "compaction",
    "branch-summary",
    "retry",
    "hide-thinking-block",
    "quiet-startup",
    "npm-command",
    "collapse-changelog",
    "enable-install-telemetry",
    "packages",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "enable-skill-commands",
    "terminal",
    "images",
    "enabled-models",
    "double-escape-action",
    "tree-filter-mode",
    "thinking-budgets",
    "editor-padding-x",
    "autocomplete-max-visible",
    "show-hardware-cursor",
    "markdown",
    "warnings",
    "session-dir",
];

/// Sub-struct keys we recognise (kebab-case) for `compaction` / `retry`.
const KNOWN_COMPACTION: &[&str] = &[
    "enabled",
    "threshold",
    "keep-recent-tokens",
    "max-context-tokens",
];
const KNOWN_RETRY: &[&str] = &[
    "enabled",
    "max-retries",
    "initial-delay-ms",
    "max-delay-ms",
    "provider",
];

/// Convert one snake_case identifier to kebab-case (`a_b_c` → `a-b-c`).
fn snake_to_kebab(s: &str) -> String {
    s.replace('_', "-")
}

/// Top-level fields whose string value is a kebab-cased enum tag in the
/// new schema. Legacy JSON often serialised these as snake_case; when the
/// raw value (after kebab conversion) still doesn't match a known tag we
/// fall back to the original.
///
/// Each entry is `(field_name_in_kebab_case, &[allowed_kebab_values])`.
const ENUM_VALUE_FIELDS: &[(&str, &[&str])] = &[
    ("theme", &["dark", "light", "high-contrast", "system"]),
    (
        "default-thinking-level",
        &["off", "minimal", "low", "medium", "high", "xhigh"],
    ),
];

/// Walk a `serde_json::Value` and rewrite every map key from snake_case to
/// kebab-case. Recurses through nested maps and arrays.
///
/// Object values whose key is one of [`ENUM_VALUE_FIELDS`] also have their
/// **string value** snake→kebab-converted, but only when the converted
/// form is a known enum tag — otherwise we leave the original alone so a
/// genuinely unknown value still surfaces a deserialization error.
fn rekey_to_kebab(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let new_key = snake_to_kebab(&k);
                let new_value = match (&v, lookup_enum_field(&new_key)) {
                    (serde_json::Value::String(s), Some(allowed)) => {
                        let kebab = snake_to_kebab(s);
                        if allowed.contains(&kebab.as_str()) {
                            serde_json::Value::String(kebab)
                        } else {
                            serde_json::Value::String(s.clone())
                        }
                    }
                    _ => rekey_to_kebab(v),
                };
                out.insert(new_key, new_value);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(rekey_to_kebab).collect())
        }
        other => other,
    }
}

fn lookup_enum_field(field: &str) -> Option<&'static [&'static str]> {
    ENUM_VALUE_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, values)| *values)
}

/// Filter a (kebab-cased) JSON value to keep only fields recognised by the
/// current [`Settings`] schema. Unknown top-level and known-substruct fields
/// are dropped with a `tracing::warn!`. Special-case rewrites:
///
/// - `retry.base-delay-ms` (legacy) → `retry.initial-delay-ms` (new schema).
fn filter_known_fields(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(map) = value else {
        return value;
    };
    let mut out = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        if !KNOWN_TOP_LEVEL.contains(&k.as_str()) {
            tracing::warn!(field = %k, "settings migration: dropped unknown legacy field");
            continue;
        }
        match k.as_str() {
            "compaction" => out.insert(k, filter_sub_object(v, "compaction", KNOWN_COMPACTION)),
            "retry" => {
                let v = rewrite_retry_legacy_keys(v);
                out.insert(k, filter_sub_object(v, "retry", KNOWN_RETRY))
            }
            _ => out.insert(k, v),
        };
    }
    serde_json::Value::Object(out)
}

/// Filter a sub-object's keys against `allowed`; warn-and-drop everything
/// else. Pass-through for non-objects (serde will produce a clear error
/// downstream if the shape is wrong).
fn filter_sub_object(
    value: serde_json::Value,
    parent: &str,
    allowed: &[&str],
) -> serde_json::Value {
    let serde_json::Value::Object(map) = value else {
        return value;
    };
    let mut out = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        if allowed.contains(&k.as_str()) {
            out.insert(k, v);
        } else {
            tracing::warn!(
                field = %format!("{parent}.{k}"),
                "settings migration: dropped legacy field",
            );
        }
    }
    serde_json::Value::Object(out)
}

/// Rename legacy `retry.base-delay-ms` to the new `retry.initial-delay-ms`.
/// If both are present, the new key wins and we warn about the duplicate.
fn rewrite_retry_legacy_keys(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut map) = value else {
        return value;
    };
    if let Some(legacy) = map.remove("base-delay-ms") {
        if map.contains_key("initial-delay-ms") {
            tracing::warn!(
                "settings migration: both retry.base-delay-ms and retry.initial-delay-ms present; preferring initial-delay-ms",
            );
        } else {
            map.insert("initial-delay-ms".to_string(), legacy);
        }
    }
    serde_json::Value::Object(map)
}

/// Migrate legacy `<base>/settings.json` to `<base>/settings.yaml` if needed.
///
/// - If `settings.yaml` already exists at `base`: no-op (`AlreadyMigrated`).
/// - If `settings.json` does not exist: no-op (`NotApplicable`).
/// - Else: parse JSON, write YAML, rename JSON → `settings.json.bak`.
///
/// Per-step errors are logged but do not propagate; on any failure the
/// JSON file is left untouched and YAML is not created. Re-runs are safe:
/// once YAML exists the function short-circuits with `AlreadyMigrated`.
pub fn migrate_legacy_json_settings(base: &Path) -> MigrationOutcome {
    let json_path = base.join("settings.json");
    let yaml_path = base.join("settings.yaml");
    let backup_path = base.join("settings.json.bak");

    // Order matters for idempotency: a successful previous run leaves
    // YAML in place (and the original JSON renamed to `.bak`). If we
    // checked JSON first, the second run would return `NotApplicable`
    // even though a migration had already taken place. Checking YAML
    // first keeps the post-migration steady state observable as
    // `AlreadyMigrated`.
    if yaml_path.exists() {
        return MigrationOutcome::AlreadyMigrated;
    }
    if !json_path.exists() {
        return MigrationOutcome::NotApplicable;
    }

    // 1. Read + parse JSON.
    let raw = match std::fs::read_to_string(&json_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                path = %json_path.display(),
                error = %e,
                "settings migration: failed to read legacy json",
            );
            return MigrationOutcome::Failed {
                reason: format!("read: {e}"),
            };
        }
    };
    let json_value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %json_path.display(),
                error = %e,
                "settings migration: legacy json parse failed",
            );
            return MigrationOutcome::Failed {
                reason: format!("json parse: {e}"),
            };
        }
    };

    // 2. Pre-process: snake_case → kebab-case keys, then drop fields the
    //    new schema no longer carries (warning the user about each).
    let kebabed = rekey_to_kebab(json_value);
    let filtered = filter_known_fields(kebabed);

    // 3. Deserialize into the strongly-typed Settings.
    let settings: Settings = match serde_json::from_value(filtered) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                path = %json_path.display(),
                error = %e,
                "settings migration: structural conversion failed",
            );
            return MigrationOutcome::Failed {
                reason: format!("conversion: {e}"),
            };
        }
    };

    // 4. Serialize to YAML and write next to the JSON. We deliberately do
    //    NOT touch the JSON until the YAML write has succeeded — that
    //    preserves user data on any failure.
    let yaml = match serde_yaml::to_string(&settings) {
        Ok(s) => s,
        Err(e) => {
            return MigrationOutcome::Failed {
                reason: format!("yaml emit: {e}"),
            };
        }
    };
    // Atomic write: stage to a temp file in the same directory, then
    // rename into place. A crashed/aborted run leaves the original YAML
    // (or no YAML at all) intact rather than a half-written file.
    {
        use std::io::Write as _;
        let parent = yaml_path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = match tempfile::NamedTempFile::new_in(parent) {
            Ok(t) => t,
            Err(e) => {
                return MigrationOutcome::Failed {
                    reason: format!("yaml write: {e}"),
                };
            }
        };
        if let Err(e) = tmp.write_all(yaml.as_bytes()) {
            return MigrationOutcome::Failed {
                reason: format!("yaml write: {e}"),
            };
        }
        if let Err(e) = tmp.persist(&yaml_path) {
            return MigrationOutcome::Failed {
                reason: format!("yaml write: {e}"),
            };
        }
    }

    // 5. Rename JSON → .bak. On POSIX `rename` is atomic and overwrites
    //    an existing target; on Windows it errors if the target exists.
    //    If the rename fails we keep the YAML and log; the next call will
    //    short-circuit on `AlreadyMigrated` because YAML now exists.
    if let Err(e) = std::fs::rename(&json_path, &backup_path) {
        tracing::warn!(
            json = %json_path.display(),
            backup = %backup_path.display(),
            error = %e,
            "settings: migration wrote yaml but failed to rename json to .bak",
        );
    }

    tracing::info!(
        json = %json_path.display(),
        yaml = %yaml_path.display(),
        backup = %backup_path.display(),
        "settings: migrated legacy json to yaml",
    );

    MigrationOutcome::Migrated {
        yaml_path,
        backup_path,
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
        // Install telemetry defaults to ON, matching the TS reference.
        assert!(s.enable_install_telemetry());
    }

    #[test]
    fn enable_install_telemetry_round_trips_through_yaml() {
        let dir = TempDir::new().unwrap();
        // Explicit `false` survives a load.
        let p = write_yaml(&dir, "off.yaml", "enable-install-telemetry: false\n");
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s.enable_install_telemetry, Some(false));
        assert!(!s.enable_install_telemetry());

        // Explicit `true` survives a load.
        let p = write_yaml(&dir, "on.yaml", "enable-install-telemetry: true\n");
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s.enable_install_telemetry, Some(true));
        assert!(s.enable_install_telemetry());

        // Field absent → default ON via the accessor (matches TS).
        let p = write_yaml(&dir, "absent.yaml", "default-model: foo\n");
        let s = Settings::load(Some(&p), None).unwrap();
        assert!(s.enable_install_telemetry());
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
        let g = write_yaml(&dir, "global.yaml", "compaction:\n  threshold: 0.7\n");
        let p = write_yaml(&dir, "project.yaml", "compaction:\n  enabled: false\n");
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

    // ---------------------------------------------------------------------
    // Hot-reload watcher tests (T4.2)
    // ---------------------------------------------------------------------

    use tokio::sync::broadcast::error::RecvError;

    /// Build a manager whose project layer points at the given file path
    /// (without requiring the file to exist yet) and no global layer.
    fn manager_for_project(project: PathBuf) -> SettingsManager {
        // Bypass `from_cwd` (which forces the standard layout) and use
        // `Settings::load_layers` directly so we control which files exist.
        let (global_layer, project_layer, settings) =
            Settings::load_layers(None, Some(project.as_path())).unwrap_or_else(|_| {
                (
                    Settings::default(),
                    Settings::default(),
                    Settings::defaults(),
                )
            });
        SettingsManager {
            settings,
            global_layer,
            project_layer,
            project_path: Some(project),
            global_path: None,
            watch_handle: None,
        }
    }

    /// Atomic file rewrite via tmp + rename — closer to how editors save
    /// and a robust way to trigger a single coalescable filesystem event.
    fn atomic_write(path: &Path, content: &str) {
        use std::io::Write;
        let parent = path.parent().expect("path has a parent");
        let tmp = parent.join(format!(
            ".{}.tmp",
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("settings")
        ));
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().ok();
        drop(f);
        std::fs::rename(&tmp, path).unwrap();
    }

    #[tokio::test]
    async fn watcher_delivers_event_to_single_subscriber() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        std::fs::write(&project, "default-model: alpha\n").unwrap();

        let mut mgr = manager_for_project(project.clone());
        assert_eq!(mgr.current().default_model.as_deref(), Some("alpha"));

        let mut rx = mgr.watch();

        // Modify after the watcher is up.
        atomic_write(&project, "default-model: beta\n");

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event arrived within timeout")
            .expect("broadcast not closed");
        assert_eq!(event.previous.default_model.as_deref(), Some("alpha"));
        assert_eq!(event.current.default_model.as_deref(), Some("beta"));
    }

    #[tokio::test]
    async fn watcher_broadcasts_to_multiple_subscribers() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        std::fs::write(&project, "default-model: alpha\n").unwrap();

        let mut mgr = manager_for_project(project.clone());
        let mut rx_a = mgr.watch();
        let mut rx_b = mgr.watch();

        atomic_write(&project, "default-model: gamma\n");

        let ev_a = tokio::time::timeout(Duration::from_secs(2), rx_a.recv())
            .await
            .expect("a: timed out")
            .expect("a: closed");
        let ev_b = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
            .await
            .expect("b: timed out")
            .expect("b: closed");

        assert_eq!(ev_a.current.default_model.as_deref(), Some("gamma"));
        assert_eq!(ev_b.current.default_model.as_deref(), Some("gamma"));
    }

    #[tokio::test]
    async fn watcher_suppresses_event_when_content_unchanged() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        let body = "default-model: alpha\n";
        std::fs::write(&project, body).unwrap();

        let mut mgr = manager_for_project(project.clone());
        let mut rx = mgr.watch();

        // Re-write the same content.
        atomic_write(&project, body);

        // Allow the debounce + reload to run, then assert no event.
        let outcome = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            outcome.is_err(),
            "expected no event for identical write, got {outcome:?}",
        );
    }

    #[tokio::test]
    async fn watcher_debounces_rapid_writes_into_one_event() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        std::fs::write(&project, "default-model: v0\n").unwrap();

        let mut mgr = manager_for_project(project.clone());
        let mut rx = mgr.watch();

        // 5 rapid writes; final state is `default-model: v5`.
        for i in 1..=5 {
            atomic_write(&project, &format!("default-model: v{i}\n"));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // First event must arrive.
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("first event timed out")
            .expect("broadcast closed");
        assert_eq!(first.current.default_model.as_deref(), Some("v5"));

        // Within a generous window, no further events should arrive — the
        // debounce should have coalesced the burst.
        let second = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            second.is_err(),
            "expected only one event from a debounced burst, got {second:?}",
        );
    }

    #[tokio::test]
    async fn watcher_survives_malformed_yaml_then_recovers() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        std::fs::write(&project, "default-model: alpha\n").unwrap();

        let mut mgr = manager_for_project(project.clone());
        let mut rx = mgr.watch();

        // Bad write — watcher should log + skip.
        atomic_write(&project, "default-model: [unterminated\n");
        // Give the debounce window time to fire and the reload to fail.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Good write — should fire.
        atomic_write(&project, "default-model: recovered\n");

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event after recovery timed out")
            .expect("broadcast closed");
        assert_eq!(event.current.default_model.as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn stop_watching_closes_existing_subscribers() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("settings.yaml");
        std::fs::write(&project, "default-model: alpha\n").unwrap();

        let mut mgr = manager_for_project(project.clone());
        let mut rx = mgr.watch();

        mgr.stop_watching();

        let res = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("recv resolved within timeout");
        assert!(matches!(res, Err(RecvError::Closed)), "got {res:?}");

        // Idempotent.
        mgr.stop_watching();
    }

    #[tokio::test]
    async fn project_layer_overrides_global_under_watcher() {
        let dir = TempDir::new().unwrap();
        // Use distinct directories so the watcher registers two parents.
        let global_dir = dir.path().join("global");
        let project_dir = dir.path().join("project");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let global = global_dir.join("settings.yaml");
        let project = project_dir.join("settings.yaml");
        std::fs::write(&global, "default-model: A\n").unwrap();
        // Project file does not yet exist.

        let (global_layer, project_layer, settings) =
            Settings::load_layers(Some(global.as_path()), Some(project.as_path())).unwrap();
        let mut mgr = SettingsManager {
            settings,
            global_layer,
            project_layer,
            project_path: Some(project.clone()),
            global_path: Some(global.clone()),
            watch_handle: None,
        };
        assert_eq!(mgr.current().default_model.as_deref(), Some("A"));

        let mut rx = mgr.watch();

        // Create the project file mid-watch — it should shadow global.
        atomic_write(&project, "default-model: B\n");

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event timed out")
            .expect("broadcast closed");
        assert_eq!(event.previous.default_model.as_deref(), Some("A"));
        assert_eq!(event.current.default_model.as_deref(), Some("B"));
    }

    // ---------------------------------------------------------------------
    // Legacy JSON → YAML migration (T4.4)
    // ---------------------------------------------------------------------

    fn write_text(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn migration_no_files_is_not_applicable() {
        let dir = TempDir::new().unwrap();
        let outcome = migrate_legacy_json_settings(dir.path());
        assert_eq!(outcome, MigrationOutcome::NotApplicable);
        // Nothing created.
        assert!(!dir.path().join("settings.yaml").exists());
        assert!(!dir.path().join("settings.json").exists());
        assert!(!dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn migration_writes_yaml_and_backup_when_only_json_exists() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(&json, r#"{"default_model": "gpt-4o"}"#);

        let outcome = migrate_legacy_json_settings(dir.path());
        let yaml_path = dir.path().join("settings.yaml");
        let backup_path = dir.path().join("settings.json.bak");
        assert_eq!(
            outcome,
            MigrationOutcome::Migrated {
                yaml_path: yaml_path.clone(),
                backup_path: backup_path.clone(),
            }
        );
        assert!(yaml_path.exists());
        assert!(backup_path.exists());
        assert!(!json.exists());
        let yaml_body = std::fs::read_to_string(&yaml_path).unwrap();
        assert!(
            yaml_body.contains("default-model: gpt-4o"),
            "expected kebab-case key + value, got: {yaml_body}",
        );
    }

    #[test]
    fn migration_is_already_migrated_when_yaml_exists() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        let yaml = dir.path().join("settings.yaml");
        let json_body = r#"{"default_model": "gpt-4o"}"#;
        let yaml_body = "default-model: claude-x\n";
        write_text(&json, json_body);
        write_text(&yaml, yaml_body);

        let outcome = migrate_legacy_json_settings(dir.path());
        assert_eq!(outcome, MigrationOutcome::AlreadyMigrated);
        // Both files unchanged.
        assert_eq!(std::fs::read_to_string(&json).unwrap(), json_body);
        assert_eq!(std::fs::read_to_string(&yaml).unwrap(), yaml_body);
        assert!(!dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn migration_failed_leaves_files_untouched_on_bad_json() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(&json, "not json");

        let outcome = migrate_legacy_json_settings(dir.path());
        match outcome {
            MigrationOutcome::Failed { reason } => {
                assert!(reason.contains("json parse"), "unexpected reason: {reason}",)
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // JSON survives, YAML never created.
        assert!(json.exists());
        assert_eq!(std::fs::read_to_string(&json).unwrap(), "not json");
        assert!(!dir.path().join("settings.yaml").exists());
        assert!(!dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn migrated_yaml_loads_via_settings_load() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(
            &json,
            r#"{
                "default_provider": "openai",
                "default_model": "gpt-4o",
                "compaction": {"enabled": false}
            }"#,
        );

        let outcome = migrate_legacy_json_settings(dir.path());
        let yaml_path = match outcome {
            MigrationOutcome::Migrated { yaml_path, .. } => yaml_path,
            other => panic!("expected Migrated, got {other:?}"),
        };

        let s = Settings::load(None, Some(&yaml_path)).unwrap();
        assert_eq!(s.default_provider.as_deref(), Some("openai"));
        assert_eq!(s.default_model.as_deref(), Some("gpt-4o"));
        assert!(!s.compaction.enabled());
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(&json, r#"{"default_model": "gpt-4o"}"#);

        let first = migrate_legacy_json_settings(dir.path());
        assert!(matches!(first, MigrationOutcome::Migrated { .. }));

        let yaml = dir.path().join("settings.yaml");
        let backup = dir.path().join("settings.json.bak");
        let yaml_before = std::fs::read_to_string(&yaml).unwrap();
        let backup_before = std::fs::read_to_string(&backup).unwrap();

        let second = migrate_legacy_json_settings(dir.path());
        assert_eq!(second, MigrationOutcome::AlreadyMigrated);

        // No mutation between runs.
        assert_eq!(std::fs::read_to_string(&yaml).unwrap(), yaml_before);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), backup_before);
    }

    #[test]
    fn migration_drops_legacy_compaction_reserve_tokens() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(
            &json,
            r#"{
                "compaction": {
                    "enabled": true,
                    "reserve_tokens": 5000,
                    "keep_recent_tokens": 12345
                }
            }"#,
        );

        let outcome = migrate_legacy_json_settings(dir.path());
        assert!(matches!(outcome, MigrationOutcome::Migrated { .. }));

        let yaml_body = std::fs::read_to_string(dir.path().join("settings.yaml")).unwrap();
        assert!(
            !yaml_body.contains("reserve"),
            "legacy field leaked into yaml: {yaml_body}",
        );
        // Recognised fields survive.
        assert!(yaml_body.contains("keep-recent-tokens: 12345"));

        // And it round-trips through Settings::load with the surviving value.
        let yaml_path = dir.path().join("settings.yaml");
        let s = Settings::load(None, Some(&yaml_path)).unwrap();
        assert_eq!(s.compaction.keep_recent_tokens(), 12_345);
    }

    #[test]
    fn from_cwd_triggers_project_migration() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path();
        let project_json = cwd.join(".hand").join("settings.json");
        write_text(&project_json, r#"{"default_model": "gpt-4o"}"#);

        let mgr = SettingsManager::from_cwd(cwd).unwrap();
        // Settings actually loaded the migrated value.
        assert_eq!(mgr.current().default_model.as_deref(), Some("gpt-4o"));

        // YAML + .bak in place; original JSON gone.
        assert!(cwd.join(".hand").join("settings.yaml").exists());
        assert!(cwd.join(".hand").join("settings.json.bak").exists());
        assert!(!project_json.exists());
    }

    /// F31: Guard against `KNOWN_*` allow-lists drifting from the actual
    /// `Settings` schema. If a new field is added to `Settings` but not
    /// added to `KNOWN_TOP_LEVEL`, every legacy migration silently drops
    /// it. Same logic for `KNOWN_COMPACTION` / `KNOWN_RETRY`.
    #[test]
    fn migration_known_field_lists_match_schema() {
        // Top-level fields: serialize defaults to a JSON object and
        // assert every kebab-case key appears in `KNOWN_TOP_LEVEL`.
        let value = serde_json::to_value(Settings::defaults()).unwrap();
        let serde_json::Value::Object(map) = value else {
            panic!("Settings should serialize to an object")
        };
        for k in map.keys() {
            let kebab = k.replace('_', "-");
            assert!(
                KNOWN_TOP_LEVEL.contains(&kebab.as_str()),
                "KNOWN_TOP_LEVEL missing field: {kebab}",
            );
        }

        // Sub-struct fields: same check for compaction / retry.
        let value = serde_json::to_value(CompactionSettings::with_defaults()).unwrap();
        let serde_json::Value::Object(map) = value else {
            panic!("CompactionSettings should serialize to an object")
        };
        for k in map.keys() {
            let kebab = k.replace('_', "-");
            assert!(
                KNOWN_COMPACTION.contains(&kebab.as_str()),
                "KNOWN_COMPACTION missing field: {kebab}",
            );
        }

        let value = serde_json::to_value(RetrySettings::with_defaults()).unwrap();
        let serde_json::Value::Object(map) = value else {
            panic!("RetrySettings should serialize to an object")
        };
        for k in map.keys() {
            let kebab = k.replace('_', "-");
            assert!(
                KNOWN_RETRY.contains(&kebab.as_str()),
                "KNOWN_RETRY missing field: {kebab}",
            );
        }
    }

    /// F33: legacy JSON values that are themselves snake_case enum tags
    /// (e.g. `theme: "high_contrast"`) must be rewritten to kebab-case
    /// during migration so they match the new `ThemeSetting` shape.
    #[test]
    fn migration_rewrites_snake_case_enum_values() {
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(&json, r#"{"theme": "high_contrast"}"#);

        let outcome = migrate_legacy_json_settings(dir.path());
        assert!(matches!(outcome, MigrationOutcome::Migrated { .. }));

        let yaml_body = std::fs::read_to_string(dir.path().join("settings.yaml")).unwrap();
        assert!(
            yaml_body.contains("theme: high-contrast"),
            "expected kebab-case enum value, got: {yaml_body}",
        );

        let yaml_path = dir.path().join("settings.yaml");
        let s = Settings::load(None, Some(&yaml_path)).unwrap();
        assert_eq!(s.theme(), ThemeSetting::HighContrast);
    }

    #[test]
    fn migration_renames_retry_base_delay_ms_to_initial_delay_ms() {
        // Sanity check on the legacy-rename path: pre-T4.1 retry settings
        // used `base_delay_ms`; new schema uses `initial-delay-ms`.
        let dir = TempDir::new().unwrap();
        let json = dir.path().join("settings.json");
        write_text(
            &json,
            r#"{
                "retry": {
                    "enabled": true,
                    "max_retries": 5,
                    "base_delay_ms": 2500,
                    "max_delay_ms": 60000
                }
            }"#,
        );

        let outcome = migrate_legacy_json_settings(dir.path());
        assert!(matches!(outcome, MigrationOutcome::Migrated { .. }));

        let yaml_path = dir.path().join("settings.yaml");
        let s = Settings::load(None, Some(&yaml_path)).unwrap();
        assert_eq!(s.retry.max_retries(), 5);
        assert_eq!(s.retry.initial_delay_ms(), 2_500);
        assert_eq!(s.retry.max_delay_ms(), 60_000);
    }

    // ---------------------------------------------------------------------
    // Resource-side fields: packages / extensions / skills / prompts / themes
    // ---------------------------------------------------------------------

    #[test]
    fn resource_side_accessors_default_empty() {
        let s = Settings::defaults();
        assert!(s.packages().is_empty());
        assert!(s.extensions().is_empty());
        assert!(s.skills().is_empty());
        assert!(s.prompts().is_empty());
        assert!(s.themes().is_empty());
    }

    #[test]
    fn package_source_bare_round_trips_through_yaml() {
        let dir = TempDir::new().unwrap();
        let p = write_yaml(
            &dir,
            "settings.yaml",
            "packages:\n  - npm:@scope/pkg\n  - git:owner/repo\n",
        );
        let s = Settings::load(Some(&p), None).unwrap();
        let pkgs = s.packages();
        assert_eq!(pkgs.len(), 2);
        assert!(matches!(&pkgs[0], PackageSource::Bare(spec) if spec == "npm:@scope/pkg"));
        assert!(matches!(&pkgs[1], PackageSource::Bare(spec) if spec == "git:owner/repo"));
        assert_eq!(pkgs[0].source(), "npm:@scope/pkg");
    }

    #[test]
    fn package_source_filtered_round_trips_through_yaml() {
        let dir = TempDir::new().unwrap();
        let body = "\
packages:
  - source: github:owner/repo
    extensions:
      - ext-a
      - ext-b
    skills:
      - skill-x
    themes: []
";
        let p = write_yaml(&dir, "settings.yaml", body);
        let s = Settings::load(Some(&p), None).unwrap();
        let pkgs = s.packages();
        assert_eq!(pkgs.len(), 1);
        match &pkgs[0] {
            PackageSource::Filtered {
                source,
                extensions,
                skills,
                prompts,
                themes,
            } => {
                assert_eq!(source, "github:owner/repo");
                assert_eq!(
                    extensions.as_deref(),
                    Some(&["ext-a".to_string(), "ext-b".to_string()][..]),
                );
                assert_eq!(skills.as_deref(), Some(&["skill-x".to_string()][..]));
                assert!(prompts.is_none(), "absent list stays None");
                assert_eq!(themes.as_deref(), Some(&[][..]), "empty list survives");
            }
            other => panic!("expected Filtered, got {other:?}"),
        }
    }

    #[test]
    fn package_source_serializes_round_trip() {
        // Round-trip a Vec through serde_yaml to pin the on-disk shape.
        let pkgs = vec![
            PackageSource::Bare("npm:foo".into()),
            PackageSource::Filtered {
                source: "git:bar".into(),
                extensions: Some(vec!["a".into()]),
                skills: None,
                prompts: None,
                themes: None,
            },
        ];
        let yaml = serde_yaml::to_string(&pkgs).unwrap();
        let parsed: Vec<PackageSource> = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, pkgs);
    }

    #[test]
    fn project_packages_replace_global_whole_list() {
        // Whole-list override semantics: if project sets `packages`, the
        // global list is dropped entirely (it doesn't union or interleave).
        let dir = TempDir::new().unwrap();
        let g = write_yaml(
            &dir,
            "global.yaml",
            "packages:\n  - npm:global-pkg\n  - git:global-repo\n",
        );
        let p = write_yaml(&dir, "project.yaml", "packages:\n  - npm:project-only\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        let pkgs = s.packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].source(), "npm:project-only");
    }

    #[test]
    fn project_absent_packages_leaves_global_list_intact() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "packages:\n  - npm:global-pkg\n");
        let p = write_yaml(&dir, "project.yaml", "default-model: gpt-4o\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        assert_eq!(s.packages().len(), 1);
        assert_eq!(s.packages()[0].source(), "npm:global-pkg");
    }

    #[test]
    fn project_explicit_empty_packages_clears_global_list() {
        // `Some(vec![])` is "explicitly cleared", distinct from `None`
        // ("layer didn't say"). Confirm the merge respects that distinction.
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "packages:\n  - npm:global-pkg\n");
        let p = write_yaml(&dir, "project.yaml", "packages: []\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        assert!(s.packages().is_empty());
    }

    #[test]
    fn extensions_skills_prompts_themes_round_trip() {
        let dir = TempDir::new().unwrap();
        let body = "\
extensions:
  - ./local/ext.ts
skills:
  - ~/skills
prompts:
  - ./prompts
themes:
  - ./themes/dark.json
";
        let p = write_yaml(&dir, "settings.yaml", body);
        let s = Settings::load(Some(&p), None).unwrap();
        assert_eq!(s.extensions(), &["./local/ext.ts".to_string()]);
        assert_eq!(s.skills(), &["~/skills".to_string()]);
        assert_eq!(s.prompts(), &["./prompts".to_string()]);
        assert_eq!(s.themes(), &["./themes/dark.json".to_string()]);
    }

    #[test]
    fn project_extension_paths_replace_global() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(
            &dir,
            "global.yaml",
            "extensions:\n  - /global/a\n  - /global/b\n",
        );
        let p = write_yaml(&dir, "project.yaml", "extensions:\n  - /project/x\n");
        let s = Settings::load(Some(&g), Some(&p)).unwrap();
        assert_eq!(s.extensions(), &["/project/x".to_string()]);
    }

    // ---------------------------------------------------------------------
    // Layer-targeted writes (Track 1: SettingsManager YAML persistence)
    // ---------------------------------------------------------------------

    fn manager_with_layers(global_path: PathBuf, project_path: PathBuf) -> SettingsManager {
        let (global_layer, project_layer, settings) =
            Settings::load_layers(Some(global_path.as_path()), Some(project_path.as_path()))
                .unwrap();
        SettingsManager {
            settings,
            global_layer,
            project_layer,
            project_path: Some(project_path),
            global_path: Some(global_path),
            watch_handle: None,
        }
    }

    #[test]
    fn layer_accessors_expose_raw_layers() {
        let dir = TempDir::new().unwrap();
        let g = write_yaml(&dir, "global.yaml", "default-model: from-global\n");
        let p = write_yaml(&dir, "project.yaml", "packages:\n  - npm:proj-only\n");
        let mgr = manager_with_layers(g, p);
        // Project layer carries only what was specified — no defaults.
        assert!(mgr.project_layer().default_model.is_none());
        assert_eq!(mgr.project_layer().packages().len(), 1);
        // Global layer also raw — no defaults injected.
        assert_eq!(
            mgr.global_layer().default_model.as_deref(),
            Some("from-global"),
        );
        assert!(mgr.global_layer().packages.is_none());
        // Merged view still has defaults applied.
        assert_eq!(mgr.current().default_model.as_deref(), Some("from-global"));
        assert!(mgr.current().enable_install_telemetry());
    }

    #[test]
    fn set_packages_then_save_reload_roundtrips_for_global_scope() {
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "").unwrap();
        std::fs::write(&p, "").unwrap();

        let mut mgr = manager_with_layers(g.clone(), p.clone());
        mgr.set_packages(
            SettingsScope::Global,
            Some(vec![PackageSource::Bare("npm:hello".into())]),
        );
        mgr.save(SettingsScope::Global).unwrap();

        let reloaded = Settings::load(Some(&g), Some(&p)).unwrap();
        let pkgs = reloaded.packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].source(), "npm:hello");
    }

    #[test]
    fn set_packages_then_save_reload_roundtrips_for_project_scope() {
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "packages:\n  - npm:keep-me\n").unwrap();
        std::fs::write(&p, "").unwrap();

        let mut mgr = manager_with_layers(g.clone(), p.clone());
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:proj".into())]),
        );
        mgr.save(SettingsScope::Project).unwrap();

        // Project YAML now has the new entry; global YAML untouched.
        let project_yaml = std::fs::read_to_string(&p).unwrap();
        assert!(project_yaml.contains("npm:proj"));
        let global_yaml = std::fs::read_to_string(&g).unwrap();
        assert!(global_yaml.contains("npm:keep-me"));

        // After reload, project shadows global (whole-list override).
        let reloaded = Settings::load(Some(&g), Some(&p)).unwrap();
        let pkgs = reloaded.packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].source(), "npm:proj");
    }

    #[test]
    fn set_extensions_skills_prompts_themes_persist_per_scope() {
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "").unwrap();
        std::fs::write(&p, "").unwrap();

        let mut mgr = manager_with_layers(g.clone(), p.clone());
        mgr.set_extensions(SettingsScope::Project, Some(vec!["/p/ext".into()]));
        mgr.set_skills(SettingsScope::Project, Some(vec!["/p/skill".into()]));
        mgr.set_prompts(SettingsScope::Global, Some(vec!["/g/pr".into()]));
        mgr.set_themes(SettingsScope::Global, Some(vec!["/g/th".into()]));
        mgr.save(SettingsScope::Project).unwrap();
        mgr.save(SettingsScope::Global).unwrap();

        let reloaded = Settings::load(Some(&g), Some(&p)).unwrap();
        assert_eq!(reloaded.extensions(), &["/p/ext".to_string()]);
        assert_eq!(reloaded.skills(), &["/p/skill".to_string()]);
        assert_eq!(reloaded.prompts(), &["/g/pr".to_string()]);
        assert_eq!(reloaded.themes(), &["/g/th".to_string()]);
    }

    #[test]
    fn set_packages_does_not_touch_other_layer() {
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "default-model: g-model\n").unwrap();
        std::fs::write(&p, "default-model: p-model\n").unwrap();

        let mut mgr = manager_with_layers(g.clone(), p.clone());
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:p".into())]),
        );
        mgr.save(SettingsScope::Project).unwrap();

        // The project YAML should round-trip both default-model and the
        // packages list — a layer write must not lose unrelated fields.
        let reloaded = Settings::load(None, Some(&p)).unwrap();
        assert_eq!(reloaded.default_model.as_deref(), Some("p-model"));
        assert_eq!(reloaded.packages().len(), 1);
        // Global YAML on disk is untouched (we never called save(Global)).
        let global_yaml = std::fs::read_to_string(&g).unwrap();
        assert_eq!(global_yaml, "default-model: g-model\n");
    }

    #[test]
    fn save_with_no_path_returns_error() {
        let mut mgr = SettingsManager::in_memory();
        mgr.set_packages(
            SettingsScope::Global,
            Some(vec![PackageSource::Bare("npm:foo".into())]),
        );
        let err = mgr.save(SettingsScope::Global).unwrap_err();
        assert!(matches!(err, SettingsError::NoPath { .. }));
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        // Path under a nested directory that does not exist yet.
        let p = dir.path().join("nested/.hand/settings.yaml");
        let g = dir.path().join("global.yaml");
        std::fs::write(&g, "").unwrap();
        let (global_layer, project_layer, settings) =
            Settings::load_layers(Some(g.as_path()), None).unwrap();
        let mut mgr = SettingsManager {
            settings,
            global_layer,
            project_layer,
            project_path: Some(p.clone()),
            global_path: Some(g),
            watch_handle: None,
        };
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:foo".into())]),
        );
        mgr.save(SettingsScope::Project).unwrap();
        assert!(p.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_unix_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "").unwrap();
        std::fs::write(&p, "").unwrap();
        // Loosen the existing mode so we can detect the reassert.
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&p, perms).unwrap();

        let mut mgr = manager_with_layers(g, p.clone());
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:foo".into())]),
        );
        mgr.save(SettingsScope::Project).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
    }

    #[test]
    fn save_is_atomic_when_target_does_not_exist() {
        // A successful save must produce a complete file; we can't easily
        // inject a partial-write failure mid-save, but we can confirm
        // that NamedTempFile::persist hands us a syntactically valid YAML
        // for the layer (i.e. no partial content leaks through).
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "").unwrap();
        std::fs::write(&p, "default-model: original\n").unwrap();

        let mut mgr = manager_with_layers(g, p.clone());
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:atomic".into())]),
        );
        mgr.save(SettingsScope::Project).unwrap();
        // Reload through Settings::load — if the rename had been torn we'd
        // see a YAML parse error; instead we get the merged view.
        let reloaded = Settings::load(None, Some(&p)).unwrap();
        assert_eq!(reloaded.default_model.as_deref(), Some("original"));
        assert_eq!(reloaded.packages().len(), 1);
    }

    #[test]
    fn current_reflects_in_memory_set_before_save() {
        // The merged view is recomputed on every set; tests that observe
        // current() between set and save should see the new value.
        let dir = TempDir::new().unwrap();
        let g = dir.path().join("global.yaml");
        let p = dir.path().join("project.yaml");
        std::fs::write(&g, "").unwrap();
        std::fs::write(&p, "").unwrap();

        let mut mgr = manager_with_layers(g, p);
        assert!(mgr.current().packages().is_empty());
        mgr.set_packages(
            SettingsScope::Project,
            Some(vec![PackageSource::Bare("npm:in-memory".into())]),
        );
        let pkgs = mgr.current().packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].source(), "npm:in-memory");
    }
}
