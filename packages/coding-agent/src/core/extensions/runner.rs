//! Extension runner — manages extension lifecycle and event emission.
//!
//! The runner holds loaded extensions, dispatches events to their handlers,
//! and collects registered tools, commands, shortcuts, and flags.

use std::collections::HashMap;
use std::path::PathBuf;

use super::types::{
    Extension, ExtensionError, ExtensionFlag, ExtensionHookContext, ExtensionHookResult,
    ExtensionHookType, ExtensionShortcut, RegisteredTool,
};

/// Callback for extension errors.
pub type ExtensionErrorListener = Box<dyn Fn(&ExtensionError) + Send + Sync>;

/// The extension runner manages loaded extensions and dispatches events.
pub struct ExtensionRunner {
    /// Loaded extensions.
    extensions: Vec<Extension>,
    /// Accumulated errors.
    errors: Vec<ExtensionError>,
    /// Error listener callback.
    error_listener: Option<ExtensionErrorListener>,
    /// Current working directory.
    cwd: PathBuf,
    /// Whether the runner has been initialized.
    initialized: bool,
}

impl std::fmt::Debug for ExtensionRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRunner")
            .field("extensions", &self.extensions.len())
            .field("errors", &self.errors.len())
            .field("cwd", &self.cwd)
            .field("initialized", &self.initialized)
            .finish()
    }
}

impl ExtensionRunner {
    /// Create a new runner with no extensions.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            extensions: Vec::new(),
            errors: Vec::new(),
            error_listener: None,
            cwd,
            initialized: false,
        }
    }

    /// Set the extensions to manage.
    pub fn set_extensions(&mut self, extensions: Vec<Extension>) {
        self.extensions = extensions;
    }

    /// Add a single extension.
    pub fn add_extension(&mut self, extension: Extension) {
        self.extensions.push(extension);
    }

    /// Set an error listener.
    pub fn set_error_listener(&mut self, listener: ExtensionErrorListener) {
        self.error_listener = Some(listener);
    }

    /// Get all loaded extensions.
    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    /// Get accumulated errors.
    pub fn errors(&self) -> &[ExtensionError] {
        &self.errors
    }

    /// Get the number of loaded extensions.
    pub fn extension_count(&self) -> usize {
        self.extensions.len()
    }

    /// Whether the runner has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Mark the runner as initialized.
    pub fn initialize(&mut self) {
        self.initialized = true;
    }

    /// Get all registered tools across all extensions.
    pub fn all_tools(&self) -> Vec<&RegisteredTool> {
        self.extensions
            .iter()
            .flat_map(|ext| ext.tools.iter())
            .collect()
    }

    /// Get all registered commands across all extensions.
    pub fn all_commands(&self) -> Vec<ResolvedCommand> {
        self.extensions
            .iter()
            .flat_map(|ext| {
                ext.commands.iter().map(|cmd| ResolvedCommand {
                    name: cmd.name.clone(),
                    description: cmd.description.clone(),
                    source: ext.manifest.path.clone(),
                })
            })
            .collect()
    }

    /// Get all registered shortcuts across all extensions.
    pub fn all_shortcuts(&self) -> Vec<&ExtensionShortcut> {
        self.extensions
            .iter()
            .flat_map(|ext| ext.shortcuts.iter())
            .collect()
    }

    /// Get all registered flags across all extensions.
    pub fn all_flags(&self) -> Vec<&ExtensionFlag> {
        self.extensions
            .iter()
            .flat_map(|ext| ext.flags.iter())
            .collect()
    }

    /// Create a context for event handlers.
    pub fn create_context(&self) -> ExtensionHookContext {
        ExtensionHookContext::new(self.cwd.clone())
    }

    /// Emit an event to all extensions that subscribe to it.
    ///
    /// Returns collected results from all handlers.
    pub fn emit(
        &mut self,
        event_type: ExtensionHookType,
        _context: &ExtensionHookContext,
    ) -> Vec<ExtensionHookResult> {
        let mut results = Vec::new();

        for ext in &self.extensions {
            if !ext.subscriptions.contains(&event_type) {
                continue;
            }

            // In a full implementation, this would invoke the extension's
            // handler for the event type. For now, we just record that
            // the event was dispatched.
            results.push(ExtensionHookResult::default());
        }

        results
    }

    /// Emit an event and check if any handler cancelled it.
    pub fn emit_cancellable(
        &mut self,
        event_type: ExtensionHookType,
        context: &ExtensionHookContext,
    ) -> bool {
        let results = self.emit(event_type, context);
        results.iter().any(|r| r.cancel)
    }

    /// Record an extension error.
    pub fn record_error(&mut self, error: ExtensionError) {
        if let Some(listener) = &self.error_listener {
            listener(&error);
        }
        self.errors.push(error);
    }

    /// Clear accumulated errors.
    pub fn clear_errors(&mut self) {
        self.errors.clear();
    }

    /// Get a summary of loaded extensions for diagnostics.
    pub fn diagnostics_summary(&self) -> HashMap<String, ExtensionDiagnostic> {
        self.extensions
            .iter()
            .map(|ext| {
                (
                    ext.manifest.name.clone(),
                    ExtensionDiagnostic {
                        path: ext.manifest.path.clone(),
                        version: ext.manifest.version.clone(),
                        tools: ext.tools.len(),
                        commands: ext.commands.len(),
                        shortcuts: ext.shortcuts.len(),
                        subscriptions: ext.subscriptions.len(),
                    },
                )
            })
            .collect()
    }
}

