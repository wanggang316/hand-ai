//! System diagnostics — check environment, API keys, and dependencies.
//!
//! In addition to the legacy `DiagCheck` rows, the report surfaces the new
//! Phase-6 subsystems: on-disk auth storage, install-telemetry gate, the
//! startup-timings gate, skill-discovery errors, and the layered-settings
//! summary. Every subsystem records its own error in the relevant status
//! struct rather than panicking — `--diagnostics` must always finish.

use std::fmt;
use std::path::PathBuf;
use std::process::Command;

use crate::core::auth_storage::AuthStorage;
use crate::core::settings::SettingsManager;
use crate::core::skills;
use crate::core::telemetry;
use crate::core::timings;

/// Status of a diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagStatus {
    /// Check passed.
    Ok,
    /// Check passed with a note.
    Warn(String),
    /// Check failed.
    Error(String),
}

impl fmt::Display for DiagStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiagStatus::Ok => write!(f, "OK"),
            DiagStatus::Warn(msg) => write!(f, "WARN: {msg}"),
            DiagStatus::Error(msg) => write!(f, "ERROR: {msg}"),
        }
    }
}

/// A single diagnostic check result.
#[derive(Debug, Clone)]
pub struct DiagCheck {
    pub name: String,
    pub status: DiagStatus,
    pub value: Option<String>,
}

/// On-disk auth storage status (`~/.hand/agent/auth.json`).
///
/// `error` carries the human-readable failure message if the file existed
/// but couldn't be parsed; `provider_count` and `mode_octal` are populated
/// only when the load succeeded.
#[derive(Debug, Clone)]
pub struct AuthStorageStatus {
    pub path: PathBuf,
    pub exists: bool,
    /// Unix file mode (permission bits, e.g. `0o600`). `None` on non-Unix
    /// or when the file doesn't exist.
    pub mode_octal: Option<u32>,
    /// Number of provider entries in the file. `None` when the file is
    /// missing or failed to parse.
    pub provider_count: Option<usize>,
    pub error: Option<String>,
}

/// Resolution outcome of [`telemetry::is_install_telemetry_enabled`].
#[derive(Debug, Clone)]
pub struct TelemetryStatus {
    pub enabled: bool,
    /// `"env"` when `HAND_TELEMETRY` decided the result, `"settings"`
    /// when the YAML layer set the flag, `"default"` when neither did.
    pub source: &'static str,
    /// Raw `HAND_TELEMETRY` env value if set, for reporting only.
    pub env_value: Option<String>,
    pub error: Option<String>,
}

/// Resolution outcome of [`timings::enabled`].
#[derive(Debug, Clone)]
pub struct TimingsStatus {
    pub enabled: bool,
    /// Raw `HAND_TIMING` env value if set.
    pub env_value: Option<String>,
}

/// One skill-discovery failure surfaced by [`skills::discover_skills`].
#[derive(Debug, Clone)]
pub struct SkillErrorSummary {
    /// Path of the offending SKILL.md, when known.
    pub path: Option<PathBuf>,
    /// `Display` of the `SkillError` — already includes the path.
    pub message: String,
}

/// Layered-settings summary: which YAML files were loaded and the merged
/// result rendered as YAML.
#[derive(Debug, Clone)]
pub struct SettingsLayerSummary {
    pub global_path: Option<PathBuf>,
    pub global_exists: bool,
    pub project_path: Option<PathBuf>,
    pub project_exists: bool,
    /// Resolved settings serialized as YAML. Empty on serializer failure.
    pub settings_yaml: String,
    pub error: Option<String>,
}

/// Complete diagnostics report.
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    pub checks: Vec<DiagCheck>,
    pub auth_storage: AuthStorageStatus,
    pub telemetry: TelemetryStatus,
    pub timings: TimingsStatus,
    pub skill_errors: Vec<SkillErrorSummary>,
    pub settings: SettingsLayerSummary,
}

impl DiagnosticsReport {
    /// Count passed checks.
    pub fn ok_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == DiagStatus::Ok)
            .count()
    }

    /// Count warnings.
    pub fn warn_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.status, DiagStatus::Warn(_)))
            .count()
    }

    /// Count errors.
    pub fn error_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.status, DiagStatus::Error(_)))
            .count()
    }

    /// True when any subsystem reports a hard error. Used by `main.rs` to
    /// pick the process exit code.
    ///
    /// Skill-discovery errors are NOT counted: a malformed SKILL.md is a
    /// per-file warning condition, not a system-level failure (the rest of
    /// the agent keeps working). Auth-load and settings-load errors *are*
    /// counted because they signal a config layer that's unreadable.
    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
            || self.auth_storage.error.is_some()
            || self.telemetry.error.is_some()
            || self.settings.error.is_some()
    }
}

