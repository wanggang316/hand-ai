//! Write tool — create or overwrite files.

use crate::tools::file_mutation_queue::with_file_mutation_queue;
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

    let path = resolve_path(cwd, path_str);

    // Create parent directories
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ToolResult::error(format!("Failed to create directories: {}", e));
    }

    let line_count = content.lines().count();
    let content = content.to_string();
    let path_for_async = path.clone();
    // Pi-mono parity: serialise mutations against the same file so that
    // parallel tool_use blocks targeting one path don't race.
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

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() { p } else { cwd.join(p) }
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
}
