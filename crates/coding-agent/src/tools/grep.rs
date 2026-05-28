//! Grep tool — search file contents using regex patterns.

use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Default max matches.
const DEFAULT_MAX_MATCHES: usize = 100;
/// Cap each match line to this many characters so a single
/// minified-bundle line cannot dump megabytes into the model context.
const GREP_MAX_LINE_LENGTH: usize = 500;

/// Truncate each line in `output` to `GREP_MAX_LINE_LENGTH` chars, appending
/// `... [truncated]` to lines that exceed the cap. Returns the modified
/// output and a `truncated` flag indicating whether any line was clipped.
fn truncate_long_lines(output: &str) -> (String, bool) {
    let mut any_truncated = false;
    let mut result = String::with_capacity(output.len());
    for (i, line) in output.split('\n').enumerate() {
        if i > 0 {
            result.push('\n');
        }
        if line.chars().count() > GREP_MAX_LINE_LENGTH {
            // Slice at a char boundary, not a byte boundary, so we never
            // emit invalid UTF-8 when a multi-byte codepoint straddles
            // the cutoff.
            let mut end = 0;
            for (idx, _) in line.char_indices().take(GREP_MAX_LINE_LENGTH) {
                end = idx;
            }
            // `end` points at the start of the 500th char; we want to keep
            // it, so advance to the byte after it.
            if let Some((next, _)) = line[end..].char_indices().nth(1) {
                end += next;
            } else {
                end = line.len();
            }
            result.push_str(&line[..end]);
            result.push_str("... [truncated]");
            any_truncated = true;
        } else {
            result.push_str(line);
        }
    }
    (result, any_truncated)
}

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
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return per file (default: 100). `max_matches` is accepted as a deprecated alias for the same parameter."
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
    // Accept `limit` (canonical, matches upstream upstream naming) and fall
    // back to `max_matches` for backwards compatibility with scripts
    // written against an earlier hand schema. The two name-collide on
    // a single int so there's no ambiguity if both are supplied —
    // `limit` wins.
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .or_else(|| args.get("max_matches").and_then(|v| v.as_u64()))
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
        limit,
        case_insensitive,
    )
    .or_else(|| {
        try_grep(
            pattern,
            &search_path,
            include,
            context,
            limit,
            case_insensitive,
        )
    });

    match result {
        Some(output) => {
            if output.is_empty() {
                ToolResult::text("No matches found.")
            } else {
                let (mut clipped, any_truncated) = truncate_long_lines(&output);
                // Count match lines (not context lines). rg context lines
                // contain `-LINENUM-` instead of `:LINENUM:` after the path,
                // so a line containing `:NN:` after a non-empty path
                // segment counts as a match.
                let match_count = clipped.lines().filter(|line| is_match_line(line)).count();
                if match_count >= limit {
                    clipped.push_str(&format!(
                        "\n[{} matches limit reached. Use limit={} for more, or refine pattern]",
                        match_count,
                        limit * 2
                    ));
                }
                if any_truncated {
                    clipped.push_str(&format!(
                        "\n[Some lines truncated to {} chars. Use read tool to see full lines.]",
                        GREP_MAX_LINE_LENGTH
                    ));
                }
                ToolResult::text(clipped)
            }
        }
        None => ToolResult::error("Neither rg nor grep is available"),
    }
}

/// A rg/grep output line is a "match line" (not a context line) iff,
/// after stripping the leading path token (everything up to the first
/// `:` or `-`), the next character is `:` followed by digits. rg uses
/// `-NUM-` for context lines and `:NUM:` for matches; grep -C mirrors
/// that convention. We count match lines to decide whether the result
/// hit the user-supplied `limit`.
fn is_match_line(line: &str) -> bool {
    // Skip the leading path segment by finding the FIRST `:` or `-` from
    // the right side of the first separator boundary. Simpler: find the
    // first `:` and check that the char *immediately* after is a digit
    // followed by `:` — that's the line-number-plus-separator pattern.
    let bytes = line.as_bytes();
    // Scan for `:DIGITS:` somewhere after byte 1.
    let mut i = 1;
    while i + 1 < bytes.len() {
        if bytes[i] == b':' && bytes[i + 1].is_ascii_digit() {
            // Now consume digits.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // Expect a `:` immediately after the digit run.
            return j < bytes.len() && bytes[j] == b':';
        }
        i += 1;
    }
    false
}