impl fmt::Display for DiagnosticsReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "System Diagnostics")?;
        writeln!(f, "==================")?;
        writeln!(f)?;

        let max_name = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);

        for check in &self.checks {
            let status_marker = match &check.status {
                DiagStatus::Ok => "✓",
                DiagStatus::Warn(_) => "⚠",
                DiagStatus::Error(_) => "✗",
            };
            let value = check
                .value
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            writeln!(
                f,
                "  {status_marker} {:<width$} {}{value}",
                check.name,
                check.status,
                width = max_name
            )?;
        }

        writeln!(f)?;
        writeln!(
            f,
            "Summary: {} passed, {} warnings, {} errors",
            self.ok_count(),
            self.warn_count(),
            self.error_count()
        )?;
        Ok(())
    }
}

/// Run all diagnostic checks against the process cwd. Convenience wrapper
/// around [`run_diagnostics_at`].
pub fn run_diagnostics() -> DiagnosticsReport {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_diagnostics_at(&cwd, AuthStorage::default_path().ok())
}

/// Run all diagnostic checks against an explicit cwd and auth-storage path.
///
/// `auth_path = None` records a "home directory not found" error in the
/// auth-storage status. Tests pass `Some(tmp/auth.json)` to avoid touching
/// the real `~/.hand/`.
pub fn run_diagnostics_at(cwd: &std::path::Path, auth_path: Option<PathBuf>) -> DiagnosticsReport {
    let mut checks = vec![
        // OS info
        check_os(),
        check_shell(),
        check_terminal(),
        check_terminal_size(),
        // Tools
        check_command("git", &["--version"]),
        check_command("cargo", &["--version"]),
    ];

    // API keys
    let api_keys = [
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("GEMINI_API_KEY", "Google"),
        ("GROQ_API_KEY", "Groq"),
        ("XAI_API_KEY", "xAI"),
        ("MISTRAL_API_KEY", "Mistral"),
    ];

    for (env_var, provider) in api_keys {
        checks.push(check_api_key(env_var, provider));
    }

    // .hand directory (legacy DiagCheck row).
    checks.push(check_hand_directory());

    let auth_storage = inspect_auth_storage(auth_path);
    let settings = inspect_settings(cwd);
    let telemetry = inspect_telemetry(cwd, &settings);
    let timings = inspect_timings();
    let skill_errors = inspect_skill_errors(cwd);

    DiagnosticsReport {
        checks,
        auth_storage,
        telemetry,
        timings,
        skill_errors,
        settings,
    }
}

fn inspect_auth_storage(path: Option<PathBuf>) -> AuthStorageStatus {
    let Some(path) = path else {
        return AuthStorageStatus {
            path: PathBuf::new(),
            exists: false,
            mode_octal: None,
            provider_count: None,
            error: Some("home directory not found".to_string()),
        };
    };

    let exists = path.exists();
    let mode_octal = file_mode_octal(&path);

    if !exists {
        return AuthStorageStatus {
            path,
            exists: false,
            mode_octal: None,
            provider_count: None,
            error: None,
        };
    }

    let storage = AuthStorage::at(&path);
    match storage.load() {
        Ok(records) => AuthStorageStatus {
            path,
            exists: true,
            mode_octal,
            provider_count: Some(records.len()),
            error: None,
        },
        Err(e) => AuthStorageStatus {
            path,
            exists: true,
            mode_octal,
            provider_count: None,
            error: Some(e.to_string()),
        },
    }
}

fn inspect_settings(cwd: &std::path::Path) -> SettingsLayerSummary {
    match SettingsManager::from_cwd(cwd) {
        Ok(mgr) => {
            let global_path = mgr.global_path().map(|p| p.to_path_buf());
            let project_path = mgr.project_path().map(|p| p.to_path_buf());
            let global_exists = global_path.as_deref().map(|p| p.exists()).unwrap_or(false);
            let project_exists = project_path.as_deref().map(|p| p.exists()).unwrap_or(false);
            let settings_yaml = serde_yaml::to_string(mgr.current()).unwrap_or_default();
            SettingsLayerSummary {
                global_path,
                global_exists,
                project_path,
                project_exists,
                settings_yaml,
                error: None,
            }
        }
        Err(e) => SettingsLayerSummary {
            global_path: None,
            global_exists: false,
            project_path: None,
            project_exists: false,
            settings_yaml: String::new(),
            error: Some(e.to_string()),
        },
    }
}