/// A command resolved with its source extension.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    /// Command name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Source extension path.
    pub source: PathBuf,
}

/// Diagnostic information about a loaded extension.
#[derive(Debug, Clone)]
pub struct ExtensionDiagnostic {
    /// Extension file path.
    pub path: PathBuf,
    /// Version.
    pub version: String,
    /// Number of registered tools.
    pub tools: usize,
    /// Number of registered commands.
    pub commands: usize,
    /// Number of registered shortcuts.
    pub shortcuts: usize,
    /// Number of event subscriptions.
    pub subscriptions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::{ExtensionManifest, RegisteredCommand};

    fn test_manifest(name: &str) -> ExtensionManifest {
        ExtensionManifest {
            name: name.to_string(),
            description: format!("Test extension {name}"),
            version: "0.1.0".to_string(),
            path: PathBuf::from(format!("/ext/{name}.rs")),
        }
    }

    fn test_extension(name: &str) -> Extension {
        Extension::new(test_manifest(name))
    }

    #[test]
    fn runner_new() {
        let runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        assert_eq!(runner.extension_count(), 0);
        assert!(!runner.is_initialized());
        assert!(runner.errors().is_empty());
    }

    #[test]
    fn runner_add_extensions() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        runner.add_extension(test_extension("a"));
        runner.add_extension(test_extension("b"));
        assert_eq!(runner.extension_count(), 2);
    }

    #[test]
    fn runner_set_extensions() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        runner.add_extension(test_extension("old"));
        runner.set_extensions(vec![test_extension("new1"), test_extension("new2")]);
        assert_eq!(runner.extension_count(), 2);
        assert_eq!(runner.extensions()[0].manifest.name, "new1");
    }

    #[test]
    fn runner_initialize() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        assert!(!runner.is_initialized());
        runner.initialize();
        assert!(runner.is_initialized());
    }

    #[test]
    fn runner_emit_no_subscribers() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        runner.add_extension(test_extension("ext1"));
        let ctx = runner.create_context();
        let results = runner.emit(ExtensionHookType::SessionStart, &ctx);
        assert!(results.is_empty()); // no subscriptions
    }

    #[test]
    fn runner_emit_with_subscriber() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let mut ext = test_extension("ext1");
        ext.subscriptions.push(ExtensionHookType::SessionStart);
        runner.add_extension(ext);

        let ctx = runner.create_context();
        let results = runner.emit(ExtensionHookType::SessionStart, &ctx);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn runner_emit_cancellable() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let ctx = runner.create_context();
        let cancelled = runner.emit_cancellable(ExtensionHookType::SessionBeforeSwitch, &ctx);
        assert!(!cancelled);
    }

    #[test]
    fn runner_record_error() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let error = ExtensionError {
            extension_path: PathBuf::from("/ext/bad.rs"),
            event: "load".to_string(),
            message: "parse error".to_string(),
        };
        runner.record_error(error);
        assert_eq!(runner.errors().len(), 1);

        runner.clear_errors();
        assert!(runner.errors().is_empty());
    }

    #[test]
    fn runner_error_listener() {
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        runner.set_error_listener(Box::new(move |err| {
            captured_clone.lock().unwrap().push(err.message.clone());
        }));

        runner.record_error(ExtensionError {
            extension_path: PathBuf::from("/ext/err.rs"),
            event: "test".to_string(),
            message: "test error".to_string(),
        });

        assert_eq!(captured.lock().unwrap().len(), 1);
        assert_eq!(captured.lock().unwrap()[0], "test error");
    }

    #[test]
    fn runner_all_tools() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let mut ext = test_extension("toolbox");
        ext.tools.push(RegisteredTool {
            name: "my_tool".to_string(),
            label: "My Tool".to_string(),
            description: "Does stuff".to_string(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters_schema: serde_json::json!({}),
            source: PathBuf::from("/ext/toolbox.rs"),
        });
        runner.add_extension(ext);

        let tools = runner.all_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "my_tool");
    }

    #[test]
    fn runner_all_commands() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let mut ext = test_extension("cmds");
        ext.commands.push(RegisteredCommand {
            name: "deploy".to_string(),
            description: "Deploy the app".to_string(),
            source: PathBuf::from("/ext/cmds.rs"),
        });
        runner.add_extension(ext);

        let cmds = runner.all_commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "deploy");
        assert_eq!(cmds[0].source, PathBuf::from("/ext/cmds.rs"));
    }

    #[test]
    fn runner_diagnostics_summary() {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let mut ext = test_extension("diag_ext");
        ext.subscriptions.push(ExtensionHookType::AgentStart);
        ext.subscriptions.push(ExtensionHookType::AgentEnd);
        runner.add_extension(ext);

        let summary = runner.diagnostics_summary();
        assert_eq!(summary.len(), 1);
        let diag = &summary["diag_ext"];
        assert_eq!(diag.subscriptions, 2);
        assert_eq!(diag.tools, 0);
    }

    #[test]
    fn runner_debug() {
        let runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let debug = format!("{runner:?}");
        assert!(debug.contains("ExtensionRunner"));
        assert!(debug.contains("extensions: 0"));
    }

    #[test]
    fn resolved_command_fields() {
        let cmd = ResolvedCommand {
            name: "test".to_string(),
            description: "A test command".to_string(),
            source: PathBuf::from("/ext/test.rs"),
        };
        assert_eq!(cmd.name, "test");
    }
}
