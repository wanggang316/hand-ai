//! Default constants for coding-agent settings.
//!
//! Today [`crate::core::settings::Settings::defaults`] leaves
//! `default_thinking_level` as `None` and the request-time logic falls
//! back to the model's intrinsic default. This module exposes the same
//! "medium" baseline that the project ships with so downstream code (and
//! a future `Settings::defaults` change) can opt into it without
//! re-deriving the value.
//!
//! Keeping this in a tiny dedicated module — rather than inlining it
//! into `settings.rs` — makes the migration to a non-`None`
//! `default_thinking_level` a single-line edit elsewhere.

use crate::core::settings::ThinkingLevelSetting;

/// Default reasoning effort applied when neither the user, the project
/// settings, nor the global settings select a thinking level.
///
/// Mirrors `DEFAULT_THINKING_LEVEL` in TS — the TS string `"medium"`
/// maps to [`ThinkingLevelSetting::Medium`] under the kebab-case serde
/// rename used by the settings schema.
pub const DEFAULT_THINKING_LEVEL: ThinkingLevelSetting = ThinkingLevelSetting::Medium;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thinking_level_is_medium() {
        // Pin the TS-matching default: changing this away from `Medium`
        // is a user-facing behaviour change and should be a deliberate
        // call, not a quiet refactor.
        assert_eq!(DEFAULT_THINKING_LEVEL, ThinkingLevelSetting::Medium);
    }

    #[test]
    fn default_thinking_level_serializes_to_kebab_case_medium() {
        // The settings schema uses kebab-case via serde; if this drifts
        // away from `"medium"` the on-disk YAML would break round-trip.
        let s = serde_yaml::to_string(&DEFAULT_THINKING_LEVEL).expect("yaml serialize");
        assert!(
            s.trim() == "medium",
            "expected `medium`, got {:?}",
            s.trim()
        );
    }
}
