//! Extension system types.
//!
//! Extensions are dynamic modules that can:
//! - Subscribe to agent lifecycle events
//! - Register LLM-callable tools
//! - Register commands, keyboard shortcuts, and CLI flags
//! - Interact with the user via UI primitives

use std::collections::HashMap;
use std::path::PathBuf;

/// Describes an extension's metadata and configuration.
#[derive(Debug, Clone)]
pub struct ExtensionManifest {
    /// Unique name for the extension.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Version string (semver).
    pub version: String,
    /// File path of the extension source.
    pub path: PathBuf,
}

/// Types of hooks an extension can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionHookType {
    // Session events
    SessionStart,
    SessionBeforeSwitch,
    SessionSwitch,
    SessionBeforeFork,
    SessionFork,
    SessionBeforeCompact,
    SessionCompact,
    SessionShutdown,
    SessionBeforeTree,
    SessionTree,
    SessionDirectory,

    // Agent events
    AgentStart,
    AgentEnd,
    Context,
    BeforeProviderRequest,
    BeforeAgentStart,

    // Message events
    MessageStart,
    MessageUpdate,
    MessageEnd,
    TurnStart,
    TurnEnd,

    // Tool events
    ToolCall,
    ToolResult,
    ToolExecutionStart,
    ToolExecutionUpdate,
    ToolExecutionEnd,

    // Resource events
    ResourcesDiscover,

    // Input events
    Input,
    UserBash,
    ModelSelect,
}

impl std::fmt::Display for ExtensionHookType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::SessionStart => "session_start",
            Self::SessionBeforeSwitch => "session_before_switch",
            Self::SessionSwitch => "session_switch",
            Self::SessionBeforeFork => "session_before_fork",
            Self::SessionFork => "session_fork",
            Self::SessionBeforeCompact => "session_before_compact",
            Self::SessionCompact => "session_compact",
            Self::SessionShutdown => "session_shutdown",
            Self::SessionBeforeTree => "session_before_tree",
            Self::SessionTree => "session_tree",
            Self::SessionDirectory => "session_directory",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::Context => "context",
            Self::BeforeProviderRequest => "before_provider_request",
            Self::BeforeAgentStart => "before_agent_start",
            Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update",
            Self::MessageEnd => "message_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update",
            Self::ToolExecutionEnd => "tool_execution_end",
            Self::ResourcesDiscover => "resources_discover",
            Self::Input => "input",
            Self::UserBash => "user_bash",
            Self::ModelSelect => "model_select",
        };
        write!(f, "{name}")
    }
}

/// Configuration for loading extensions.
#[derive(Debug, Clone, Default)]
pub struct ExtensionConfig {
    /// Paths to extension files or directories.
    pub paths: Vec<PathBuf>,
    /// Whether to auto-discover extensions from standard locations.
    pub auto_discover: bool,
    /// Global extensions directory (~/.hand/agent/extensions/).
    pub global_dir: Option<PathBuf>,
    /// Project-local extensions directory (.hand/extensions/).
    pub project_dir: Option<PathBuf>,
}

/// Context passed to extension event handlers.
#[derive(Debug, Clone)]
pub struct ExtensionHookContext {
    /// Current working directory.
    pub cwd: PathBuf,
    /// Whether UI is available.
    pub has_ui: bool,
    /// Current model identifier, if any.
    pub model_id: Option<String>,
    /// Whether the agent is currently streaming.
    pub is_idle: bool,
    /// Arbitrary key-value metadata.
    pub metadata: HashMap<String, String>,
}

impl ExtensionHookContext {
    /// Create a minimal context for the given working directory.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            has_ui: false,
            model_id: None,
            is_idle: true,
            metadata: HashMap::new(),
        }
    }
}

/// Result returned by extension hook handlers.
#[derive(Debug, Clone, Default)]
pub struct ExtensionHookResult {
    /// Whether the event was handled (consumed).
    pub handled: bool,
    /// Whether to cancel the operation (for `before_*` events).
    pub cancel: bool,
    /// Optional message to surface to the user.
    pub message: Option<String>,
    /// Arbitrary key-value data returned by the handler.
    pub data: HashMap<String, String>,
}

