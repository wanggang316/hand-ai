//! Write tool — create or overwrite files.

use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_to_cwd;
use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Create the write tool.
pub fn create_write_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "write",
        "Write content to a file. Creates the file and any parent directories if they don't exist. \
         Overwrites the file if it already exists.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to write to"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        }),
        "Write",
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_write(&cwd, args).await }
        },
    )
}

async fn execute_write(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: path"),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ToolResult::error("Missing required parameter: content"),
    };

    let path = resolve_to_cwd(path_str, cwd);

    // Create parent directories
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ToolResult::error(format!("Failed to create directories: {}", e));
    }

    let line_count = content.lines().count();
    let content = content.to_string();
    let path_for_async = path.clone();
    // Serialise mutations against the same file so that parallel
    // tool_use blocks targeting one path don't race.
    with_file_mutation_queue(&path, async move {
        let existed = path_for_async.exists();
        match std::fs::write(&path_for_async, &content) {
            Ok(()) => {
                let action = if existed { "Updated" } else { "Created" };
                ToolResult::text(format!(
                    "{} {} ({} lines)",
                    action,
                    path_for_async.display(),
                    line_count
                ))
            }
            Err(e) => ToolResult::error(format!("Failed to write file: {}", e)),
        }
    })
    .await
}

/// Internal cross-module test surface. The edit-and-write mutation-queue
/// integration test in `tools::edit` needs to call into write's executor;
/// expose it under a hidden `__test_only` module so the path stays opt-in
/// and doesn't leak into the public API.
#[doc(hidden)]
pub mod __test_only {
    use super::execute_write;
    use hand_agent::types::ToolResult;
    use std::path::Path;

    pub async fn execute_write_for_test(cwd: &Path, args: serde_json::Value) -> ToolResult {
        execute_write(cwd, args).await
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
    async fn test_write_new_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("new.txt");

        let result = execute_write(
            dir.path(),
            json!({"path": file.to_str().unwrap(), "content": "hello\nworld"}),
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("Created"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\nworld");
    }

    #[tokio::test]
    async fn test_write_overwrite() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("exist.txt");
        std::fs::write(&file, "old").unwrap();

        let result = execute_write(
            dir.path(),
            json!({"path": file.to_str().unwrap(), "content": "new"}),
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("Updated"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
    }

    #[tokio::test]
    async fn test_write_creates_dirs() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a").join("b").join("c.txt");

        let result = execute_write(
            dir.path(),
            json!({"path": file.to_str().unwrap(), "content": "deep"}),
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("Created"));
        assert!(file.exists());
    }

    #[tokio::test]
    async fn test_write_missing_params() {
        let dir = TempDir::new().unwrap();
        let result = execute_write(dir.path(), json!({"path": "foo.txt"})).await;
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter: content"));
    }

    /// `~/...` paths must expand to $HOME on write too. Without it, a
    /// write to `~/output.txt` lands in `<cwd>/~/output.txt` (a literal
    /// tilde directory) — which silently succeeds and leaves the user
    /// wondering where the file went.
    #[tokio::test]
    async fn test_write_expands_tilde() {
        let dir = TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", dir.path());
        }

        let _result = execute_write(
            dir.path(),
            json!({"path": "~/written.txt", "content": "ok"}),
        )
        .await;

        let expected = dir.path().join("written.txt");
        let landed = expected.exists();

        if let Some(h) = original_home {
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }

        assert!(
            landed,
            "expected ~/written.txt to land at $HOME/written.txt"
        );
    }
}