fn inspect_telemetry(cwd: &std::path::Path, settings: &SettingsLayerSummary) -> TelemetryStatus {
    let env_value = std::env::var("HAND_TELEMETRY").ok();

    // If settings failed to load, we can't faithfully resolve the gate.
    // Fall back to `default = true` (TS-faithful) and report that the
    // settings layer is unavailable.
    if settings.error.is_some() {
        let enabled = env_value.as_deref().map(is_truthy_env_flag).unwrap_or(true);
        let source: &'static str = if env_value.is_some() {
            "env"
        } else {
            "default"
        };
        return TelemetryStatus {
            enabled,
            source,
            env_value,
            error: Some("settings unavailable; telemetry resolved with defaults".to_string()),
        };
    }

    // Re-load a SettingsManager rather than threading one through:
    // settings.error.is_none() means the load succeeded once already, so
    // this second load is expected to also succeed; if it doesn't, fall
    // through to the same default path as above.
    let mgr = match SettingsManager::from_cwd(cwd) {
        Ok(m) => m,
        Err(e) => {
            return TelemetryStatus {
                enabled: env_value.as_deref().map(is_truthy_env_flag).unwrap_or(true),
                source: if env_value.is_some() {
                    "env"
                } else {
                    "default"
                },
                env_value,
                error: Some(e.to_string()),
            };
        }
    };

    let enabled = telemetry::is_install_telemetry_enabled(&mgr, env_value.as_deref());
    let source: &'static str = if env_value.is_some() {
        "env"
    } else if mgr.current().enable_install_telemetry.is_some() {
        "settings"
    } else {
        "default"
    };

    TelemetryStatus {
        enabled,
        source,
        env_value,
        error: None,
    }
}

fn inspect_timings() -> TimingsStatus {
    TimingsStatus {
        enabled: timings::enabled(),
        env_value: std::env::var("HAND_TIMING").ok(),
    }
}

fn inspect_skill_errors(cwd: &std::path::Path) -> Vec<SkillErrorSummary> {
    // User skills live under `~/.hand/skills/`. Builtin skills are not
    // bundled yet (Phase 2.x). Mirror `AgentSession::new`'s wiring: skip
    // user_dir when it doesn't exist so a missing global dir isn't
    // surfaced as an error.
    let user_dir = dirs::home_dir()
        .map(|h| h.join(".hand").join("skills"))
        .filter(|p| p.exists());
    let (_skills, errors) = skills::discover_skills(cwd, user_dir.as_deref(), None);

    errors
        .into_iter()
        .map(|e| SkillErrorSummary {
            path: skill_error_path(&e),
            message: e.to_string(),
        })
        .collect()
}

fn skill_error_path(err: &skills::SkillError) -> Option<PathBuf> {
    match err {
        skills::SkillError::Loader { path, .. }
        | skills::SkillError::MissingDescription { path }
        | skills::SkillError::DescriptionTooLong { path, .. }
        | skills::SkillError::NameMismatch { path, .. }
        | skills::SkillError::InvalidName { path, .. } => Some(path.clone()),
    }
}

