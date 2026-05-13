//! Edit tool — find-and-replace edits with diff output.

use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_to_cwd;
use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use similar::TextDiff;
use std::path::{Path, PathBuf};

/// Create the edit tool.
pub fn create_edit_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
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
        move |_tool_call_id, args| {
            let cwd = cwd.clone();
            async move { execute_edit(&cwd, args).await }
        },
    )
}

async fn execute_edit(cwd: &Path, args: serde_json::Value) -> ToolResult {
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

    let path = resolve_to_cwd(file_path, cwd);
    let old_string = old_string.to_string();
    let new_string = new_string.to_string();
    let path_for_async = path.clone();

    // Pi-mono parity: serialise the read-modify-write against the same
    // file. Two parallel edits to the same path would otherwise both
    // observe the original content and the later writer would clobber
    // the earlier edit silently.
    with_file_mutation_queue(&path, async move {
        run_edit(&path_for_async, &old_string, &new_string, replace_all)
    })
    .await
}

fn run_edit(path: &Path, old_string: &str, new_string: &str, replace_all: bool) -> ToolResult {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
    };

    // Pi-mono parity: tolerate LF-vs-CRLF mismatches between the
    // model-supplied old_string and the file on disk. Try the literal
    // match first; if that fails AND the line endings of the two
    // strings differ, normalize the old_string to the file's line
    // ending style and retry. Same for new_string so the replacement
    // doesn't introduce mixed endings. See pi-mono edit tool CRLF
    // tests.
    let file_has_crlf = content.contains("\r\n");
    let crlf_old_owned: String;
    let crlf_new_owned: String;
    let (crlf_old, crlf_new): (&str, &str) = {
        if content.contains(old_string) {
            (old_string, new_string)
        } else if file_has_crlf && !old_string.contains("\r\n") && old_string.contains('\n') {
            crlf_old_owned = old_string.replace('\n', "\r\n");
            crlf_new_owned = new_string.replace('\n', "\r\n");
            (crlf_old_owned.as_str(), crlf_new_owned.as_str())
        } else if !file_has_crlf && old_string.contains("\r\n") {
            crlf_old_owned = old_string.replace("\r\n", "\n");
            crlf_new_owned = new_string.replace("\r\n", "\n");
            (crlf_old_owned.as_str(), crlf_new_owned.as_str())
        } else {
            (old_string, new_string)
        }
    };

    // Pi-mono parity stage 2: Unicode fuzzy match. If the CRLF-tolerant
    // version still doesn't find the old_string in the file, try
    // matching in a normalized space where smart quotes / Unicode
    // dashes / NBSP collapse to their ASCII equivalents. When the
    // fuzzy match succeeds, the replacement happens in the normalized
    // content — same side effect as pi: smart quotes in the file get
    // rewritten to ASCII as part of the edit. Documented behavior.
    let (content, old_string, new_string): (String, String, String) = if content.contains(crlf_old)
    {
        (content.clone(), crlf_old.to_string(), crlf_new.to_string())
    } else {
        let fuzzy_content = normalize_for_fuzzy_match(&content);
        let fuzzy_old = normalize_for_fuzzy_match(crlf_old);
        if fuzzy_content.contains(&fuzzy_old) {
            let fuzzy_new = normalize_for_fuzzy_match(crlf_new);
            (fuzzy_content, fuzzy_old, fuzzy_new)
        } else {
            (content.clone(), crlf_old.to_string(), crlf_new.to_string())
        }
    };
    let (old_string, new_string): (&str, &str) = (old_string.as_str(), new_string.as_str());

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

