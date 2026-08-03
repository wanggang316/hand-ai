//! Tier 2 extension manifest loader.
//!
//! Parses `extension.toml` files into [`ExtensionManifest`]. Used by Tier 2
//! (subprocess) extensions; Tier 1 extensions construct their manifest in
//! Rust and skip this module.
//!
//! Unknown fields are rejected per ADR-001 (R-EXT-3).

use super::api::{ExtensionManifest, ManifestError};
use std::path::Path;

/// Parse `extension.toml` from disk.
pub fn load_manifest(path: &Path) -> Result<ExtensionManifest, ManifestError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_manifest_str(&raw)
}

/// Parse a manifest string. Useful for tests and for callers that already
/// hold the file contents.
pub fn parse_manifest_str(raw: &str) -> Result<ExtensionManifest, ManifestError> {
    let manifest: ExtensionManifest = toml::from_str(raw)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Validate semantic constraints not expressible via serde.
fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), ManifestError> {
    if manifest.name.trim().is_empty() {
        return Err(ManifestError::MissingField {
            field: "name".to_string(),
        });
    }
    // Names must be lowercase kebab/snake-friendly identifiers so they round-
    // trip safely through filesystem paths and JSON-RPC routing keys.
    if !manifest
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(ManifestError::InvalidName {
            name: manifest.name.clone(),
            reason: "must be ascii lowercase alphanumeric with - or _".to_string(),
        });
    }
    if manifest.version.trim().is_empty() {
        return Err(ManifestError::MissingField {
            field: "version".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest_with_default_capabilities() {
        let raw = r#"
name = "foo"
version = "0.1"
"#;
        let manifest = parse_manifest_str(raw).expect("minimal manifest parses");
        assert_eq!(manifest.name, "foo");
        assert_eq!(manifest.version, "0.1");
        assert!(manifest.description.is_none());
        assert!(manifest.exec.is_none());
        assert!(manifest.env.is_empty());
        assert!(!manifest.capabilities.before_tool_call);
        assert!(!manifest.capabilities.slash_commands);
    }

    #[test]
    fn timeouts_default_when_absent() {
        let raw = r#"
name = "foo"
version = "0.1"
"#;
        let manifest = parse_manifest_str(raw).expect("manifest parses");
        assert_eq!(manifest.timeouts.before_tool_call_ms, 5_000);
        assert_eq!(manifest.timeouts.after_tool_call_ms, 2_000);
        assert_eq!(
            manifest.timeouts.on_before_tool_call_timeout,
            crate::core::extensions::api::TimeoutPolicy::Cancel,
            "a blocking hook that stops answering must fail closed by default"
        );
    }

    #[test]
    fn timeouts_table_overrides_per_hook_budgets() {
        let raw = r#"
name = "foo"
version = "0.1"

[timeouts]
before-tool-call-ms = 250
on-before-tool-call-timeout = "continue"
"#;
        let manifest = parse_manifest_str(raw).expect("manifest parses");
        assert_eq!(manifest.timeouts.before_tool_call_ms, 250);
        // Unspecified budgets keep their defaults.
        assert_eq!(manifest.timeouts.after_tool_call_ms, 2_000);
        assert_eq!(
            manifest.timeouts.on_before_tool_call_timeout,
            crate::core::extensions::api::TimeoutPolicy::Continue
        );
    }

    #[test]
    fn parses_manifest_with_all_fields() {
        let raw = r#"
name = "kitchen-sink"
version = "1.2.3"
description = "Does it all"
exec = ["python3", "main.py"]

[env]
LOG_LEVEL = "debug"
API_KEY = "PI_API_KEY"

[capabilities]
before-tool-call = true
after-tool-call = true
on-user-message = true
slash-commands = true
custom-tools = true
custom-provider = true
"#;
        let manifest = parse_manifest_str(raw).expect("full manifest parses");
        assert_eq!(manifest.name, "kitchen-sink");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.description.as_deref(), Some("Does it all"));
        assert_eq!(
            manifest.exec.as_deref(),
            Some(&["python3".to_string(), "main.py".to_string()][..])
        );
        assert_eq!(
            manifest.env.get("LOG_LEVEL").map(String::as_str),
            Some("debug")
        );
        assert_eq!(
            manifest.env.get("API_KEY").map(String::as_str),
            Some("PI_API_KEY")
        );
        assert!(manifest.capabilities.before_tool_call);
        assert!(manifest.capabilities.after_tool_call);
        assert!(manifest.capabilities.on_user_message);
        assert!(manifest.capabilities.slash_commands);
        assert!(manifest.capabilities.custom_tools);
        assert!(manifest.capabilities.custom_provider);
    }

    #[test]
    fn capability_kebab_case_parses_to_snake_case_field() {
        let raw = r#"
name = "hooked"
version = "0.1"

[capabilities]
before-tool-call = true
"#;
        let manifest = parse_manifest_str(raw).expect("kebab capability parses");
        assert!(manifest.capabilities.before_tool_call);
    }

    #[test]
    fn missing_required_name_is_an_error() {
        let raw = r#"
version = "0.1"
"#;
        let err = parse_manifest_str(raw).expect_err("missing name should error");
        // serde reports this as InvalidToml since `name` has no default; the
        // TOML deserializer surfaces the missing field through its own error.
        match err {
            ManifestError::InvalidToml(_) => {}
            other => panic!("expected InvalidToml for missing name, got {other:?}"),
        }
    }

    #[test]
    fn empty_name_is_a_missing_field_error() {
        let raw = r#"
name = ""
version = "0.1"
"#;
        let err = parse_manifest_str(raw).expect_err("empty name should error");
        match err {
            ManifestError::MissingField { field } => assert_eq!(field, "name"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn invalid_toml_syntax_is_an_error() {
        let raw = "this is not = valid = toml = at all [";
        let err = parse_manifest_str(raw).expect_err("garbage should not parse");
        assert!(matches!(err, ManifestError::InvalidToml(_)));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let raw = r#"
name = "future"
version = "0.1"
mystery_field = "from-tomorrow"
"#;
        let err = parse_manifest_str(raw).expect_err("unknown field should reject");
        assert!(matches!(err, ManifestError::InvalidToml(_)));
    }

    #[test]
    fn invalid_name_is_an_error() {
        let raw = r#"
name = "Has Spaces"
version = "0.1"
"#;
        let err = parse_manifest_str(raw).expect_err("invalid name should error");
        match err {
            ManifestError::InvalidName { name, .. } => assert_eq!(name, "Has Spaces"),
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    /// T3.5: a manifest carrying `[[slash_commands]]` and `[[custom_tools]]`
    /// round-trips through the TOML deserializer and surfaces both as
    /// populated `Vec` fields. Schemas are kept as raw strings; semantic
    /// JSON validation happens at `SubprocessExtension::new` time.
    #[test]
    fn parses_manifest_with_slash_commands_and_custom_tools() {
        let raw = r#"
name = "fixture"
version = "0.1.0"

[[slash-commands]]
name = "review"
description = "Run code review"
usage = "/review [file]"

[[slash-commands]]
name = "ping"
description = "Ping the extension"

[[custom-tools]]
name = "rust_check"
description = "Run cargo check on the project"
schema = """
{ "type": "object", "properties": { "package": { "type": "string" } } }
"""
"#;
        let manifest = parse_manifest_str(raw).expect("manifest parses");
        assert_eq!(manifest.slash_commands.len(), 2);
        assert_eq!(manifest.slash_commands[0].name, "review");
        assert_eq!(
            manifest.slash_commands[0].usage.as_deref(),
            Some("/review [file]")
        );
        assert_eq!(manifest.slash_commands[1].name, "ping");
        assert!(manifest.slash_commands[1].usage.is_none());
        assert_eq!(manifest.custom_tools.len(), 1);
        assert_eq!(manifest.custom_tools[0].name, "rust_check");
        assert!(manifest.custom_tools[0].schema.contains("\"package\""));
    }

    #[test]
    fn loads_manifest_from_disk() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("extension.toml");
        std::fs::write(
            &path,
            r#"
name = "disk-loaded"
version = "0.1"
"#,
        )
        .unwrap();
        let manifest = load_manifest(&path).expect("loads from disk");
        assert_eq!(manifest.name, "disk-loaded");
    }
}
