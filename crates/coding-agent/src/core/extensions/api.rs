//! Extension API: the public interface every extension talks to.
//!
//! Tier 1 extensions (compiled-in Rust crates) implement [`Extension`] directly.
//! Tier 2 extensions (subprocess) declare the same shape via `extension.toml`.
//!
//! See ADR-001 for the design.
//!
//! # Manifest schema policy
//!
//! `ExtensionManifest` and its nested types use `#[serde(deny_unknown_fields)]`
//! per ADR-001 risk R-EXT-3 (manifest schema drift). Unknown fields produce a
//! structured parse error rather than being silently ignored, so bumping the
//! schema is a documented event with a migration note.

use async_trait::async_trait;
use hand_agent::types::AgentTool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What an extension declares about itself.
///
/// For Tier 1 extensions, this is constructed in [`Extension::manifest`].
/// For Tier 2 extensions, it's parsed from `extension.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExtensionManifest {
    /// Stable identifier, used for routing and configuration.
    pub name: String,
    /// Display version (e.g., "0.1.0"). Informational; not enforced.
    pub version: String,
    /// One-line description shown in --diagnostics or extension listings.
    #[serde(default)]
    pub description: Option<String>,
    /// Capabilities this extension provides. Influences which hooks the host
    /// will dispatch to it.
    #[serde(default)]
    pub capabilities: ExtensionCapabilities,
    /// Tier 2 only: how to invoke the subprocess. Ignored for Tier 1.
    #[serde(default)]
    pub exec: Option<Vec<String>>,
    /// Tier 2 only: extra environment variables for the subprocess.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Slash commands declared by this extension. Tier 1 extensions usually
    /// override `Extension::slash_commands()` directly; Tier 2 extensions
    /// declare them here in `extension.toml`.
    #[serde(default)]
    pub slash_commands: Vec<SlashCommandSpec>,
    /// Custom AgentTools declared by this extension (Tier 2 only — Tier 1
    /// builds tools in code via `Extension::custom_tools()`).
    #[serde(default)]
    pub custom_tools: Vec<CustomToolSpec>,
}

/// Which extension hooks/contributions the extension provides.
///
/// All `false` by default. Set the booleans for what the extension implements
/// so the host can avoid round-tripping events the extension doesn't care about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExtensionCapabilities {
    #[serde(default)]
    pub before_tool_call: bool,
    #[serde(default)]
    pub after_tool_call: bool,
    #[serde(default)]
    pub on_user_message: bool,
    /// Whether this extension contributes slash commands.
    #[serde(default)]
    pub slash_commands: bool,
    /// Whether this extension contributes custom AgentTools.
    #[serde(default)]
    pub custom_tools: bool,
    /// Whether this extension contributes a custom ApiProvider.
    #[serde(default)]
    pub custom_provider: bool,
}

/// Fired before the agent loop executes a tool call. The extension may
/// continue, cancel, or replace the tool call's arguments.
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub call_id: String,
}

/// Fired after the agent loop executes a tool call. Read-only — the
/// extension cannot rewrite the result.
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub tool_name: String,
    pub call_id: String,
    pub success: bool,
    pub result: serde_json::Value,
}

/// What an extension's `on_before_tool_call` decides.
#[derive(Debug, Clone)]
pub enum HookDecision {
    /// Let the agent loop run the tool as-is.
    Continue,
    /// Block the tool call. The model sees an error result with this message.
    Cancel(String),
    /// Replace the tool's arguments with new JSON, then continue.
    Replace(serde_json::Value),
}

/// What a slash command extension declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlashCommandSpec {
    pub name: String,
    pub description: String,
    /// Optional usage hint shown in /help.
    #[serde(default)]
    pub usage: Option<String>,
}

/// Manifest declaration for a custom AgentTool contributed by a Tier 2
/// extension. Tier 1 extensions construct `AgentTool` values directly and
/// don't go through this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool parameters, encoded as a string. Parsed into
    /// `serde_json::Value` at extension load time; if the string is not valid
    /// JSON, the extension fails to load.
    pub schema: String,
}

/// Per-extension load-time and per-event context. Provided by the host.
///
/// Field set is intentionally minimal in v1; expand as Tier 1 examples
/// surface real needs (see Phase 3 fixture extensions).
#[derive(Clone, Debug)]
pub struct ExtensionContext {
    /// Working directory of the agent session.
    pub cwd: PathBuf,
    /// Identifier of the current session. Stable for a session's lifetime.
    pub session_id: String,
    /// This extension's private slot for persistent state, and nobody
    /// else's: `<data root>/<extension name>/data/`. The root is the host's
    /// data directory when it pinned one (`AgentSessionConfig::base_dir`),
    /// else `<cwd>/.hand/`. Created lazily — the host does not mkdir it
    /// until something is about to write there.
    pub data_dir: PathBuf,
}

