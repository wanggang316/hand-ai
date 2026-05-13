//! Grep tool — search file contents using regex patterns.

use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Default max matches.
const DEFAULT_MAX_MATCHES: usize = 100;

/// Create the grep tool.
pub fn create_grep_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "grep",
        "Search file contents using regex patterns. Uses ripgrep (rg) if available, \
         falls back to grep. Returns matching lines with file paths and line numbers.",
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in (default: cwd)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs', '*.ts')"
                },
                "context": {
                    "type": "integer",
                    "description": "Number of context lines before and after each match"
                },
                "max_matches": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default: 100)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search (default: false)"
                }
            },
            "required": ["pattern"]
        }),
        "Grep",
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_grep(&cwd, args) }
        },
    )
}

fn execute_grep(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: pattern"),
    };

    let search_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_path(cwd, p))
        .unwrap_or_else(|| cwd.to_path_buf());

    let include = args.get("include").and_then(|v| v.as_str());
    let context = args
        .get("context")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let max_matches = args
        .get("max_matches")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_MATCHES as u64) as usize;
    let case_insensitive = args
        .get("case_insensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Try ripgrep first, fall back to grep
    let result = try_ripgrep(
        pattern,
        &search_path,
        include,
        context,
        max_matches,
        case_insensitive,
    )
    .or_else(|| {
        try_grep(
            pattern,
            &search_path,
            include,
            context,
            max_matches,
            case_insensitive,
        )
    });

    match result {
        Some(output) => {
            if output.is_empty() {
                ToolResult::text("No matches found.")
            } else {
                ToolResult::text(output)
            }
        }
        None => ToolResult::error("Neither rg nor grep is available"),
    }
}

fn try_ripgrep(
    pattern: &str,
    search_path: &Path,
    include: Option<&str>,
    context: Option<usize>,
    max_matches: usize,
    case_insensitive: bool,
) -> Option<String> {
    let mut cmd = Command::new("rg");
    cmd.arg("--no-heading")
        .arg("--line-number")
        .arg("--max-count")
        .arg(max_matches.to_string());

    if case_insensitive {
        cmd.arg("--ignore-case");
    }
    if let Some(ctx) = context {
        cmd.arg("--context").arg(ctx.to_string());
    }
    if let Some(glob) = include {
        cmd.arg("--glob").arg(glob);
    }

    // Stop flag parsing before the user-controlled pattern. Without
    // `--`, a pattern like `--pre=/tmp/payload.sh` is interpreted by
    // ripgrep as the `--pre` preprocessor flag, which executes the
    // script for every searched file (an LLM-injection RCE). Pi-mono
    // has an explicit test for this.
    cmd.arg("--").arg(pattern).arg(search_path);

    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !stderr.is_empty() && stdout.is_empty() {
                // rg might not be installed
                if stderr.contains("not found") {
                    return None;
                }
            }
            Some(stdout)
        }
        Err(_) => None,
    }
}

fn try_grep(
    pattern: &str,
    search_path: &Path,
    include: Option<&str>,
    context: Option<usize>,
    max_matches: usize,
    case_insensitive: bool,
) -> Option<String> {
    let mut cmd = Command::new("grep");
    cmd.arg("-r")
        .arg("-n")
        .arg("--max-count")
        .arg(max_matches.to_string());

    if case_insensitive {
        cmd.arg("-i");
    }
    if let Some(ctx) = context {
        cmd.arg("-C").arg(ctx.to_string());
    }
    if let Some(glob) = include {
        cmd.arg("--include").arg(glob);
    }

    // Same flag-injection guard as the ripgrep branch above.
    cmd.arg("--").arg(pattern).arg(search_path);

    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            Some(stdout)
        }
        Err(_) => None,
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
    fn test_grep_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.txt"),
            "hello world\nfoo bar\nhello again",
        )
        .unwrap();

        let result = execute_grep(dir.path(), json!({"pattern": "hello"}));
        let text = get_text(&result);
        assert!(text.contains("hello"));
    }

    #[test]
    fn test_grep_no_matches() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let result = execute_grep(dir.path(), json!({"pattern": "nonexistent_xyz_12345"}));
        let text = get_text(&result);
        assert!(text.contains("No matches") || text.is_empty());
    }

    #[test]
    fn test_grep_missing_pattern() {
        let dir = TempDir::new().unwrap();
        let result = execute_grep(dir.path(), json!({}));
        let text = get_text(&result);
        assert!(text.contains("Missing required parameter"));
    }

    /// Pi-mono test: a `--pre=…` pattern must not let ripgrep execute
    /// the referenced script as a preprocessor. With `cmd.arg(pattern)`
    /// directly (no `--`), ripgrep parses the flag and runs the
    /// preprocessor on every searched file. This is a real RCE vector
    /// when the pattern comes from an LLM acting on attacker-controlled
    /// content. The fix is to insert `--` before the pattern so flag
    /// parsing stops.
    ///
    /// We verify the guard by trying a flag-shaped pattern that, if
    /// interpreted as a flag, would either error or execute. Either
    /// way the search must NOT produce results and MUST NOT leave a
    /// marker file behind.
    #[test]
    fn test_grep_flag_pattern_does_not_execute_preprocessor() {
        let dir = TempDir::new().unwrap();
        let marker = dir.path().join("grep-injection-marker");
        let payload = dir.path().join("payload.sh");
        let target = dir.path().join("target.txt");
        std::fs::write(
            &payload,
            format!(
                "#!/bin/sh\necho executed > {}\ncat \"$1\"\n",
                marker.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(&target, "target\n").unwrap();

        let result = execute_grep(
            dir.path(),
            json!({
                "pattern": format!("--pre={}", payload.display()),
                "path": dir.path().to_str().unwrap(),
            }),
        );
        let text = get_text(&result);
        // The flag-shaped pattern should be treated as literal search
        // text (no match against `target\n`).
        assert!(
            text.contains("No matches") || text.contains("no matches") || text.trim().is_empty()
                || text.to_lowercase().contains("not found"),
            "expected no matches for flag-pattern, got: {text}"
        );
        // And the preprocessor MUST NOT have run.
        assert!(
            !marker.exists(),
            "RCE: payload executed and wrote {}",
            marker.display()
        );
    }
}
