//! Read tool — read file contents with line numbers.

use crate::tools::path_utils::resolve_read_path;
use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Default max lines to read.
const DEFAULT_MAX_LINES: usize = 2000;
/// Default max bytes to read (pi-mono parity: 50 KB).
const DEFAULT_MAX_BYTES: usize = 50 * 1024;

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

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

    let path = resolve_read_path(path_str, cwd);
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    // `limit` is user-controlled — only enforce the line budget if absent.
    let user_limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
    let limit = user_limit.unwrap_or(DEFAULT_MAX_LINES);

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Pi-mono parity: an offset past the last line is an error the model
    // should see explicitly, not a silently-empty read.
    let start_zero_based = offset.saturating_sub(1);
    if start_zero_based >= total_lines && total_lines > 0 {
        return ToolResult::error(format!(
            "Offset {} is beyond end of file ({} lines total)",
            offset, total_lines
        ));
    }

    // Apply offset (1-based) and limit (line budget)
    let start = start_zero_based.min(total_lines);
    let end = (start + limit).min(total_lines);

    // Pi-mono parity: when no explicit user limit was given, ALSO enforce
    // a byte budget (50 KB). A 2000-line file of long lines (minified JS,
    // generated bundles) can blow the context window even though the line
    // count fits. We accumulate complete lines until either the line
    // window or the byte budget is hit.
    let byte_budget_active = user_limit.is_none();
    let mut output = String::new();
    let mut included = 0usize; // number of lines actually emitted
    let mut byte_truncated = false;
    let mut total_bytes = 0usize;

    // First-line-exceeds-limit edge case (only when byte budget is active):
    // a single source line bigger than 50 KB cannot be displayed at all.
    // Point the model at a bash fallback so it has an actionable next step.
    if byte_budget_active && start < lines.len() {
        let first_line_bytes = lines[start].len();
        if first_line_bytes > DEFAULT_MAX_BYTES {
            let line_num = start + 1;
            return ToolResult::text(format!(
                "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                line_num,
                format_size(first_line_bytes),
                format_size(DEFAULT_MAX_BYTES),
                line_num,
                path.display(),
                DEFAULT_MAX_BYTES
            ));
        }
    }

    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let prefix = format!("{:>6}→", line_num);
        let line_bytes = prefix.len() + line.len() + 1; // +1 for trailing \n
        if byte_budget_active && total_bytes + line_bytes > DEFAULT_MAX_BYTES && included > 0 {
            byte_truncated = true;
            break;
        }
        output.push_str(&prefix);
        output.push_str(line);
        output.push('\n');
        total_bytes += line_bytes;
        included += 1;
    }

    let last_shown = start + included;
    if byte_truncated {
        output.push_str(&format!(
            "\n[Showing lines {}-{} of {} ({} byte limit). Use offset={} to continue.]",
            start + 1,
            last_shown,
            total_lines,
            format_size(DEFAULT_MAX_BYTES),
            last_shown + 1
        ));
    } else if last_shown < total_lines {
        output.push_str(&format!(
            "\n[Showing lines {}-{} of {} total. Use offset/limit to read more.]",
            start + 1,
            last_shown,
            total_lines
        ));
    }

    ToolResult::text(output)
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

    /// Pi-mono parity: a `~/...` path in the read tool must expand to
    /// the user's home directory. Hand was passing the literal `~/foo`
    /// through `cwd.join("~/foo")`, which on POSIX produces an absolute
    /// path joined-then-discarded but never actually reaches the home
    /// directory — so reads of `~/something` silently fail.
    #[test]
    fn test_read_expands_tilde() {
        // Pick a file that virtually always exists in $HOME and that we
        // are allowed to read on macOS sandboxed CI: ~/.zprofile is too
        // unreliable, but we can create one in a TempDir set as $HOME.
        let dir = TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        // SAFETY: tests run single-threaded for std::env::set_var; tokio
        // tests can race so we keep this in a sync test. The pattern is
        // used throughout the codebase.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        std::fs::write(dir.path().join("tilde.txt"), "from home").unwrap();

        let result = execute_read(dir.path(), json!({"path": "~/tilde.txt"}));
        let text = get_text(&result);

        if let Some(h) = original_home {
            unsafe { std::env::set_var("HOME", h); }
        } else {
            unsafe { std::env::remove_var("HOME"); }
        }

        assert!(
            text.contains("from home"),
            "tilde path must resolve to $HOME, got: {text}"
        );
    }

    /// Pi-mono parity: an explicit offset past the end of the file is a
    /// programming error from the model side, not a silently-empty read.
    /// Pi raises "Offset N is beyond end of file"; hand was silently
    /// returning empty output, leaving the model confused as to whether
    /// the file truly had no content past that line or it had skipped
    /// past EOF.
    #[test]
    fn test_read_offset_beyond_eof_errors() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("small.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();

        let result = execute_read(
            dir.path(),
            json!({"path": file.to_str().unwrap(), "offset": 99}),
        );
        let text = get_text(&result);
        assert!(
            text.contains("beyond end of file") || text.contains("offset"),
            "expected explicit out-of-bounds error, got: {text}"
        );
    }

    /// Pi-mono parity: when no user-supplied limit is given, the read tool
    /// must cap output at BOTH 2000 lines AND 50KB. A file of short lines
    /// well under the line limit can still blow the context window if the
    /// payload bytes are large (minified JS, vendored CSS).
    #[test]
    fn test_read_applies_byte_limit_when_no_user_limit() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("big.txt");
        // 1000 lines × 100 chars/line ≈ 100KB, well over the 50KB byte cap
        // but under the 2000-line cap. Without a byte cap the whole file
        // would be returned.
        let line: String = "x".repeat(100);
        let mut content = String::new();
        for _ in 0..1000 {
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(&file, &content).unwrap();

        let result = execute_read(dir.path(), json!({"path": file.to_str().unwrap()}));
        let text = get_text(&result);
        // Output must fit in roughly the byte budget plus banners — ~50KB
        // payload + a few hundred bytes of line-number prefixes per line.
        // We assert a generous 80KB upper bound and a truncation notice.
        assert!(
            text.len() < 80 * 1024,
            "expected byte-budgeted truncation, got {} bytes",
            text.len()
        );
        // Pi mentions the byte limit in its truncation notice when bytes
        // were the limiting factor.
        assert!(
            text.contains("KB limit") || text.contains("byte"),
            "expected byte-limit truncation notice, got: {}",
            &text[text.len().saturating_sub(300)..]
        );
    }

    /// Pi-mono parity: if the first line alone exceeds the byte budget,
    /// return a special error pointing at a `sed | head -c` fallback so
    /// the LLM has an actionable next step. Pi calls this the
    /// `firstLineExceedsLimit` edge case.
    #[test]
    fn test_read_first_line_exceeds_byte_limit() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("minified.js");
        // One single line of 100KB — common with minified JS / bundled JSON.
        let blob: String = "a".repeat(100 * 1024);
        std::fs::write(&file, &blob).unwrap();

        let result = execute_read(dir.path(), json!({"path": file.to_str().unwrap()}));
        let text = get_text(&result);
        assert!(
            text.contains("exceeds") && text.contains("limit"),
            "expected first-line-exceeds-limit message, got: {}",
            text
        );
        // Hand should suggest a bash fallback so the model knows what to do.
        assert!(
            text.contains("sed") || text.contains("head"),
            "expected sed/head fallback hint, got: {}",
            text
        );
    }
}
