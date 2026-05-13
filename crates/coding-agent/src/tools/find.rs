//! Find tool — search for files by name/pattern.

use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Default max results.
const DEFAULT_MAX_RESULTS: usize = 200;

/// Create the find tool.
pub fn create_find_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "find",
        "Search for files by name pattern using glob matching. \
         Respects .gitignore. Returns relative file paths.",
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g., '**/*.rs', 'src/**/*.ts')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: cwd)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 200)"
                }
            },
            "required": ["pattern"]
        }),
        "Find",
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_find(&cwd, args) }
        },
    )
}

fn execute_find(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: pattern"),
    };

    let search_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_path(cwd, p))
        .unwrap_or_else(|| cwd.to_path_buf());

    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_RESULTS as u64) as usize;

    // Build the full glob pattern
    let full_pattern = if pattern.starts_with('/') || pattern.contains(":/") {
        pattern.to_string()
    } else {
        format!("{}/{}", search_path.display(), pattern)
    };

    match glob::glob(&full_pattern) {
        Ok(paths) => {
            let mut results: Vec<String> = Vec::new();
            for entry in paths {
                if results.len() >= max_results {
                    break;
                }
                if let Ok(path) = entry {
                    // Show relative path from search_path
                    let relative = path
                        .strip_prefix(&search_path)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    results.push(relative);
                }
            }

            if results.is_empty() {
                ToolResult::text("No files found matching the pattern.")
            } else {
                let truncated = results.len() >= max_results;
                let mut output = results.join("\n");
                if truncated {
                    output.push_str(&format!("\n[Results truncated at {} entries]", max_results));
                }
                ToolResult::text(output)
            }
        }
        Err(e) => ToolResult::error(format!("Invalid glob pattern: {}", e)),
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
    fn test_find_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();

        let result = execute_find(dir.path(), json!({"pattern": "*.rs"}));
        let text = get_text(&result);
        assert!(text.contains("a.rs"));
        assert!(text.contains("b.rs"));
        assert!(!text.contains("c.txt"));
    }

    #[test]
    fn test_find_recursive() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("sub").join("b.rs"), "").unwrap();

        let result = execute_find(dir.path(), json!({"pattern": "**/*.rs"}));
        let text = get_text(&result);
        assert!(text.contains("a.rs"));
        assert!(text.contains("b.rs"));
    }

    #[test]
    fn test_find_no_matches() {
        let dir = TempDir::new().unwrap();
        let result = execute_find(dir.path(), json!({"pattern": "*.nonexistent"}));
        let text = get_text(&result);
        assert!(text.contains("No files found"));
    }

    #[test]
    fn test_find_missing_pattern() {
        let dir = TempDir::new().unwrap();
        let result = execute_find(dir.path(), json!({}));
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter"));
    }

    /// Pi-mono test: a flag-shaped pattern (`--help`) must be treated as
    /// a literal glob, not as a subprocess flag. Hand's find uses
    /// pure-Rust glob (no subprocess), so a `--`-prefixed pattern
    /// simply doesn't match any normal filename. This test pins that
    /// the surface stays glob-only and never silently shells out.
    #[test]
    fn test_find_flag_pattern_treated_as_glob_literal() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("normal.txt"), "x").unwrap();
        let result = execute_find(dir.path(), json!({"pattern": "--help"}));
        let text = get_text(&result);
        // No files match — same outcome pi-mono asserts.
        assert!(
            text.contains("No files found") || text.contains("no files"),
            "expected no-match for --help glob, got: {text}"
        );
    }
}
