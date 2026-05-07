//! User-facing CLI hints for missing / invalid model credentials.
//!
//! Mirrors `pi-mono/packages/coding-agent/src/core/auth-guidance.ts`. The TS
//! file is a small set of formatting helpers used by the model-selection and
//! request-auth paths to build error messages that point the user at the
//! `/login` slash command and the bundled provider docs.
//!
//! ## Scope (matches TS exactly)
//!
//! The TS reference contains four pure functions and a single
//! `UNKNOWN_PROVIDER = "unknown"` sentinel. It does **not** branch on
//! provider id, OAuth-vs-API-key, expiry, or [`crate::core::auth_storage`]
//! state. This port preserves that responsibility 1:1; provider-specific
//! OAuth advice and credential-expiry checks belong elsewhere if/when they
//! are introduced upstream.
//!
//! ## Rebrand notes
//!
//! User-facing copy is held verbatim from TS. The TS reference resolves
//! `getDocsPath()` (the `docs/` directory bundled with the npm package) at
//! call time. Rust has no equivalent ambient package layout, so this port
//! takes `docs_path: &Path` as a pure parameter — the caller resolves the
//! path against whatever distribution layout is in play and passes it in.
//! That keeps the helpers deterministic for tests and avoids embedding a
//! filesystem assumption into the module.
//!
//! Apart from the `docs_path` parameterisation, the only deviation from TS
//! is the `pi` → `hand` rebrand where the binary name appears. The TS
//! reference does not actually mention the `pi` binary by name in this file
//! (it only references the `/login` and `/model` slash commands and the
//! docs paths), so in practice the on-disk strings are byte-identical to
//! TS.

use std::path::Path;

/// Sentinel provider id that means "we don't know which provider was
/// requested" — matches `UNKNOWN_PROVIDER` in the TS reference.
pub const UNKNOWN_PROVIDER: &str = "unknown";

/// Common 3-line login pointer used by every other helper.
///
/// TS reference: `getProviderLoginHelp()`. The two paths printed are
/// `<docs>/providers.md` and `<docs>/models.md`, joined with the platform
/// path separator (Rust's [`Path::join`] matches TS `node:path`'s `join` on
/// Unix; on Windows both use `\`).
pub fn provider_login_help(docs_path: &Path) -> String {
    let providers = docs_path.join("providers.md");
    let models = docs_path.join("models.md");
    format!(
        "Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        providers.display(),
        models.display(),
    )
}

/// Message shown when the model registry has zero usable models.
///
/// TS reference: `formatNoModelsAvailableMessage()`.
pub fn no_models_available_message(docs_path: &Path) -> String {
    format!("No models available. {}", provider_login_help(docs_path))
}

/// Message shown when no model has been selected for the session.
///
/// TS reference: `formatNoModelSelectedMessage()`.
pub fn no_model_selected_message(docs_path: &Path) -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        provider_login_help(docs_path),
    )
}

/// Message shown when a request is attempted but the selected model's
/// provider has no credential on file.
///
/// TS reference: `formatNoApiKeyFoundMessage(provider)`. If `provider`
/// equals [`UNKNOWN_PROVIDER`] the display name is replaced with
/// `"the selected model"`, otherwise the provider id is used verbatim.
pub fn no_api_key_found_message(provider: &str, docs_path: &Path) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {}.\n\n{}",
        provider_display,
        provider_login_help(docs_path),
    )
}

/// Convenience alias for [`no_api_key_found_message`].
///
/// The conversion brief (T-B2) names the canonical entry point
/// `guidance_for_missing_credentials`. This re-uses the same TS-faithful
/// implementation under that name. Returns `None` only for completeness
/// with the brief's signature; in practice the TS reference always
/// produces a string, so this always returns `Some`.
pub fn guidance_for_missing_credentials(
    provider: &str,
    docs_path: &Path,
) -> Option<String> {
    Some(no_api_key_found_message(provider, docs_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn docs() -> PathBuf {
        // Use a stable, platform-portable fixture path. We assert on
        // suffix membership rather than the full string so the tests
        // pass on both Unix and Windows path separators.
        PathBuf::from("/tmp/hand-docs")
    }

    #[test]
    fn provider_login_help_lists_providers_and_models_docs() {
        let s = provider_login_help(&docs());
        assert!(s.starts_with("Use /login to log into a provider via OAuth or API key. See:\n"));
        assert!(s.contains("providers.md"));
        assert!(s.contains("models.md"));
        // The two doc lines are indented with two spaces, matching TS.
        assert!(s.contains("\n  "));
    }

    #[test]
    fn no_models_available_prefixes_login_help() {
        let s = no_models_available_message(&docs());
        assert!(s.starts_with("No models available. "));
        assert!(s.contains("Use /login"));
        assert!(s.contains("providers.md"));
    }

    #[test]
    fn no_model_selected_includes_model_command_hint() {
        let s = no_model_selected_message(&docs());
        assert!(s.starts_with("No model selected.\n\n"));
        assert!(s.contains("Use /login"));
        assert!(s.ends_with("\n\nThen use /model to select a model."));
    }

    #[test]
    fn no_api_key_found_uses_provider_id_verbatim() {
        let s = no_api_key_found_message("anthropic", &docs());
        assert!(s.starts_with("No API key found for anthropic.\n\n"));
        assert!(s.contains("Use /login"));
    }

    #[test]
    fn no_api_key_found_substitutes_friendly_label_for_unknown() {
        let s = no_api_key_found_message(UNKNOWN_PROVIDER, &docs());
        assert!(s.starts_with("No API key found for the selected model.\n\n"));
        // The literal "unknown" should not leak into the user-facing copy.
        assert!(!s.contains("for unknown."));
    }

    #[test]
    fn no_api_key_found_does_not_match_unknown_case_insensitively() {
        // TS uses strict equality (`===`); a different-cased "Unknown" is
        // treated as a real provider id. Pin that.
        let s = no_api_key_found_message("Unknown", &docs());
        assert!(s.starts_with("No API key found for Unknown.\n\n"));
    }

    #[test]
    fn guidance_for_missing_credentials_matches_no_api_key_found() {
        let provider = "openai";
        let direct = no_api_key_found_message(provider, &docs());
        let aliased = guidance_for_missing_credentials(provider, &docs()).unwrap();
        assert_eq!(direct, aliased);
    }

    #[test]
    fn guidance_for_missing_credentials_handles_unknown_sentinel() {
        let s = guidance_for_missing_credentials(UNKNOWN_PROVIDER, &docs()).unwrap();
        assert!(s.contains("for the selected model."));
    }

    #[test]
    fn auth_storage_provider_keys_round_trip_through_guidance() {
        // Cross-tool wire compat: the provider id used as the key in
        // `auth.json` is what callers feed into the guidance helpers.
        // Construct an `AuthRecord` matching what TS would write, derive
        // the provider id the same way a caller would (the map key), and
        // confirm the rendered hint mentions it verbatim.
        use crate::core::auth_storage::AuthRecord;
        use std::collections::HashMap;

        let mut records: HashMap<String, AuthRecord> = HashMap::new();
        records.insert(
            "anthropic".to_string(),
            AuthRecord::oauth("a", "r", 1_700_000_000_000),
        );

        // Pretend the registry asked for "anthropic" and the lookup missed.
        let provider = records.keys().next().unwrap();
        let s = guidance_for_missing_credentials(provider, &docs()).unwrap();
        assert!(s.contains("anthropic"));
    }
}