/// Session-level inputs from which a per-extension [`ExtensionContext`] is
/// derived.
///
/// The context handed to a hook differs per extension in exactly one field
/// (`data_dir`), so the host holds one factory per session and stamps the
/// extension's identity in at dispatch time. This keeps two extensions from
/// sharing a directory — and silently clobbering each other's `state.json`.
#[derive(Clone, Debug)]
pub struct ExtensionContextFactory {
    cwd: PathBuf,
    session_id: String,
    data_root: PathBuf,
}

impl ExtensionContextFactory {
    /// `data_root` is the directory that holds every extension's private
    /// slot, i.e. `<base_dir or cwd/.hand>/extensions`.
    pub fn new(
        cwd: impl Into<PathBuf>,
        session_id: impl Into<String>,
        data_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            session_id: session_id.into(),
            data_root: data_root.into(),
        }
    }

    /// Working directory shared by every extension in this session.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Session identifier shared by every extension in this session.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Root under which per-extension data directories are allocated.
    pub fn data_root(&self) -> &std::path::Path {
        &self.data_root
    }

    /// Build the context for one extension. `name` comes from the
    /// extension's manifest and is sanitized into a single path segment, so
    /// a hand-written Tier 1 manifest cannot escape the data root.
    pub fn for_extension(&self, name: &str) -> ExtensionContext {
        ExtensionContext {
            cwd: self.cwd.clone(),
            session_id: self.session_id.clone(),
            data_dir: self.data_root.join(sanitize_segment(name)).join("data"),
        }
    }
}

/// Reduce an extension name to one safe path segment.
///
/// Tier 2 names are already validated to `[a-z0-9_-]+` by the manifest
/// loader; Tier 1 manifests are hand-written Rust and get no such check, so
/// anything outside that set (including `/`, `\`, and `..`) is folded to
/// `_`.
fn sanitize_segment(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() {
        "_unnamed".to_string()
    } else {
        mapped
    }
}

/// The Tier 1 extension trait.
///
/// Tier 1 extensions are Rust crates that depend on `hand-coding-agent` and
/// `impl Extension for MyExt { ... }`. They are registered at compile time
/// via cargo features (or the `inventory` crate; see [`super::registry`]).
///
/// Default trait method bodies are provided so extensions only override what
/// they actually use.
#[async_trait]
pub trait Extension: Send + Sync {
    /// What the extension declares about itself. Returned by reference so
    /// implementations can return a `&'static` slot for stability.
    fn manifest(&self) -> &ExtensionManifest;

    /// Called once when the session starts and the extension is registered.
    /// Use this for one-time setup (load config, open files, etc.).
    async fn on_load(&self, _cx: &ExtensionContext) -> Result<(), ExtensionError> {
        Ok(())
    }

    /// Called once when the session ends.
    async fn on_shutdown(&self, _cx: &ExtensionContext) -> Result<(), ExtensionError> {
        Ok(())
    }

    /// Called before each tool call. Default: no-op (Continue).
    async fn on_before_tool_call(
        &self,
        _cx: &ExtensionContext,
        _event: &ToolCallEvent,
    ) -> Result<HookDecision, ExtensionError> {
        Ok(HookDecision::Continue)
    }

    /// Called after each tool call. Default: no-op.
    async fn on_after_tool_call(
        &self,
        _cx: &ExtensionContext,
        _event: &ToolResultEvent,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }

    /// Slash commands this extension contributes. Default: none.
    fn slash_commands(&self) -> Vec<SlashCommandSpec> {
        Vec::new()
    }

    /// Custom AgentTools this extension contributes. Default: none.
    ///
    /// `cx` carries the live session metadata (cwd, session_id, the
    /// extension's persistent data directory) so subprocess extensions can
    /// stamp the same context into every RPC tool call dispatched from
    /// inside the agent loop. Tier 1 extensions can ignore the argument.
    fn custom_tools(&self, _cx: &ExtensionContext) -> Vec<AgentTool> {
        Vec::new()
    }