fn file_mode_octal(path: &std::path::Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Truthy parsing matching `core::telemetry::is_truthy_env_flag` /
/// `core::timings::is_truthy_env_flag`. Duplicated here (rather than
/// re-exported) because the upstream helpers are private; the rule is
/// trivial and unlikely to drift.
fn is_truthy_env_flag(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if value == "1" {
        return true;
    }
    let lower = value.to_lowercase();
    lower == "true" || lower == "yes"
}

/// Render a [`DiagnosticsReport`] to stdout in section-by-section plain
/// text. Format is ad-hoc human-readable, matching the legacy
/// `Display for DiagnosticsReport` style for the existing checks and
/// adding labelled sections for the Phase-6 subsystems.
pub fn print_report(report: &DiagnosticsReport) {
    // Legacy checks block (re-uses `Display`).
    print!("{}", report);

    println!();
    println!("Auth Storage");
    println!("------------");
    println!("  path: {}", report.auth_storage.path.display());
    println!("  exists: {}", report.auth_storage.exists);
    match report.auth_storage.mode_octal {
        Some(m) => println!("  mode: {:o}", m),
        None => println!("  mode: <n/a>"),
    }
    match report.auth_storage.provider_count {
        Some(n) => println!("  providers: {}", n),
        None => println!("  providers: <unknown>"),
    }
    if let Some(err) = &report.auth_storage.error {
        println!("  error: {}", err);
    }

    println!();
    println!("Telemetry");
    println!("---------");
    println!("  enabled: {}", report.telemetry.enabled);
    println!("  source: {}", report.telemetry.source);
    match &report.telemetry.env_value {
        Some(v) => println!("  HAND_TELEMETRY: {}", v),
        None => println!("  HAND_TELEMETRY: <unset>"),
    }
    if let Some(err) = &report.telemetry.error {
        println!("  error: {}", err);
    }

    println!();
    println!("Timings");
    println!("-------");
    println!("  enabled: {}", report.timings.enabled);
    match &report.timings.env_value {
        Some(v) => println!("  HAND_TIMING: {}", v),
        None => println!("  HAND_TIMING: <unset>"),
    }

    println!();
    println!("Skill Discovery");
    println!("---------------");
    println!("  errors: {}", report.skill_errors.len());
    for e in &report.skill_errors {
        println!("    - {}", e.message);
    }

    println!();
    println!("Settings");
    println!("--------");
    match &report.settings.global_path {
        Some(p) => println!(
            "  global: {} ({})",
            p.display(),
            if report.settings.global_exists {
                "exists"
            } else {
                "absent"
            }
        ),
        None => println!("  global: <none>"),
    }
    match &report.settings.project_path {
        Some(p) => println!(
            "  project: {} ({})",
            p.display(),
            if report.settings.project_exists {
                "exists"
            } else {
                "absent"
            }
        ),
        None => println!("  project: <none>"),
    }
    if let Some(err) = &report.settings.error {
        println!("  error: {}", err);
    } else {
        println!("  resolved YAML:");
        for line in report.settings.settings_yaml.lines() {
            println!("    {}", line);
        }
    }
}

fn check_os() -> DiagCheck {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    DiagCheck {
        name: "OS".to_string(),
        status: DiagStatus::Ok,
        value: Some(format!("{os}/{arch}")),
    }
}

fn check_shell() -> DiagCheck {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    DiagCheck {
        name: "Shell".to_string(),
        status: if shell == "unknown" {
            DiagStatus::Warn("SHELL not set".to_string())
        } else {
            DiagStatus::Ok
        },
        value: Some(shell),
    }
}

fn check_terminal() -> DiagCheck {
    let term = std::env::var("TERM").unwrap_or_else(|_| "unknown".to_string());
    DiagCheck {
        name: "Terminal".to_string(),
        status: if term == "unknown" || term == "dumb" {
            DiagStatus::Warn("Limited terminal".to_string())
        } else {
            DiagStatus::Ok
        },
        value: Some(term),
    }
}

fn check_terminal_size() -> DiagCheck {
    // Try to get terminal size
    let (cols, rows) = terminal_size();
    if cols > 0 && rows > 0 {
        DiagCheck {
            name: "Terminal Size".to_string(),
            status: if cols < 60 {
                DiagStatus::Warn("Terminal width < 60 columns".to_string())
            } else {
                DiagStatus::Ok
            },
            value: Some(format!("{cols}x{rows}")),
        }
    } else {
        DiagCheck {
            name: "Terminal Size".to_string(),
            status: DiagStatus::Warn("Could not detect".to_string()),
            value: None,
        }
    }
}

fn terminal_size() -> (u16, u16) {
    // Try tput
    if let Ok(output) = Command::new("tput").arg("cols").output()
        && let Ok(cols) = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>()
        && let Ok(output) = Command::new("tput").arg("lines").output()
        && let Ok(rows) = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>()
    {
        return (cols, rows);
    }
    (0, 0)
}

fn check_command(name: &str, args: &[&str]) -> DiagCheck {
    match Command::new(name).args(args).output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            DiagCheck {
                name: name.to_string(),
                status: DiagStatus::Ok,
                value: Some(version),
            }
        }
        Err(_) => DiagCheck {
            name: name.to_string(),
            status: DiagStatus::Error(format!("{name} not found")),
            value: None,
        },
    }
}

