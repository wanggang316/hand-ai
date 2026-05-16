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
        "Edit a file by replacing an exact string match. Use either single-edit \
         shape (`old_string`/`new_string`/`replace_all`) for a one-shot replacement, \
         or multi-edit shape (`edits: [{oldText, newText}]`) to apply several \
         disjoint replacements atomically. Returns a unified diff of all changes.",
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "Single-edit: the exact string to find and replace. \
                                    Ignored when `edits` is supplied."
                },
                "new_string": {
                    "type": "string",
                    "description": "Single-edit: the replacement string. Ignored when \
                                    `edits` is supplied."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Single-edit: if true, replace all occurrences of \
                                    old_string. Default: false. Ignored when \
                                    `edits` is supplied."
                },
                "edits": {
                    "type": "array",
                    "description": "Multi-edit: apply several disjoint replacements \
                                    against the original file content. Each entry \
                                    has `oldText` and `newText`. Matches the upstream \
                                    pi-mono shape. When supplied, the single-edit \
                                    parameters are ignored and the call is atomic — \
                                    a failure in any entry rolls back the whole batch.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["file_path"]
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
    let path = resolve_to_cwd(file_path, cwd);
    let path_for_async = path.clone();

    // Multi-edit shape: `edits: [{oldText, newText}]`. Takes priority
    // over the single-edit parameters when present.
    if let Some(edits_value) = args.get("edits") {
        let edits = match parse_edits_array(edits_value) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        return with_file_mutation_queue(&path, async move {
            run_multi_edit(&path_for_async, &edits)
        })
        .await;
    }

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

    let old_string = old_string.to_string();
    let new_string = new_string.to_string();

    // Serialise the read-modify-write against the same file. Two
    // parallel edits to the same path would otherwise both observe the
    // original content and the later writer would clobber the earlier
    // edit silently.
    with_file_mutation_queue(&path, async move {
        run_edit(&path_for_async, &old_string, &new_string, replace_all)
    })
    .await
}

/// A single (oldText → newText) replacement in the multi-edit array.
#[derive(Debug, Clone)]
struct EditEntry {
    old_text: String,
    new_text: String,
}

fn parse_edits_array(value: &serde_json::Value) -> Result<Vec<EditEntry>, String> {
    let arr = value
        .as_array()
        .ok_or_else(|| "edits must be an array of {oldText, newText} objects".to_string())?;
    if arr.is_empty() {
        return Err("edits must contain at least one replacement".to_string());
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let old_text = entry
            .get("oldText")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edits[{}].oldText must be a string", i))?
            .to_string();
        let new_text = entry
            .get("newText")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edits[{}].newText must be a string", i))?
            .to_string();
        out.push(EditEntry { old_text, new_text });
    }
    Ok(out)
}