    /// Invoke an extension-contributed slash command. The default
    /// implementation returns an error so extensions only have to override
    /// it when they actually contribute commands.
    ///
    /// `name` is the command name without the leading `/`. `args` is the raw
    /// argument string after the command name.
    async fn handle_slash_command(
        &self,
        _cx: &ExtensionContext,
        name: &str,
        _args: &str,
    ) -> Result<String, ExtensionError> {
        Err(ExtensionError::Custom {
            name: self.manifest().name.clone(),
            message: format!("slash command {name} not implemented"),
        })
    }
}

/// Errors an extension can return. Failures from a single extension never
/// propagate as session-fatal; the host logs and continues.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("extension {name} failed: {message}")]
    Custom { name: String, message: String },
    #[error("manifest error in {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },
    #[error("Tier 2 RPC error in {extension}: {source}")]
    Rpc {
        extension: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Errors specific to parsing `extension.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid TOML: {0}")]
    InvalidToml(#[from] toml::de::Error),
    #[error("required field {field} missing")]
    MissingField { field: String },
    #[error("name {name:?} fails validation: {reason}")]
    InvalidName { name: String, reason: String },
    #[error("failed to read manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal Tier 1 extension that overrides only `manifest()` to verify
    /// every other trait method has a usable default.
    struct MinimalExtension {
        manifest: ExtensionManifest,
    }

    #[async_trait]
    impl Extension for MinimalExtension {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }
    }

    fn ctx() -> ExtensionContext {
        ExtensionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".to_string(),
            data_dir: PathBuf::from("/tmp/data"),
        }
    }

    #[test]
    fn hook_decision_variants_constructible() {
        let _continue = HookDecision::Continue;
        let _cancel = HookDecision::Cancel("nope".to_string());
        let _replace = HookDecision::Replace(serde_json::json!({"path": "/etc"}));
    }

    #[tokio::test]
    async fn default_extension_methods_are_no_ops() {
        let ext = MinimalExtension {
            manifest: ExtensionManifest {
                name: "minimal".to_string(),
                version: "0.1.0".to_string(),
                description: None,
                capabilities: ExtensionCapabilities::default(),
                exec: None,
                env: Default::default(),
                slash_commands: Vec::new(),
                custom_tools: Vec::new(),
            },
        };
        let cx = ctx();

        ext.on_load(&cx).await.expect("on_load default ok");
        ext.on_shutdown(&cx).await.expect("on_shutdown default ok");

        let call_event = ToolCallEvent {
            tool_name: "read".to_string(),
            arguments: serde_json::json!({}),
            call_id: "call-1".to_string(),
        };
        let decision = ext
            .on_before_tool_call(&cx, &call_event)
            .await
            .expect("default ok");
        assert!(matches!(decision, HookDecision::Continue));

        let result_event = ToolResultEvent {
            tool_name: "read".to_string(),
            call_id: "call-1".to_string(),
            success: true,
            result: serde_json::json!({}),
        };
        ext.on_after_tool_call(&cx, &result_event)
            .await
            .expect("default ok");

        assert!(ext.slash_commands().is_empty());
        assert!(ext.custom_tools(&cx).is_empty());
    }

    #[test]
    fn factory_gives_each_extension_its_own_data_dir() {
        let factory =
            ExtensionContextFactory::new("/work", "sess-1", PathBuf::from("/state/extensions"));
        let foo = factory.for_extension("foo");
        let bar = factory.for_extension("bar");

        assert_eq!(foo.data_dir, PathBuf::from("/state/extensions/foo/data"));
        assert_eq!(bar.data_dir, PathBuf::from("/state/extensions/bar/data"));
        assert_ne!(foo.data_dir, bar.data_dir);
        assert_eq!(foo.cwd, PathBuf::from("/work"));
        assert_eq!(foo.session_id, "sess-1");
    }

    #[test]
    fn factory_sanitizes_names_into_one_path_segment() {
        let factory =
            ExtensionContextFactory::new("/work", "sess-1", PathBuf::from("/state/extensions"));

        assert_eq!(
            factory.for_extension("../../etc").data_dir,
            PathBuf::from("/state/extensions/______etc/data")
        );
        assert_eq!(
            factory.for_extension("").data_dir,
            PathBuf::from("/state/extensions/_unnamed/data")
        );
    }

    #[test]
    fn capabilities_default_all_false() {
        let caps = ExtensionCapabilities::default();
        assert!(!caps.before_tool_call);
        assert!(!caps.after_tool_call);
        assert!(!caps.on_user_message);
        assert!(!caps.slash_commands);
        assert!(!caps.custom_tools);
        assert!(!caps.custom_provider);
    }
}
