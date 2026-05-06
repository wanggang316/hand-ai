//! Edit tool — find-and-replace edits with diff output.

use hand_agent::types::{AgentTool, ToolExecuteFn, ToolResult};
use serde_json::json;
use similar::TextDiff;
use std::path::{Path, PathBuf};

/// Create the edit tool.
pub fn create_edit_tool(cwd: PathBuf) -> AgentTool {
    let execute: ToolExecuteFn = Box::new(move |_tool_call_id, args| {
        let cwd = cwd.clone();
        Box::pin(async move { execute_edit(&cwd, args) })
    });

    AgentTool::new(
        "edit",
        "Edit a file by replacing an exact string match. The old_string must appear \
         exactly once in the file. Returns a unified diff of the changes.",
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. Default: false"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        }),
        "Edit",
        execute,
    )
}

fn execute_edit(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: file_path"),
    };
    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("Missing required parameter: old_string"),
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("Missing required parameter: new_string"),
    };
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = resolve_path(cwd, file_path);

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
    };

    // Check for old_string in content
    let match_count = content.matches(old_string).count();
    if match_count == 0 {
        return ToolResult::error(format!(
            "old_string not found in {}. Make sure it matches exactly.",
            path.display()
        ));
    }
    if match_count > 1 && !replace_all {
        return ToolResult::error(format!(
            "old_string found {} times in {}. Use replace_all: true to replace all, \
             or provide more context to make it unique.",
            match_count,
            path.display()
        ));
    }

    // Perform replacement
    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };

    // Generate diff
    let diff = generate_diff(&content, &new_content, &path.display().to_string());

    // Write file
    if let Err(e) = std::fs::write(&path, &new_content) {
        return ToolResult::error(format!("Failed to write file: {}", e));
    }

    ToolResult::text(diff)
}

/// Generate a unified diff between old and new content.
pub fn generate_diff(old: &str, new: &str, filename: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    output.push_str(&format!("--- a/{}\n+++ b/{}\n", filename, filename));

    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        output.push_str(&format!("{}", hunk));
    }

    if output.lines().count() <= 2 {
        output.push_str("(no changes)\n");
    }

    output
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
    fn test_edit_simple_replace() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust"
            }),
        );
        let text = get_text(&result);
        assert!(text.contains("-hello world"));
        assert!(text.contains("+hello rust"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello rust");
    }

    #[test]
    fn test_edit_not_found() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "nonexistent",
                "new_string": "foo"
            }),
        );
        let text = get_text(&result);
        assert!(text.contains("not found"));
    }

    #[test]
    fn test_edit_ambiguous() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "aaa bbb aaa").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "ccc"
            }),
        );
        let text = get_text(&result);
        assert!(text.contains("found 2 times"));
    }

    #[test]
    fn test_edit_replace_all() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "aaa bbb aaa").unwrap();

        let _result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "ccc",
                "replace_all": true
            }),
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ccc bbb ccc");
    }

    #[test]
    fn test_generate_diff() {
        let diff = generate_diff(
            "line1\nline2\nline3\n",
            "line1\nchanged\nline3\n",
            "test.txt",
        );
        assert!(diff.contains("-line2"));
        assert!(diff.contains("+changed"));
    }

    #[test]
    fn test_edit_multiline() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let _result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "    println!(\"hello\");",
                "new_string": "    println!(\"goodbye\");"
            }),
        );
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("goodbye"));
    }
}