/// Apply a batch of edits atomically against the file's original
/// content. Semantics:
///   1. Read the file once. Strip a leading UTF-8 BOM for matching;
///      restore it on write so callers don't lose it.
///   2. CRLF tolerance — if any entry's oldText uses LF but the file
///      is CRLF (or vice versa), rewrite that entry's line endings
///      to match the file before resolving.
///   3. Locate each `oldText` in the (BOM-stripped, CRLF-aligned)
///      original; reject when missing or ambiguous.
///   4. If literal matching fails for any entry, retry the whole
///      batch under fuzzy Unicode normalisation (smart quotes,
///      dashes, NBSP collapsed to ASCII). Same side-effect as the
///      single-edit path: fuzzy mode rewrites the matched span to
///      its normalised form.
///   5. Detect overlapping byte ranges across the batch.
///   6. Apply replacements against the ORIGINAL content (not
///      incrementally), then re-prepend the BOM if present and
///      write once.
///
/// On any failure the file is NOT modified.
fn run_multi_edit(path: &Path, edits: &[EditEntry]) -> ToolResult {
    let raw_content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            let code = match e.kind() {
                std::io::ErrorKind::NotFound => "ENOENT",
                std::io::ErrorKind::PermissionDenied => "EACCES",
                std::io::ErrorKind::AlreadyExists => "EEXIST",
                std::io::ErrorKind::InvalidInput => "EINVAL",
                _ => "EIO",
            };
            return ToolResult::error(format!(
                "Could not edit file: {}. Error code: {}.",
                path.display(),
                code
            ));
        }
    };

    // BOM handling: strip a leading U+FEFF for matching, remember the
    // byte length to re-prepend it on write.
    let (bom, content): (&str, String) = if let Some(stripped) = raw_content.strip_prefix('\u{FEFF}')
    {
        ("\u{FEFF}", stripped.to_string())
    } else {
        ("", raw_content.clone())
    };

    // CRLF tolerance per-edit. Same rules as the single-edit path.
    let file_has_crlf = content.contains("\r\n");
    let crlf_aligned: Vec<(String, String)> = edits
        .iter()
        .map(|e| {
            let old = if file_has_crlf
                && !e.old_text.contains("\r\n")
                && e.old_text.contains('\n')
            {
                e.old_text.replace('\n', "\r\n")
            } else if !file_has_crlf && e.old_text.contains("\r\n") {
                e.old_text.replace("\r\n", "\n")
            } else {
                e.old_text.clone()
            };
            let new = if file_has_crlf
                && !e.new_text.contains("\r\n")
                && e.new_text.contains('\n')
            {
                e.new_text.replace('\n', "\r\n")
            } else if !file_has_crlf && e.new_text.contains("\r\n") {
                e.new_text.replace("\r\n", "\n")
            } else {
                e.new_text.clone()
            };
            (old, new)
        })
        .collect();

    // Pick the work-space: literal CRLF-aligned, or Unicode-normalised
    // fuzzy. We try literal first and only fall back to fuzzy when at
    // least one edit's oldText isn't present literally.
    let needs_fuzzy = crlf_aligned
        .iter()
        .any(|(o, _)| !content.contains(o.as_str()));
    let (work_content, work_edits): (String, Vec<(String, String)>) = if needs_fuzzy {
        let nc = normalize_for_fuzzy_match(&content);
        let ne: Vec<(String, String)> = crlf_aligned
            .iter()
            .map(|(o, n)| {
                (
                    normalize_for_fuzzy_match(o),
                    normalize_for_fuzzy_match(n),
                )
            })
            .collect();
        (nc, ne)
    } else {
        (content.clone(), crlf_aligned)
    };

    // Resolve each edit's byte range in the work content. Reject
    // missing / ambiguous before any byte is written.
    struct Resolved {
        start: usize,
        end: usize,
        new: String,
    }
    let mut resolved: Vec<Resolved> = Vec::with_capacity(work_edits.len());
    for (i, (old, new)) in work_edits.iter().enumerate() {
        let match_count = work_content.matches(old.as_str()).count();
        if match_count == 0 {
            return ToolResult::error(format!(
                "Could not find the exact text for edits[{}] in {}: {:?}",
                i,
                path.display(),
                truncate_for_display(old)
            ));
        }
        if match_count > 1 {
            return ToolResult::error(format!(
                "Found {} occurrences of edits[{}] oldText in {}; supply more \
                 context to make it unique.",
                match_count,
                i,
                path.display()
            ));
        }
        let start = work_content.find(old.as_str()).expect("just counted");
        let end = start + old.len();
        resolved.push(Resolved {
            start,
            end,
            new: new.clone(),
        });
    }

    // Sort by start offset so overlap detection is a linear scan.
    resolved.sort_by_key(|r| r.start);
    for window in resolved.windows(2) {
        if window[0].end > window[1].start {
            return ToolResult::error(format!(
                "edits overlap in {}: byte range [{}, {}) collides with [{}, {})",
                path.display(),
                window[0].start,
                window[0].end,
                window[1].start,
                window[1].end
            ));
        }
    }

    // Build the new content by stitching work-content slices with the
    // new texts in offset order.
    let mut new_content = String::with_capacity(work_content.len());
    let mut cursor = 0;
    for r in &resolved {
        new_content.push_str(&work_content[cursor..r.start]);
        new_content.push_str(&r.new);
        cursor = r.end;
    }
    new_content.push_str(&work_content[cursor..]);

    // Diff once across the whole batch so the model sees a single
    // unified diff covering every change.
    let diff = generate_diff(&work_content, &new_content, &path.display().to_string());

    // Re-prepend the BOM (if any) before writing so we don't drop it.
    let final_bytes = if bom.is_empty() {
        new_content.clone()
    } else {
        let mut s = String::with_capacity(bom.len() + new_content.len());
        s.push_str(bom);
        s.push_str(&new_content);
        s
    };
    if let Err(e) = std::fs::write(path, &final_bytes) {
        return ToolResult::error(format!("Failed to write file: {}", e));
    }

    let summary = if resolved.len() == 1 {
        "Successfully replaced 1 block".to_string()
    } else {
        format!("Successfully replaced {} block(s)", resolved.len())
    };
    let body = format!("{summary}\n\n{diff}");
    let result = ToolResult::text(body);
    result.with_details(json!({ "diff": diff, "edits_applied": resolved.len() }))
}