fn check_api_key(env_var: &str, provider: &str) -> DiagCheck {
    // Never disclose any portion of the key — users routinely paste
    // diagnostics output into issues and chat threads. Report presence
    // and source only.
    match std::env::var(env_var) {
        Ok(val) if !val.is_empty() => DiagCheck {
            name: format!("{provider} API Key"),
            status: DiagStatus::Ok,
            value: Some(format!("set (from ${env_var})")),
        },
        _ => DiagCheck {
            name: format!("{provider} API Key"),
            status: DiagStatus::Warn(format!("{env_var} not set")),
            value: None,
        },
    }
}

fn check_hand_directory() -> DiagCheck {
    let cwd = std::env::current_dir().unwrap_or_default();
    let hand_dir = cwd.join(".hand");

    if hand_dir.is_dir() {
        // Check if writable
        let test_file = hand_dir.join(".diag_test");
        match std::fs::write(&test_file, "test") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                DiagCheck {
                    name: ".hand directory".to_string(),
                    status: DiagStatus::Ok,
                    value: Some(hand_dir.display().to_string()),
                }
            }
            Err(e) => DiagCheck {
                name: ".hand directory".to_string(),
                status: DiagStatus::Error(format!("Not writable: {e}")),
                value: Some(hand_dir.display().to_string()),
            },
        }
    } else {
        DiagCheck {
            name: ".hand directory".to_string(),
            status: DiagStatus::Warn(
                "Does not exist (will be created on first session)".to_string(),
            ),
            value: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_diagnostics_returns_checks() {
        let report = run_diagnostics();
        assert!(!report.checks.is_empty());
    }

    #[test]
    fn report_display() {
        let report = run_diagnostics();
        let output = report.to_string();
        assert!(output.contains("System Diagnostics"));
        assert!(output.contains("Summary:"));
    }

    #[test]
    fn diag_status_display() {
        assert_eq!(DiagStatus::Ok.to_string(), "OK");
        assert!(DiagStatus::Warn("test".into()).to_string().contains("WARN"));
        assert!(
            DiagStatus::Error("fail".into())
                .to_string()
                .contains("ERROR")
        );
    }

    #[test]
    fn check_os_always_ok() {
        let check = check_os();
        assert_eq!(check.status, DiagStatus::Ok);
        assert!(check.value.is_some());
    }

    #[test]
    fn check_api_key_does_not_disclose_key_material() {
        // Use a sentinel env var unlikely to collide with anything real.
        let env_var = "HAND_TEST_DIAG_KEY_SECRET";
        let secret = "sk-abcd1234EFGH5678wxyz";
        // SAFETY: confined to this test; we restore on the way out.
        // Older test binaries treat env mutation as unsafe.
        unsafe {
            std::env::set_var(env_var, secret);
        }
        let check = check_api_key(env_var, "Sentinel");
        unsafe {
            std::env::remove_var(env_var);
        }
        let value = check.value.expect("value present when env is set");
        // No portion of the key may appear in the rendered value.
        for window_len in 3..=secret.len() {
            for window in secret
                .as_bytes()
                .windows(window_len)
                .filter_map(|w| std::str::from_utf8(w).ok())
            {
                assert!(
                    !value.contains(window),
                    "diagnostics leaked key fragment {window:?}: {value:?}"
                );
            }
        }
        // And we should still announce presence + source for usability.
        assert!(value.contains("set"));
        assert!(value.contains(env_var));
    }

    /// Build an otherwise-empty `DiagnosticsReport` so unit tests can
    /// exercise the count/`has_errors` helpers without booting all the
    /// real subsystems.
    fn empty_report(checks: Vec<DiagCheck>) -> DiagnosticsReport {
        DiagnosticsReport {
            checks,
            auth_storage: AuthStorageStatus {
                path: PathBuf::new(),
                exists: false,
                mode_octal: None,
                provider_count: None,
                error: None,
            },
            telemetry: TelemetryStatus {
                enabled: true,
                source: "default",
                env_value: None,
                error: None,
            },
            timings: TimingsStatus {
                enabled: false,
                env_value: None,
            },
            skill_errors: Vec::new(),
            settings: SettingsLayerSummary {
                global_path: None,
                global_exists: false,
                project_path: None,
                project_exists: false,
                settings_yaml: String::new(),
                error: None,
            },
        }
    }

    #[test]
    fn report_counts() {
        let report = empty_report(vec![
            DiagCheck {
                name: "a".into(),
                status: DiagStatus::Ok,
                value: None,
            },
            DiagCheck {
                name: "b".into(),
                status: DiagStatus::Warn("w".into()),
                value: None,
            },
            DiagCheck {
                name: "c".into(),
                status: DiagStatus::Error("e".into()),
                value: None,
            },
        ]);
        assert_eq!(report.ok_count(), 1);
        assert_eq!(report.warn_count(), 1);
        assert_eq!(report.error_count(), 1);
    }

    #[test]
    fn has_errors_flags_check_errors() {
        let r = empty_report(vec![DiagCheck {
            name: "git".into(),
            status: DiagStatus::Error("missing".into()),
            value: None,
        }]);
        assert!(r.has_errors());
    }

    #[test]
    fn has_errors_flags_subsystem_errors() {
        let mut r = empty_report(vec![]);
        r.auth_storage.error = Some("malformed".into());
        assert!(r.has_errors());

        let mut r = empty_report(vec![]);
        r.settings.error = Some("malformed yaml".into());
        assert!(r.has_errors());

        let mut r = empty_report(vec![]);
        r.telemetry.error = Some("settings unavailable".into());
        assert!(r.has_errors());
    }

    #[test]
    fn has_errors_skill_errors_alone_do_not_count() {
        let mut r = empty_report(vec![]);
        r.skill_errors.push(SkillErrorSummary {
            path: None,
            message: "bad SKILL.md".into(),
        });
        assert!(!r.has_errors());
    }

    #[test]
    fn report_includes_auth_storage_status_when_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let auth_path = dir.path().join("auth.json");
        let report = run_diagnostics_at(dir.path(), Some(auth_path.clone()));
        assert_eq!(report.auth_storage.path, auth_path);
        assert!(!report.auth_storage.exists);
        assert!(report.auth_storage.provider_count.is_none());
        assert!(report.auth_storage.error.is_none());
    }

    #[test]
    fn report_includes_auth_storage_status_when_file_exists() {
        use crate::core::auth_storage::{AuthRecord, AuthStorage};
        let dir = tempfile::TempDir::new().unwrap();
        let auth_path = dir.path().join("auth.json");
        let storage = AuthStorage::at(&auth_path);
        storage
            .set("openai", AuthRecord::api_key("sk-test"))
            .unwrap();

        let report = run_diagnostics_at(dir.path(), Some(auth_path.clone()));
        assert_eq!(report.auth_storage.path, auth_path);
        assert!(report.auth_storage.exists);
        assert_eq!(report.auth_storage.provider_count, Some(1));
        assert!(report.auth_storage.error.is_none());
        #[cfg(unix)]
        {
            assert_eq!(report.auth_storage.mode_octal, Some(0o600));
        }
    }

    #[test]
    fn report_includes_auth_storage_error_when_file_malformed() {
        let dir = tempfile::TempDir::new().unwrap();
        let auth_path = dir.path().join("auth.json");
        std::fs::write(&auth_path, "{ not json").unwrap();
        let report = run_diagnostics_at(dir.path(), Some(auth_path));
        assert!(report.auth_storage.exists);
        assert!(report.auth_storage.provider_count.is_none());
        assert!(report.auth_storage.error.is_some());
        assert!(report.has_errors());
    }

    #[test]
    fn report_includes_telemetry_gate_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = run_diagnostics_at(dir.path(), None);
        // `enabled` and `source` are always populated; the exact value
        // depends on the ambient HAND_TELEMETRY env which we do not mutate
        // here (env-var mutation is process-wide and racy across tests).
        assert!(["env", "settings", "default"].contains(&report.telemetry.source));
    }

    #[test]
    fn report_includes_timings_gate_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = run_diagnostics_at(dir.path(), None);
        // Same ambient-env caveat as above: just assert the field shape.
        assert_eq!(report.timings.enabled, crate::core::timings::enabled(),);
    }

    #[test]
    fn print_report_renders_known_sections() {
        // Build a fully synthetic report so the test is deterministic and
        // doesn't depend on the host's `~/.hand/` or env vars.
        let mut r = empty_report(vec![DiagCheck {
            name: "OS".into(),
            status: DiagStatus::Ok,
            value: Some("test/test".into()),
        }]);
        r.auth_storage.path = PathBuf::from("/tmp/auth.json");
        r.settings.settings_yaml = "compaction:\n  enabled: true\n".into();

        // `print_report` writes to stdout; capturing stdout in tests
        // requires extra plumbing. We assert via the report's `Display`
        // for the legacy block and trust the `print_report` body is
        // a thin formatting layer over the same fields. As a smoke
        // check, just ensure the call doesn't panic.
        print_report(&r);

        let display = r.to_string();
        assert!(display.contains("System Diagnostics"));
        assert!(display.contains("Summary:"));
    }
}
