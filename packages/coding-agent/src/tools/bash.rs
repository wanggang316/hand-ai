//! Bash tool — execute shell commands.

use crate::core::bash_executor;
use hand_agent::types::{AgentTool, ToolExecuteFn, ToolExecutionContext, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Configuration for the bash tool.
///
/// `shell_path` selects the interpreter used to evaluate `bash` tool calls.
/// Defaults to `/bin/bash`; settings can override via `shell-path`.
#[derive(Debug, Clone)]
pub struct BashToolConfig {
    pub shell_path: PathBuf,
}

impl Default for BashToolConfig {
    fn default() -> Self {
        Self {
            shell_path: PathBuf::from("/bin/bash"),
        }
    }
}

/// Create the bash tool with the default shell (`/bin/bash`).
///
/// Convenience for callers that don't need to thread a `BashToolConfig`
/// through. New call sites should prefer
/// [`create_bash_tool_with_config`].
pub fn create_bash_tool(cwd: PathBuf) -> AgentTool {
    create_bash_tool_with_config(cwd, BashToolConfig::default())
}

/// Create the bash tool with an explicit shell path.
pub fn create_bash_tool_with_config(cwd: PathBuf, config: BashToolConfig) -> AgentTool {
    let shell_path = config.shell_path;
    let execute: ToolExecuteFn = Box::new(move |_tool_call_id, args, _cx: ToolExecutionContext| {
        let cwd = cwd.clone();
        let shell_path = shell_path.clone();
        Box::pin(async move { execute_bash(&cwd, &shell_path, args).await })
    });

    AgentTool::new(
        "bash",
        "Execute a bash command in the project working directory. \
         Returns the stdout/stderr output and exit code. \
         Use for running tests, builds, git commands, and other shell tasks.",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120)"
                }
            },
            "required": ["command"]
        }),
        "Bash",
        execute,
    )
}

async fn execute_bash(cwd: &Path, shell_path: &Path, args: serde_json::Value) -> ToolResult {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ToolResult::error("Missing required parameter: command"),
    };

    let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    let options = bash_executor::BashExecutorOptions {
        timeout_secs: timeout,
        ..Default::default()
    };

    let shell_str = shell_path.to_str().unwrap_or("/bin/bash");
    match bash_executor::execute_bash(command, cwd, shell_str, options).await {
        Ok(result) => {
            let mut output = result.output;
            if result.truncated {
                output.push_str("\n[Output truncated]");
            }
            if let Some(code) = result.exit_code
                && code != 0
            {
                output.push_str(&format!("\n[Exit code: {}]", code));
            }
            ToolResult::text(output)
        }
        Err(e) => ToolResult::error(format!("Bash execution failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn get_text(result: &ToolResult) -> &str {
        match &result.content[0] {
            model::ToolResultContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        }
    }

    fn default_shell() -> PathBuf {
        PathBuf::from("/bin/bash")
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let dir = TempDir::new().unwrap();
        let shell = default_shell();
        let result = execute_bash(
            dir.path(),
            &shell,
            json!({"command": "echo hello"}),
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let dir = TempDir::new().unwrap();
        let shell = default_shell();
        let result = execute_bash(
            dir.path(),
            &shell,
            json!({"command": "exit 1"}),
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_bash_missing_command() {
        let dir = TempDir::new().unwrap();
        let shell = default_shell();
        let result = execute_bash(dir.path(), &shell, json!({})).await;
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter"));
    }
}