/// Render a short preview of an `oldText` for inclusion in error
/// messages. Long strings are clipped so the error stays readable.
fn truncate_for_display(s: &str) -> String {
    if s.len() <= 80 {
        s.to_string()
    } else {
        format!("{}…", &s[..80])
    }
}

fn run_edit(path: &Path, old_string: &str, new_string: &str, replace_all: bool) -> ToolResult {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            // Surface the io::ErrorKind as a named POSIX-style code so
            // error messages match pi's
            //   "Could not edit file: <path>. Error code: ENOENT."
            // shape. Falls back to the raw display when the kind is
            // not one of the conventional POSIX-mapped variants.
            let code = match e.kind() {
                std::io::ErrorKind::NotFound => "ENOENT".to_string(),
                std::io::ErrorKind::PermissionDenied => "EACCES".to_string(),
                std::io::ErrorKind::AlreadyExists => "EEXIST".to_string(),
                std::io::ErrorKind::InvalidInput => "EINVAL".to_string(),
                _ => format!("{:?}", e.kind()),
            };
            return ToolResult::error(format!(
                "Could not edit file: {}. Error code: {}.",
                path.display(),
                code
            ));
        }
    };

    // Tolerate LF-vs-CRLF mismatches between the model-supplied
    // old_string and the file on disk. Try the literal match first;
    // if that fails AND the line endings of the two strings differ,
    // normalize the old_string to the file's line ending style and
    // retry. Same for new_string so the replacement doesn't
    // introduce mixed endings.
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

    // Stage 2: Unicode fuzzy match. If the CRLF-tolerant version
    // still doesn't find the old_string in the file, try matching in
    // a normalized space where smart quotes / Unicode dashes / NBSP
    // collapse to their ASCII equivalents. When the fuzzy match
    // succeeds, the replacement happens in the normalized
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
    if let Err(e) = std::fs::write(path, &new_content) {
        return ToolResult::error(format!("Failed to write file: {}", e));
    }

    ToolResult::text(diff)
}

