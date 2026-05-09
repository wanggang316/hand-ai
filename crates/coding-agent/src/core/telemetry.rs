//! Install-telemetry gate. Mirrors `pi-mono/packages/coding-agent/src/core/telemetry.ts`.
//!
//! The single responsibility of this module is to answer one question:
//! "should the agent attach install-telemetry attribution headers when calling
//! upstream model providers?". The TS reference is a 14-line, single-function
//! file (`isInstallTelemetryEnabled`) and this module is its 1:1 port.
//!
//! Resolution order:
//!   1. `HAND_TELEMETRY` env var. If present, parsed as truthy
//!      (`"1"`, `"true"`, `"yes"` case-insensitive → on; anything else → off)
//!      and the settings layer is ignored.
//!   2. Otherwise, fall through to `SettingsManager::enable_install_telemetry()`,
//!      which defaults to `true` when unset (matches TS).
//!
//! ## Rebrand note
//!
//! The TS reference reads `PI_TELEMETRY`. This Rust port reads
//! `HAND_TELEMETRY` for project consistency with the rest of the rebrand
//! (binary name `hand`, settings dir `~/.hand/`, ...). The env-var name is
//! the only intentional deviation; truthy parsing and settings fall-through
//! are identical to TS.
//!
//! ## Out of scope (for this task)
//!
//! - Wiring this gate into the actual model-client attribution-header path
//!   (the TS consumer is `core/sdk.ts::getAttributionHeaders`). That lands
//!   in a separate task.
//! - Any per-event telemetry sink, per-turn instrumentation, or
//!   `events.jsonl`. None of that exists in the TS reference.

use crate::core::settings::SettingsManager;

/// Parse a string the same way the TS `isTruthyEnvFlag` helper does.
///
/// Returns `true` only for `"1"`, `"true"`, or `"yes"` (case-insensitive on
/// the latter two; `"1"` is matched literally). Empty strings and anything
/// else return `false`.
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

/// Resolve whether install-telemetry attribution headers are enabled.
///
/// - `env_override = Some(s)` → uses [`is_truthy_env_flag`] on `s`; the
///   settings layer is ignored.
/// - `env_override = None` → defers to
///   [`SettingsManager::enable_install_telemetry`] (default `true`).
///
/// Callers should pass `std::env::var("HAND_TELEMETRY").ok().as_deref()`.
/// Taking the env value as a parameter keeps this function pure and
/// deterministic in tests.
pub fn is_install_telemetry_enabled(
    settings: &SettingsManager,
    env_override: Option<&str>,
) -> bool {
    if let Some(raw) = env_override {
        return is_truthy_env_flag(raw);
    }
    settings.enable_install_telemetry()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::{Settings, SettingsManager};

    /// Build a `SettingsManager` whose merged settings have
    /// `enable_install_telemetry` set to `value`.
    fn manager_with_telemetry(value: Option<bool>) -> SettingsManager {
        let mut s = Settings::defaults();
        s.enable_install_telemetry = value;
        SettingsManager::from_raw_for_test(s)
    }

    #[test]
    fn env_override_truthy_wins_over_settings_false() {
        let mgr = manager_with_telemetry(Some(false));
        for raw in ["1", "true", "TRUE", "True", "yes", "YES", "Yes"] {
            assert!(
                is_install_telemetry_enabled(&mgr, Some(raw)),
                "expected truthy for {raw:?}",
            );
        }
    }

    #[test]
    fn env_override_falsy_wins_over_settings_true() {
        let mgr = manager_with_telemetry(Some(true));
        for raw in ["0", "false", "FALSE", "no", "NO", "off", "", "garbage"] {
            assert!(
                !is_install_telemetry_enabled(&mgr, Some(raw)),
                "expected falsy for {raw:?}",
            );
        }
    }

    #[test]
    fn env_override_none_falls_through_to_settings() {
        let mgr_off = manager_with_telemetry(Some(false));
        assert!(!is_install_telemetry_enabled(&mgr_off, None));

        let mgr_on = manager_with_telemetry(Some(true));
        assert!(is_install_telemetry_enabled(&mgr_on, None));
    }

    #[test]
    fn env_none_settings_none_defaults_true() {
        // Field-absent (the YAML didn't supply a value) must match the TS
        // default of `true`.
        let mgr = manager_with_telemetry(None);
        assert!(is_install_telemetry_enabled(&mgr, None));
    }
}