/// A registered extension command.
#[derive(Debug, Clone)]
pub struct RegisteredCommand {
    /// Command name (without leading slash).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Source extension path.
    pub source: PathBuf,
}

/// A registered extension tool definition.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    /// Tool name (used in LLM tool calls).
    pub name: String,
    /// Human-readable label for UI.
    pub label: String,
    /// Description for the LLM.
    pub description: String,
    /// Optional one-line snippet for the system prompt's "Available tools" section.
    pub prompt_snippet: Option<String>,
    /// Optional guideline bullets appended to the system prompt.
    pub prompt_guidelines: Vec<String>,
    /// JSON schema for parameters (as serde_json::Value).
    pub parameters_schema: serde_json::Value,
    /// Source extension path.
    pub source: PathBuf,
}

/// A keyboard shortcut registered by an extension.
#[derive(Debug, Clone)]
pub struct ExtensionShortcut {
    /// Key identifier (e.g., "ctrl+shift+p").
    pub key_id: String,
    /// Human-readable description.
    pub description: String,
    /// Source extension path.
    pub source: PathBuf,
}

/// A CLI flag registered by an extension.
#[derive(Debug, Clone)]
pub struct ExtensionFlag {
    /// Flag name (e.g., "verbose").
    pub name: String,
    /// Flag type.
    pub flag_type: ExtensionFlagType,
    /// Default value as string.
    pub default: Option<String>,
    /// Description shown in --help.
    pub description: String,
    /// Source extension path.
    pub source: PathBuf,
}

/// Type of a CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionFlagType {
    Bool,
    String,
    Number,
}

/// An error that occurred in an extension.
#[derive(Debug, Clone)]
pub struct ExtensionError {
    /// Path to the extension that errored.
    pub extension_path: PathBuf,
    /// Event type being processed when the error occurred.
    pub event: String,
    /// Error message.
    pub message: String,
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Extension error in {} (event: {}): {}",
            self.extension_path.display(),
            self.event,
            self.message
        )
    }
}

impl std::error::Error for ExtensionError {}

/// A loaded extension with all its registered items.
#[derive(Debug, Clone)]
pub struct Extension {
    /// Extension manifest.
    pub manifest: ExtensionManifest,
    /// Subscribed event types.
    pub subscriptions: Vec<ExtensionHookType>,
    /// Registered tools.
    pub tools: Vec<RegisteredTool>,
    /// Registered commands.
    pub commands: Vec<RegisteredCommand>,
    /// Registered shortcuts.
    pub shortcuts: Vec<ExtensionShortcut>,
    /// Registered flags.
    pub flags: Vec<ExtensionFlag>,
}

impl Extension {
    /// Create an extension with just a manifest and no registrations.
    pub fn new(manifest: ExtensionManifest) -> Self {
        Self {
            manifest,
            subscriptions: Vec::new(),
            tools: Vec::new(),
            commands: Vec::new(),
            shortcuts: Vec::new(),
            flags: Vec::new(),
        }
    }
}

/// Widget placement for extension UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidgetPlacement {
    #[default]
    AboveEditor,
    BelowEditor,
}

/// Discovery reason for resource events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryReason {
    Startup,
    Reload,
}

/// Result from a resources_discover event.
#[derive(Debug, Clone, Default)]
pub struct ResourcesDiscoverResult {
    pub skill_paths: Vec<PathBuf>,
    pub prompt_paths: Vec<PathBuf>,
    pub theme_paths: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_hook_type_display() {
        assert_eq!(ExtensionHookType::SessionStart.to_string(), "session_start");
        assert_eq!(ExtensionHookType::ToolCall.to_string(), "tool_call");
        assert_eq!(ExtensionHookType::AgentEnd.to_string(), "agent_end");
        assert_eq!(
            ExtensionHookType::BeforeProviderRequest.to_string(),
            "before_provider_request"
        );
    }

