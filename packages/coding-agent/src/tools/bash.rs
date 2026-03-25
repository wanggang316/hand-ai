//! Bash tool — execute shell commands.

use crate::core::bash_executor;
use hand_agent::types::{AgentTool, ToolExecuteFn, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Create the bash tool.
pub fn create_bash_tool(cwd: PathBuf) -> AgentTool {
    let execute: ToolExecuteFn = Box::new(move |_tool_call_id, args| {
        let cwd = cwd.clone();
        Box::pin(async move { execute_bash(&cwd, args).await })
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

async fn execute_bash(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ToolResult::error("Missing required parameter: command"),
    };

    let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    let options = bash_executor::BashExecutorOptions {
        timeout_secs: timeout,
        ..Default::default()
    };

    match bash_executor::execute_bash(command, cwd, "/bin/bash", options).await {
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

    #[tokio::test]
    async fn test_bash_echo() {
        let dir = TempDir::new().unwrap();
        let result =
            execute_bash(&dir.path().to_path_buf(), json!({"command": "echo hello"})).await;
        let text = get_text(&result);
        assert!(text.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(&dir.path().to_path_buf(), json!({"command": "exit 1"})).await;
        let text = get_text(&result);
        assert!(text.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_bash_missing_command() {
        let dir = TempDir::new().unwrap();
        let result = execute_bash(&dir.path().to_path_buf(), json!({})).await;
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter"));
    }
}
