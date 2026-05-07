//! Ls tool — list directory contents.

use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Default max entries.
const DEFAULT_MAX_ENTRIES: usize = 500;

/// Create the ls tool.
pub fn create_ls_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "ls",
        "List the contents of a directory. Shows file names, types (file/dir), \
         and sizes. Use this instead of running ls via bash.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to list (default: cwd)"
                },
                "max_entries": {
                    "type": "integer",
                    "description": "Maximum entries to show (default: 500)"
                }
            }
        }),
        "Ls",
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_ls(&cwd, args) }
        },
    )
}

fn execute_ls(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_path(cwd, p))
        .unwrap_or_else(|| cwd.to_path_buf());

    let max_entries = args
        .get("max_entries")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_ENTRIES as u64) as usize;

    let entries = match std::fs::read_dir(&path) {
        Ok(e) => e,
        Err(e) => return ToolResult::error(format!("Failed to read directory: {}", e)),
    };

    let mut items: Vec<(String, &str, u64)> = Vec::new();

    for entry in entries {
        if items.len() >= max_entries {
            break;
        }
        if let Ok(entry) = entry {
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().ok();
            let file_type = if metadata.as_ref().is_some_and(|m| m.is_dir()) {
                "dir"
            } else {
                "file"
            };
            let size = metadata.map(|m| m.len()).unwrap_or(0);
            items.push((name, file_type, size));
        }
    }

    // Sort: directories first, then by name
    items.sort_by(|a, b| {
        let type_order = |t: &str| if t == "dir" { 0 } else { 1 };
        type_order(a.1)
            .cmp(&type_order(b.1))
            .then_with(|| a.0.cmp(&b.0))
    });

    if items.is_empty() {
        return ToolResult::text(format!("{} (empty directory)", path.display()));
    }

    let mut output = String::new();
    for (name, file_type, size) in &items {
        if *file_type == "dir" {
            output.push_str(&format!("  {}/\n", name));
        } else {
            output.push_str(&format!("  {} ({})\n", name, format_size(*size)));
        }
    }

    if items.len() >= max_entries {
        output.push_str(&format!("[Truncated at {} entries]", max_entries));
    }

    ToolResult::text(output)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
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
    fn test_ls_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let result = execute_ls(&dir.path().to_path_buf(), json!({}));
        let text = get_text(&result);
        assert!(text.contains("subdir/"));
        assert!(text.contains("a.txt"));
    }

    #[test]
    fn test_ls_empty_dir() {
        let dir = TempDir::new().unwrap();
        let result = execute_ls(&dir.path().to_path_buf(), json!({}));
        let text = get_text(&result);
        assert!(text.contains("empty directory"));
    }

    #[test]
    fn test_ls_nonexistent() {
        let dir = TempDir::new().unwrap();
        let result = execute_ls(
            &dir.path().to_path_buf(),
            json!({"path": "/nonexistent_dir_xyz"}),
        );
        let text = get_text(&result);
        assert!(text.contains("Failed to read directory"));
    }

    #[test]
    fn test_ls_dirs_first() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("z_file.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("a_dir")).unwrap();

        let result = execute_ls(&dir.path().to_path_buf(), json!({}));
        let text = get_text(&result);
        let dir_pos = text.find("a_dir/").unwrap();
        let file_pos = text.find("z_file.txt").unwrap();
        assert!(dir_pos < file_pos, "Directories should come first");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1_048_576), "1.0 MB");
    }
}