    #[test]
    fn extension_hook_type_count() {
        // Verify all variants are accounted for in Display
        let all_types = [
            ExtensionHookType::SessionStart,
            ExtensionHookType::SessionBeforeSwitch,
            ExtensionHookType::SessionSwitch,
            ExtensionHookType::SessionBeforeFork,
            ExtensionHookType::SessionFork,
            ExtensionHookType::SessionBeforeCompact,
            ExtensionHookType::SessionCompact,
            ExtensionHookType::SessionShutdown,
            ExtensionHookType::SessionBeforeTree,
            ExtensionHookType::SessionTree,
            ExtensionHookType::SessionDirectory,
            ExtensionHookType::AgentStart,
            ExtensionHookType::AgentEnd,
            ExtensionHookType::Context,
            ExtensionHookType::BeforeProviderRequest,
            ExtensionHookType::BeforeAgentStart,
            ExtensionHookType::MessageStart,
            ExtensionHookType::MessageUpdate,
            ExtensionHookType::MessageEnd,
            ExtensionHookType::TurnStart,
            ExtensionHookType::TurnEnd,
            ExtensionHookType::ToolCall,
            ExtensionHookType::ToolResult,
            ExtensionHookType::ToolExecutionStart,
            ExtensionHookType::ToolExecutionUpdate,
            ExtensionHookType::ToolExecutionEnd,
            ExtensionHookType::ResourcesDiscover,
            ExtensionHookType::Input,
            ExtensionHookType::UserBash,
            ExtensionHookType::ModelSelect,
        ];
        assert_eq!(all_types.len(), 30);
        for t in &all_types {
            assert!(!t.to_string().is_empty());
        }
    }

    #[test]
    fn extension_config_default() {
        let config = ExtensionConfig::default();
        assert!(config.paths.is_empty());
        assert!(!config.auto_discover);
    }

    #[test]
    fn extension_hook_context_new() {
        let ctx = ExtensionHookContext::new(PathBuf::from("/tmp/test"));
        assert_eq!(ctx.cwd, PathBuf::from("/tmp/test"));
        assert!(!ctx.has_ui);
        assert!(ctx.is_idle);
        assert!(ctx.model_id.is_none());
        assert!(ctx.metadata.is_empty());
    }

    #[test]
    fn extension_hook_result_default() {
        let result = ExtensionHookResult::default();
        assert!(!result.handled);
        assert!(!result.cancel);
        assert!(result.message.is_none());
        assert!(result.data.is_empty());
    }

    #[test]
    fn extension_error_display() {
        let err = ExtensionError {
            extension_path: PathBuf::from("/ext/test.rs"),
            event: "tool_call".to_string(),
            message: "something broke".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("/ext/test.rs"));
        assert!(s.contains("tool_call"));
        assert!(s.contains("something broke"));
    }

    #[test]
    fn extension_new_empty() {
        let manifest = ExtensionManifest {
            name: "test-ext".to_string(),
            description: "A test extension".to_string(),
            version: "0.1.0".to_string(),
            path: PathBuf::from("/ext/test.rs"),
        };
        let ext = Extension::new(manifest);
        assert_eq!(ext.manifest.name, "test-ext");
        assert!(ext.subscriptions.is_empty());
        assert!(ext.tools.is_empty());
        assert!(ext.commands.is_empty());
        assert!(ext.shortcuts.is_empty());
        assert!(ext.flags.is_empty());
    }

    #[test]
    fn registered_tool_fields() {
        let tool = RegisteredTool {
            name: "my_tool".to_string(),
            label: "My Tool".to_string(),
            description: "Does things".to_string(),
            prompt_snippet: Some("Use my_tool to do things".to_string()),
            prompt_guidelines: vec!["Always confirm before running".to_string()],
            parameters_schema: serde_json::json!({"type": "object"}),
            source: PathBuf::from("/ext/tools.rs"),
        };
        assert_eq!(tool.name, "my_tool");
        assert!(tool.prompt_snippet.is_some());
        assert_eq!(tool.prompt_guidelines.len(), 1);
    }

    #[test]
    fn extension_flag_type_equality() {
        assert_eq!(ExtensionFlagType::Bool, ExtensionFlagType::Bool);
        assert_ne!(ExtensionFlagType::String, ExtensionFlagType::Number);
    }

    #[test]
    fn widget_placement_default() {
        assert_eq!(WidgetPlacement::default(), WidgetPlacement::AboveEditor);
    }

    #[test]
    fn resources_discover_result_default() {
        let result = ResourcesDiscoverResult::default();
        assert!(result.skill_paths.is_empty());
        assert!(result.prompt_paths.is_empty());
        assert!(result.theme_paths.is_empty());
    }
}
