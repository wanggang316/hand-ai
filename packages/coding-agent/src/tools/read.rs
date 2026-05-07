//! Read tool — read file contents with line numbers.

use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Default max lines to read.
const DEFAULT_MAX_LINES: usize = 2000;

/// Create the read tool.
pub fn create_read_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "read",
        "Read the contents of a file. Returns lines with line numbers. \
         Supports offset and limit parameters for reading portions of large files.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based). Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read. Defaults to 2000."
                }
            },
            "required": ["path"]
        }),
        "Read",
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_read(&cwd, args) }
        },
    )
}

fn execute_read(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: path"),
    };

    let path = resolve_path(cwd, path_str);
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_LINES as u64) as usize;

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Apply offset (1-based) and limit
    let start = (offset.saturating_sub(1)).min(total_lines);
    let end = (start + limit).min(total_lines);

    let mut output = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        output.push_str(&format!("{:>6}→{}\n", line_num, line));
    }

    if end < total_lines {
        output.push_str(&format!(
            "\n[Showing lines {}-{} of {} total. Use offset/limit to read more.]",
            start + 1,
            end,
            total_lines
        ));
    }

    ToolResult::text(output)
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

    #[test]
    fn test_read_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let result = execute_read(dir.path(), json!({"path": file.to_str().unwrap()}));
        let text = get_text(&result);
        assert!(text.contains("line1"));
        assert!(text.contains("line2"));
        assert!(text.contains("line3"));
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let result = execute_read(
            dir.path(),
            json!({"path": file.to_str().unwrap(), "offset": 2, "limit": 2}),
        );
        let text = get_text(&result);
        assert!(text.contains("b"));
        assert!(text.contains("c"));
        assert!(!text.contains("     1→a"));
    }

    #[test]
    fn test_read_missing_file() {
        let dir = TempDir::new().unwrap();
        let result = execute_read(dir.path(), json!({"path": "/nonexistent/file.txt"}));
        let text = get_text(&result);
        assert!(text.contains("Failed to read"));
    }

    #[test]
    fn test_read_relative_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "world").unwrap();

        let result = execute_read(dir.path(), json!({"path": "hello.txt"}));
        let text = get_text(&result);
        assert!(text.contains("world"));
    }

    #[test]
    fn test_read_missing_path_param() {
        let dir = TempDir::new().unwrap();
        let result = execute_read(dir.path(), json!({}));
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter"));
    }
}