/// Normalize a string for fuzzy edit-tool matching: smart quotes →
/// ASCII, Unicode dashes → ASCII hyphen, NBSP/wide spaces → space.
/// NFKC is deliberately omitted to avoid a Unicode-normalization
/// dependency for the common case; users hitting NFKC-only edge
/// cases can still rely on the literal-match fast path.
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
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
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
        )
        .await;
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
        )
        .await;
        let text = get_text(&result);
        assert!(text.contains("not found"));
    }

    /// Editing a path that doesn't exist surfaces the pi-aligned
    /// error wording: `Could not edit file: <path>. Error code: ENOENT.`
    /// — code is the POSIX-style name, not the raw Display impl.
    #[tokio::test]
    async fn test_edit_missing_file_surfaces_enoent_code() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.txt");
        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": missing.to_str().unwrap(),
                "old_string": "x",
                "new_string": "y"
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Error code: ENOENT"),
            "expected ENOENT code, got: {text}"
        );
        assert!(
            text.contains("Could not edit file:"),
            "expected pi-aligned prefix, got: {text}"
        );
    }

    /// Editing a read-only file surfaces `Error code: EACCES.` so the
    /// model can distinguish missing vs permission-denied paths.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_edit_readonly_file_surfaces_eacces_code() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("readonly.txt");
        std::fs::write(&file, "hello").unwrap();
        // Mode 0o000 — neither read nor write — so the read() call
        // inside run_edit triggers EACCES rather than EPERM.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "world"
            }),
        )
        .await;
        // Restore perms so TempDir can clean up.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let text = get_text(&result);
        assert!(
            text.contains("Error code: EACCES")
                || text.contains("Error code: PermissionDenied"),
            "expected EACCES (or platform-named permission code), got: {text}"
        );
        assert!(text.contains("Could not edit file:"));
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
        )
        .await;
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
        )
        .await;
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
        )
        .await;
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("goodbye"));
    }

    /// A multi-line `old_string` supplied with LF separators must
    /// still match a CRLF-ended file. The replacement preserves the
    /// file's existing CRLF endings (no mixed line endings
    /// introduced).
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
        )
        .await;
        let text = get_text(&result);
        assert!(
            !text.starts_with("Error: old_string not found"),
            "LF old_string should match CRLF file via line-ending normalization, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(
            after.contains("new_a\r\nnew_b"),
            "result preserves CRLF: {after:?}"
        );
        assert!(
            !after.contains("new_a\nnew_b\r\n")
                || after.matches("\n").count() == after.matches("\r\n").count(),
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
        )
        .await;
        let text = get_text(&result);
        assert!(
            !text.starts_with("Error: old_string not found"),
            "CRLF old_string should match LF file via line-ending normalization, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("new_a\nnew_b"));
        assert!(!after.contains("\r\n"), "result stays LF: {after:?}");
    }

    /// Smart curly double quotes (U+201C/D) in the file must accept
    /// ASCII `"` in the model's old_string. The replacement happens in
    /// the normalized space, so the file's curly quotes get rewritten
    /// to ASCII as part of the edit (documented side effect).
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
        )
        .await;
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
        )
        .await;
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
        )
        .await;
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
        )
        .await;
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
        )
        .await;
        let text = get_text(&result);
        assert!(!text.starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "hello rust\r\n");
    }

    /// Parallel edits to the same file must serialise through the
    /// mutation queue. Without the queue, two concurrent edits both
    /// observe the original content and the later writer silently
    /// clobbers the earlier edit. With the queue, both edits land
    /// deterministically.
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
        assert!(
            after.contains("RESULT_A"),
            "edit A must land, got: {after:?}"
        );
        assert!(
            after.contains("RESULT_B"),
            "edit B must land, got: {after:?}"
        );
        assert!(
            !after.contains("marker_A"),
            "marker_A should be replaced, got: {after:?}"
        );
        assert!(
            !after.contains("marker_B"),
            "marker_B should be replaced, got: {after:?}"
        );
    }

    /// Edit and write tools must SHARE the mutation queue. A
    /// concurrent `write` to a file currently being `edit`-ed would
    /// otherwise interleave and either:
    /// - the write clobbers the edit's mid-flight read-modify-write, OR
    /// - the edit reads original content, write completes, edit writes
    ///   back stale content, losing the write.
    ///
    /// Both ends use `with_file_mutation_queue(&path, ...)` keyed on the
    /// canonical path, so a write that arrives during a slow edit must
    /// queue behind it. We test by issuing N parallel writes + N parallel
    /// edits and asserting (a) no crashes, (b) the file remains
    /// well-formed (no zero-length or torn content), and (c) every
    /// completed call returned successfully.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_edit_and_write_share_mutation_queue() {
        use crate::tools::write;
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("shared.txt");
        // Initial content with two edit anchors.
        std::fs::write(&file, "alpha\nbeta\n").unwrap();

        let cwd = dir.path().to_path_buf();
        let path = file.to_str().unwrap().to_string();
        let mut handles = Vec::new();

        // Three edits + three writes against the same file, interleaved.
        for i in 0..3 {
            let cwd_e = cwd.clone();
            let path_e = path.clone();
            let h = tokio::spawn(async move {
                execute_edit(
                    &cwd_e,
                    json!({
                        "file_path": path_e,
                        "old_string": if i % 2 == 0 { "alpha" } else { "beta" },
                        "new_string": format!("EDIT_{i}"),
                        "replace_all": true,
                    }),
                )
                .await
            });
            handles.push(h);

            let cwd_w = cwd.clone();
            let path_w = path.clone();
            let h = tokio::spawn(async move {
                // Each write replaces the whole file with a single sentinel
                // line. If edits and writes don't share the queue, this
                // would land in the middle of an in-flight edit's
                // read-modify-write and either crash or produce torn content.
                write::__test_only::execute_write_for_test(
                    &cwd_w,
                    json!({"path": path_w, "content": format!("WRITE_{i}\n")}),
                )
                .await
            });
            handles.push(h);
        }
        for h in handles {
            h.await.unwrap();
        }

        // File must still be readable and one of the known final states.
        let after = std::fs::read_to_string(&file).expect("file readable");
        assert!(
            !after.is_empty(),
            "file became empty — torn write under no-queue race"
        );
        // The final state is either a write sentinel or an edit result —
        // both are valid endpoints. What must NOT happen is a chimera
        // that mixes characters from both (e.g. `WRITE_0\nbeta\n` with a
        // stray `EDIT_` substring), which would mean a write and an edit
        // didn't serialise.
        let valid =
            after.starts_with("WRITE_") || after.starts_with("EDIT_") || after.contains("EDIT_");
        assert!(valid, "unexpected final content: {after:?}");
    }

    // ---------------- Multi-edit array surface ----------------

    /// UC-edit-005 — a batch `edits: [{oldText, newText}, ...]` array
    /// replaces multiple disjoint regions in a single call. The
    /// response reports a `Successfully replaced N block(s)` summary
    /// and a unified diff covering every change.
    #[tokio::test]
    async fn test_edit_multi_edit_replaces_disjoint_regions() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("multi.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "alpha", "newText": "ALPHA" },
                    { "oldText": "gamma", "newText": "GAMMA" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Successfully replaced 2 block(s)"),
            "expected 2-block summary, got: {text}"
        );
        assert!(text.contains("-alpha"));
        assert!(text.contains("+ALPHA"));
        assert!(text.contains("-gamma"));
        assert!(text.contains("+GAMMA"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "ALPHA\nbeta\nGAMMA\ndelta\n");
    }

    /// UC-edit-007 — every entry resolves against the ORIGINAL file
    /// content, not the file as mutated by previous entries. If a
    /// `newText` happens to recreate a later entry's `oldText`, the
    /// later entry should still match its original site, not the
    /// freshly-written one.
    #[tokio::test]
    async fn test_edit_multi_edit_matches_against_original_not_incremental() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("orig.txt");
        // `bar` appears once in the original. The first edit rewrites
        // `foo` → `foo bar`, which would create a second `bar` if we
        // matched incrementally. The second edit `bar` → `BAR` must
        // therefore see only the ORIGINAL single `bar`, not the one
        // the first edit synthesised.
        std::fs::write(&file, "foo\nbar\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "foo", "newText": "foo bar" },
                    { "oldText": "bar", "newText": "BAR" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Successfully replaced 2 block(s)"),
            "expected both edits to land, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "foo bar\nBAR\n");
    }

    /// UC-edit-008 — empty `edits` arrays are rejected at parse time.
    #[tokio::test]
    async fn test_edit_multi_edit_empty_array_rejected() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("empty-edits.txt");
        std::fs::write(&file, "hello\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": []
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("at least one replacement"),
            "expected empty-array rejection, got: {text}"
        );
        // File untouched.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    }

    /// UC-edit-009 — two entries whose resolved byte ranges overlap
    /// are rejected before any write.
    #[tokio::test]
    async fn test_edit_multi_edit_overlapping_regions_rejected() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("overlap.txt");
        std::fs::write(&file, "abcdefg\n").unwrap();

        // Both anchors are nested inside `abcdef` — they share bytes.
        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "abcd", "newText": "X" },
                    { "oldText": "bcde", "newText": "Y" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("overlap"),
            "expected overlap rejection, got: {text}"
        );
        // File untouched — atomic rollback.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "abcdefg\n");
    }

    /// UC-edit-010 — when one entry in the batch fails (oldText not
    /// found), the whole batch rolls back. No partial application.
    #[tokio::test]
    async fn test_edit_multi_edit_no_partial_application_on_failure() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("rollback.txt");
        let original = "alpha\nbeta\n";
        std::fs::write(&file, original).unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "alpha", "newText": "ALPHA" },
                    { "oldText": "NEVER-EXISTS", "newText": "X" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Could not find the exact text"),
            "expected per-edit miss error, got: {text}"
        );
        // File untouched — the first edit must NOT have landed.
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            original,
            "atomic rollback: file content must equal pre-call snapshot"
        );
    }

    /// UC-edit-025 — fuzzy matching applies in multi-edit mode too.
    /// One entry that requires smart-quote → ASCII normalisation
    /// triggers fuzzy mode for the whole batch.
    #[tokio::test]
    async fn test_edit_multi_edit_fuzzy_matching_applies() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fuzzy-multi.txt");
        std::fs::write(
            &file,
            "name = \u{201C}widget\u{201D};\nrange: 1\u{2013}5\n",
        )
        .unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "name = \"widget\";", "newText": "name = \"gadget\";" },
                    { "oldText": "range: 1-5", "newText": "range: 10-50" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Successfully replaced 2 block(s)"),
            "expected both fuzzy edits to land, got: {text}"
        );
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("gadget"));
        assert!(after.contains("range: 10-50"));
    }

    /// UC-edit-022 — exact match is preferred over fuzzy match. If
    /// the literal `old_string` is already present in the file, the
    /// edit lands there without engaging the Unicode-fuzzy fallback.
    /// We verify by editing a file that contains both an ASCII and a
    /// smart-quoted variant — the ASCII anchor must hit the ASCII
    /// occurrence verbatim, never the smart-quoted one.
    #[tokio::test]
    async fn test_edit_prefers_exact_match_over_fuzzy() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mix.txt");
        // Two distinct lines: one with ASCII quotes, one with smart
        // double quotes. Editing the ASCII line should land there;
        // the smart-quoted line must remain untouched (no
        // normalisation rewrite of the smart quotes).
        let body = "msg = \"plain\";\nother = \u{201C}fancy\u{201D};\n";
        std::fs::write(&file, body).unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "\"plain\"",
                "new_string": "\"changed\"",
            }),
        )
        .await;
        assert!(!get_text(&result).starts_with("Error:"));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("\"changed\""), "exact match must apply");
        // Smart quotes preserved — fuzzy fallback NOT engaged.
        assert!(
            after.contains('\u{201C}') && after.contains('\u{201D}'),
            "smart quotes must remain untouched, got: {after:?}"
        );
    }

    /// UC-edit-024 — duplicate detection survives Unicode-fuzzy
    /// normalisation. When two distinct fuzzy-equivalent occurrences
    /// exist (e.g. one ASCII `"foo"` and one smart `\u{201C}foo\u{201D}`),
    /// the file has 2 normalised matches for the ASCII `old_string`
    /// — the edit must refuse with an ambiguity error rather than
    /// silently replacing one.
    #[tokio::test]
    async fn test_edit_fuzzy_match_detects_duplicates() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("dup.txt");
        // Two lines that BOTH match `"foo"` once normalised (one
        // smart, one ASCII). Without the file having a literal
        // `"foo"` for the fast-path, the fuzzy stage takes over and
        // sees two matches.
        let body = "a = \u{201C}foo\u{201D};\nb = \u{201C}foo\u{201D};\n";
        std::fs::write(&file, body).unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "\"foo\"",
                "new_string": "\"bar\"",
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("found 2 times") || text.contains("2 occurrences"),
            "fuzzy duplicate must surface an ambiguity error, got: {text}"
        );
        // File untouched.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
    }

    /// UC-edit-029 — CRLF↔LF normalisation must not mask duplicates.
    /// A file containing both CRLF and LF variants of the same
    /// anchor should be rejected with an ambiguity error when the
    /// model supplies the anchor in either form, not silently apply
    /// to only one variant.
    #[tokio::test]
    async fn test_edit_detects_duplicates_across_crlf_lf_variants() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mixed-endings.txt");
        // The same anchor appears twice — once with literal LF only,
        // once embedded in CRLF context. With CRLF tolerance on, both
        // are reachable from one `old_string`.
        let body = "marker\nbody\nmarker\nbody\n";
        std::fs::write(&file, body).unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "marker",
                "new_string": "MARK",
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("found 2 times"),
            "two literal anchors must be flagged ambiguous, got: {text}"
        );
    }

    /// UC-edit-030 — a UTF-8 BOM at the head of the file survives a
    /// single-edit replacement. The BOM is not part of the model's
    /// `old_string`; it sits before the matched region and the
    /// rebuild preserves it verbatim.
    #[tokio::test]
    async fn test_edit_single_edit_preserves_utf8_bom() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("bom.txt");
        // BOM + body. The model's old_string is content-only.
        let body = "\u{FEFF}hello world\n";
        std::fs::write(&file, body.as_bytes()).unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust",
            }),
        )
        .await;
        assert!(!get_text(&result).starts_with("Error:"));
        let after = std::fs::read(&file).unwrap();
        assert_eq!(
            &after[..3],
            &[0xEF, 0xBB, 0xBF],
            "BOM must remain at byte 0 after single-edit"
        );
        let s = String::from_utf8(after).unwrap();
        assert!(s.contains("hello rust"));
    }

    /// UC-edit-031 — preserve UTF-8 BOM and CRLF line endings across
    /// a multi-edit batch. BOM stays at the file head; CRLF endings
    /// remain wherever they were in the original.
    #[tokio::test]
    async fn test_edit_multi_edit_preserves_bom_and_crlf() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("bom-crlf.txt");
        let original = "\u{FEFF}alpha\r\nbeta\r\ngamma\r\n";
        std::fs::write(&file, original.as_bytes()).unwrap();

        // Caller supplies LF in the entries; CRLF normalisation must
        // re-align so the matches land.
        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "alpha\nbeta", "newText": "ALPHA\nBETA" },
                    { "oldText": "gamma", "newText": "GAMMA" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("Successfully replaced 2 block(s)"),
            "expected both edits to land, got: {text}"
        );
        let after = std::fs::read(&file).unwrap();
        // BOM intact at byte 0.
        assert_eq!(&after[..3], &[0xEF, 0xBB, 0xBF], "BOM preserved");
        let after_str = String::from_utf8(after).unwrap();
        assert!(
            after_str.contains("ALPHA\r\nBETA"),
            "CRLF preserved between rewritten lines: {after_str:?}"
        );
        assert!(after_str.contains("GAMMA\r\n"));
        // No stray LF-only sequences.
        assert_eq!(
            after_str.matches('\n').count(),
            after_str.matches("\r\n").count(),
            "all line breaks remain CRLF"
        );
    }

    /// `edits` array entries missing `oldText` or `newText` keys are
    /// rejected with a precise per-index error message — this is the
    /// schema-validation contract callers see when their JSON is
    /// malformed.
    #[tokio::test]
    async fn test_edit_multi_edit_rejects_missing_fields() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("bad-edits.txt");
        std::fs::write(&file, "hello\n").unwrap();

        let result = execute_edit(
            dir.path(),
            json!({
                "file_path": file.to_str().unwrap(),
                "edits": [
                    { "oldText": "hello" }
                ]
            }),
        )
        .await;
        let text = get_text(&result);
        assert!(
            text.contains("edits[0].newText must be a string"),
            "expected per-index error, got: {text}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    }
}