/// Normalize a string for fuzzy edit-tool matching: smart quotes →
/// ASCII, Unicode dashes → ASCII hyphen, NBSP/wide spaces → space.
/// Mirrors pi-mono's `normalizeForFuzzyMatch` minus NFKC (we don't
/// pull a full Unicode normalization dependency for the common case;
/// users hitting NFKC-only edge cases can still rely on the literal-
/// match fast path).
pub fn normalize_for_fuzzy_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let replacement = match ch {
            // Smart single quotes (U+2018, U+2019, U+201A, U+201B)
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            // Smart double quotes (U+201C, U+201D, U+201E, U+201F)
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            // Unicode dashes / minus signs
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            // NBSP and wide / math spaces → ASCII space
            '\u{00A0}'
            | '\u{2002}'..='\u{200A}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        };
        out.push(replacement);
    }
    out
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
    async fn test_edit_simple_replace() {
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
        ).await;
        let text = get_text(&result);
        assert!(text.contains("-hello world"));
        assert!(text.contains("+hello rust"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn test_edit_not_found() {
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
        ).await;
        let text = get_text(&result);
        assert!(text.contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_ambiguous() {
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
        ).await;
        let text = get_text(&result);
        assert!(text.contains("found 2 times"));
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
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
        ).await;
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

    #[tokio::test]
    async fn test_edit_multiline() {
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
        ).await;
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("goodbye"));
    }

    /// Pi-mono CRLF parity: a multi-line `old_string` supplied with LF
    /// separators must still match a CRLF-ended file. The replacement
    /// preserves the file's existing CRLF endings (no mixed line
    /// endings introduced).
    #[tokio::test]
    async fn test_edit_lf_old_string_matches_crlf_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("crlf.txt");
        std::fs::write(&file, "line1\r\nold_a\r\nold_b\r\nline4\r\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "old_a\nold_b",
                "new_string": "new_a\nnew_b",
            }),
        ).await;
        let text = get_text(&result);
        assert!(
            !text.starts_with("Error: old_string not found"),
            "LF old_string should match CRLF file via line-ending normalization, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("new_a\r\nnew_b"), "result preserves CRLF: {after:?}");
        assert!(
            !after.contains("new_a\nnew_b\r\n") || after.matches("\n").count() == after.matches("\r\n").count(),
            "no mixed line endings introduced: {after:?}"
        );
    }

    /// Inverse case: file is LF, model supplies CRLF in old_string.
    /// Match must succeed and result must stay LF.
    #[tokio::test]
    async fn test_edit_crlf_old_string_matches_lf_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lf.txt");
        std::fs::write(&file, "line1\nold_a\nold_b\nline4\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "old_a\r\nold_b",
                "new_string": "new_a\r\nnew_b",
            }),
        ).await;
        let text = get_text(&result);
        assert!(
            !text.starts_with("Error: old_string not found"),
            "CRLF old_string should match LF file via line-ending normalization, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("new_a\nnew_b"));
        assert!(!after.contains("\r\n"), "result stays LF: {after:?}");
    }

    /// Pi-mono fuzzy-match parity: smart curly double quotes (U+201C/D)
    /// in the file must accept ASCII `"` in the model's old_string.
    /// The replacement happens in the normalized space, so the file's
    /// curly quotes get rewritten to ASCII as part of the edit
    /// (documented side effect, mirrors pi).
    #[tokio::test]
    async fn test_edit_fuzzy_smart_double_quotes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("smart.txt");
        std::fs::write(&file, "const msg = \u{201C}Hello World\u{201D};\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "const msg = \"Hello World\";",
                "new_string": "const msg = \"Goodbye\";",
            }),
        ).await;
        let text = get_text(&result);
        assert!(
            !text.starts_with("Error:"),
            "fuzzy quotes must match, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("Goodbye"), "replacement applied: {after:?}");
    }

    /// Smart single quotes (apostrophes) likewise.
    #[tokio::test]
    async fn test_edit_fuzzy_smart_single_quotes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("smart-single.txt");
        std::fs::write(&file, "it\u{2019}s working\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "it's working",
                "new_string": "it's fixed",
            }),
        ).await;
        assert!(!get_text(&result).starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("fixed"));
    }

    /// Unicode en-dash / em-dash collapse to ASCII hyphen.
    #[tokio::test]
    async fn test_edit_fuzzy_unicode_dashes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("dashes.txt");
        // U+2013 en-dash and U+2014 em-dash.
        std::fs::write(&file, "range: 1\u{2013}5\nbreak\u{2014}here\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "range: 1-5",
                "new_string": "range: 10-50",
            }),
        ).await;
        assert!(!get_text(&result).starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("range: 10-50"));
    }

    /// NBSP (U+00A0) in the file matches plain space in old_string.
    #[tokio::test]
    async fn test_edit_fuzzy_nbsp() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("nbsp.txt");
        std::fs::write(&file, "hello\u{00A0}world\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "hello world",
                "new_string": "hello rust",
            }),
        ).await;
        assert!(!get_text(&result).starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("hello rust"));
    }

    #[test]
    fn test_normalize_for_fuzzy_match_pure_function() {
        assert_eq!(
            normalize_for_fuzzy_match("smart \u{201C}quotes\u{201D} and \u{2014}dash"),
            "smart \"quotes\" and -dash"
        );
        assert_eq!(normalize_for_fuzzy_match("plain ascii"), "plain ascii");
        assert_eq!(normalize_for_fuzzy_match("nb\u{00A0}sp"), "nb sp");
    }

    /// Single-line edits with no `\n` in either string must continue
    /// to work the same way they did before the CRLF support landed.
    #[tokio::test]
    async fn test_edit_crlf_normalization_does_not_affect_single_line() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("simple.txt");
        std::fs::write(&file, "hello world\r\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust",
            }),
        ).await;
        let text = get_text(&result);
        assert!(!text.starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "hello rust\r\n");
    }

    /// Pi-mono parity: parallel edits to the same file must serialise
    /// through the mutation queue. Without the queue, two concurrent
    /// edits both observe the original content and the later writer
    /// silently clobbers the earlier edit. With the queue, both edits
    /// land deterministically.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_edit_serialises_concurrent_calls_to_same_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("race.txt");
        // Use a unique starting content so we know the pre-state. Each
        // edit replaces a distinct marker; if the queue is wired, both
        // markers should be gone in the final file.
        std::fs::write(&file, "marker_A\nmarker_B\n").unwrap();

        let cwd = dir.path().to_path_buf();
        let path1 = file.to_str().unwrap().to_string();
        let path2 = path1.clone();
        let cwd1 = cwd.clone();
        let cwd2 = cwd.clone();

        let h1 = tokio::spawn(async move {
            execute_edit(
                &cwd1,
                json!({
                    "file_path": path1,
                    "old_string": "marker_A",
                    "new_string": "RESULT_A",
                }),
            )
            .await
        });
        let h2 = tokio::spawn(async move {
            execute_edit(
                &cwd2,
                json!({
                    "file_path": path2,
                    "old_string": "marker_B",
                    "new_string": "RESULT_B",
                }),
            )
            .await
        });
        let (r1, r2) = tokio::join!(h1, h2);
        let _ = r1.unwrap();
        let _ = r2.unwrap();

        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("RESULT_A"), "edit A must land, got: {after:?}");
        assert!(after.contains("RESULT_B"), "edit B must land, got: {after:?}");
        assert!(
            !after.contains("marker_A"),
            "marker_A should be replaced, got: {after:?}"
        );
        assert!(
            !after.contains("marker_B"),
            "marker_B should be replaced, got: {after:?}"
        );
    }
}