fn try_ripgrep(
    pattern: &str,
    search_path: &Path,
    include: Option<&str>,
    context: Option<usize>,
    limit: usize,
    case_insensitive: bool,
) -> Option<String> {
    let mut cmd = Command::new("rg");
    cmd.arg("--no-heading")
        .arg("--line-number")
        .arg("--max-count")
        .arg(limit.to_string());

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
    // script for every searched file (an LLM-injection RCE).
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
    limit: usize,
    case_insensitive: bool,
) -> Option<String> {
    let mut cmd = Command::new("grep");
    cmd.arg("-r")
        .arg("-n")
        .arg("--max-count")
        .arg(limit.to_string());

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

    /// Each match line is capped at GREP_MAX_LINE_LENGTH chars so a
    /// minified JS file containing the pattern cannot dump a 50KB
    /// single line into the LLM context. The clip is signalled with a
    /// trailing `... [truncated]` suffix.
    #[test]
    fn test_grep_clips_long_match_lines() {
        let dir = TempDir::new().unwrap();
        // A line with MATCHME early (so it survives clipping), followed by
        // a huge padding tail that should be replaced with the truncation
        // marker. Without the per-line cap the whole 5000-char line ends
        // up in the result.
        let big_line = format!("prefix MATCHME more text {}", "y".repeat(5000));
        std::fs::write(dir.path().join("bundle.min.js"), &big_line).unwrap();

        let result = execute_grep(dir.path(), json!({"pattern": "MATCHME"}));
        let text = get_text(&result);
        assert!(
            text.contains("MATCHME"),
            "match content must still appear, got len={}",
            text.len()
        );
        assert!(
            text.contains("... [truncated]"),
            "expected truncation marker, got: {}...",
            &text[..text.len().min(200)]
        );
        // Whole output should be well under the original 5KB+ line.
        assert!(
            text.len() < 2000,
            "expected clipped output, got {} bytes",
            text.len()
        );
    }

    #[test]
    fn test_truncate_long_lines_short_lines_passthrough() {
        let input = "short line\nanother short line\n";
        let (out, truncated) = truncate_long_lines(input);
        assert_eq!(out, input);
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_long_lines_clips_long_line() {
        let long = "a".repeat(GREP_MAX_LINE_LENGTH + 200);
        let (out, truncated) = truncate_long_lines(&long);
        assert!(truncated);
        assert!(out.starts_with(&"a".repeat(GREP_MAX_LINE_LENGTH)));
        assert!(out.ends_with("... [truncated]"));
    }

    /// UTF-8 boundary safety: clipping a long line at the 500-char mark
    /// must not slice mid-codepoint. Use a 4-byte emoji repeated past the
    /// cap so a naive byte-slice would land mid-sequence.
    #[test]
    fn test_truncate_long_lines_respects_utf8_boundary() {
        let line: String = "😀".repeat(GREP_MAX_LINE_LENGTH + 10);
        let (out, truncated) = truncate_long_lines(&line);
        assert!(truncated);
        // Round-trip through String parsing — if any codepoint got chopped,
        // String::from_utf8 would catch it (String already enforces this,
        // but we run a chars() count to double-check no garbage emojis).
        let kept_emojis = out.chars().filter(|&c| c == '😀').count();
        assert_eq!(kept_emojis, GREP_MAX_LINE_LENGTH);
    }

    /// A `--pre=…` pattern must not let ripgrep execute the
    /// referenced script as a preprocessor. With `cmd.arg(pattern)`
    /// directly (no `--`), ripgrep parses the flag and runs the
    /// preprocessor on every searched file. This is a real RCE vector
    /// when the pattern comes from an LLM acting on attacker-
    /// controlled content. The fix is to insert `--` before the
    /// pattern so flag parsing stops.
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
            text.contains("No matches")
                || text.contains("no matches")
                || text.trim().is_empty()
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

    /// `limit` caps per-file matches and emits a footer pointing at how
    /// the user can fetch more. The wording matches the upstream's
    /// `[N matches limit reached. Use limit=M for more, or refine pattern]`
    /// so scripts that parse the footer keep working.
    #[test]
    fn test_grep_limit_param_emits_footer() {
        let dir = TempDir::new().unwrap();
        let lines = (1..=5)
            .map(|i| format!("match line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("limited.txt"), lines).unwrap();

        let result = execute_grep(dir.path(), json!({"pattern": "match", "limit": 2}));
        let text = get_text(&result);
        assert!(
            text.contains("matches limit reached"),
            "expected limit footer, got: {text}"
        );
        assert!(
            text.contains("Use limit="),
            "footer should suggest a higher limit, got: {text}"
        );
        // Exactly the first two match lines should be present.
        assert!(text.contains("match line 1"));
        assert!(text.contains("match line 2"));
        assert!(
            !text.contains("match line 3"),
            "third match should be cut by limit=2"
        );
    }

    /// The deprecated `max_matches` parameter is still honoured as an
    /// alias for `limit` so scripts written against an earlier hand
    /// schema keep working. When both are supplied, `limit` wins.
    #[test]
    fn test_grep_max_matches_alias_for_limit() {
        let dir = TempDir::new().unwrap();
        let lines = (1..=5)
            .map(|i| format!("match line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("aliased.txt"), lines).unwrap();

        let result = execute_grep(dir.path(), json!({"pattern": "match", "max_matches": 1}));
        let text = get_text(&result);
        assert!(text.contains("match line 1"));
        assert!(
            !text.contains("match line 2"),
            "max_matches alias should cap at 1, got: {text}"
        );
    }
}
