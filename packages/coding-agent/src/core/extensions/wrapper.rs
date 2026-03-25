//! Tool wrappers for extension-registered tools.
//!
//! These wrappers adapt extension-registered tool definitions so they can be
//! used as agent tools. Tool call and result interception is handled by the
//! agent session via hooks.

use super::runner::ExtensionRunner;
use super::types::RegisteredTool;

/// A wrapped extension tool ready for use by the agent.
#[derive(Debug, Clone)]
pub struct WrappedTool {
    /// Tool name.
    pub name: String,
    /// Tool label.
    pub label: String,
    /// Tool description.
    pub description: String,
    /// Parameter schema as JSON.
    pub parameters_schema: serde_json::Value,
    /// Source extension path display string.
    pub source: String,
}

/// Wrap a single registered tool into a form usable by the agent.
pub fn wrap_registered_tool(tool: &RegisteredTool, _runner: &ExtensionRunner) -> WrappedTool {
    WrappedTool {
        name: tool.name.clone(),
        label: tool.label.clone(),
        description: tool.description.clone(),
        parameters_schema: tool.parameters_schema.clone(),
        source: tool.source.display().to_string(),
    }
}

/// Wrap all registered tools from an extension runner.
pub fn wrap_registered_tools(runner: &ExtensionRunner) -> Vec<WrappedTool> {
    runner
        .all_tools()
        .into_iter()
        .map(|t| wrap_registered_tool(t, runner))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::{Extension, ExtensionManifest};
    use std::path::PathBuf;

    fn make_runner_with_tool() -> ExtensionRunner {
        let mut runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let mut ext = Extension::new(ExtensionManifest {
            name: "test".to_string(),
            description: "test ext".to_string(),
            version: "0.1.0".to_string(),
            path: PathBuf::from("/ext/test.rs"),
        });
        ext.tools.push(RegisteredTool {
            name: "search".to_string(),
            label: "Search".to_string(),
            description: "Search for files".to_string(),
            prompt_snippet: Some("Use search to find files".to_string()),
            prompt_guidelines: vec![],
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                }
            }),
            source: PathBuf::from("/ext/test.rs"),
        });
        runner.add_extension(ext);
        runner
    }

    #[test]
    fn wrap_single_tool() {
        let runner = make_runner_with_tool();
        let tools = runner.all_tools();
        let wrapped = wrap_registered_tool(tools[0], &runner);
        assert_eq!(wrapped.name, "search");
        assert_eq!(wrapped.label, "Search");
        assert_eq!(wrapped.description, "Search for files");
        assert!(wrapped.source.contains("test.rs"));
    }

    #[test]
    fn wrap_all_tools() {
        let runner = make_runner_with_tool();
        let wrapped = wrap_registered_tools(&runner);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].name, "search");
    }

    #[test]
    fn wrap_no_tools() {
        let runner = ExtensionRunner::new(PathBuf::from("/tmp"));
        let wrapped = wrap_registered_tools(&runner);
        assert!(wrapped.is_empty());
    }

    #[test]
    fn wrapped_tool_debug() {
        let runner = make_runner_with_tool();
        let wrapped = wrap_registered_tools(&runner);
        let debug = format!("{:?}", wrapped[0]);
        assert!(debug.contains("search"));
    }

    #[test]
    fn wrapped_tool_schema_preserved() {
        let runner = make_runner_with_tool();
        let wrapped = wrap_registered_tools(&runner);
        let schema = &wrapped[0].parameters_schema;
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }
}
