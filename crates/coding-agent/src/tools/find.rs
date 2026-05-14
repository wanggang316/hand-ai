//! Find tool — search for files by name/pattern.

use crate::tools::path_utils::resolve_to_cwd;
use hand_agent::types::{AgentTool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Default max results. Pi-mono ships 1000 — keep parity.
const DEFAULT_MAX_RESULTS: usize = 1000;

/// Path-component names auto-ignored by every find call. Pi-mono's `fd`-
/// backed tool skips `**/node_modules/**` and `**/.git/**`; we extend with
/// the common Rust/JS build outputs because find is meant to surface
/// *source* files for the model, not vendored or generated noise. A match
/// whose relative path begins with one of these names (followed by `/` or
/// EOL) is dropped.
const AUTO_IGNORE_NAMES: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
];

/// Create the find tool.
pub fn create_find_tool(cwd: PathBuf) -> AgentTool {
    AgentTool::simple(
        "find",
        "Search for files by glob pattern. Auto-ignores common build/VCS \
         directories (node_modules, .git, target, dist, build, .next, .cache). \
         Returns paths relative to the search directory.",
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
                    "description": "Maximum results to return (default: 1000)"
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

/// Return true if `relative` walks through one of the auto-ignored
/// directory names at any depth. Each component is checked literally —
/// a file *named* `.git` at the top level is fine (no separator after).
fn is_auto_ignored(relative: &str) -> bool {
    for comp in relative.split(['/', '\\']) {
        if AUTO_IGNORE_NAMES.iter().any(|n| *n == comp) {
            return true;
        }
    }
    false
}

fn execute_find(cwd: &Path, args: serde_json::Value) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolResult::error("Missing required parameter: pattern"),
    };

    let search_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_to_cwd(p, cwd))
        .unwrap_or_else(|| cwd.to_path_buf());

    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_RESULTS as u64) as usize;

    // Build the full glob pattern.
    //
    // Pi-mono parity (issue #3302): a basename-only pattern with no `/`
    // (e.g. `*.spec.ts`) should match at ANY depth in the search tree.
    // Without the auto-prepend a model that runs `find *.rs` only sees
    // top-level matches and misses the entire src/ subtree. Path-shaped
    // patterns (containing a `/`) stay anchored at the search root so
    // `src/**/*.spec.ts` only matches under the literal `src/`.
    let normalized = if !pattern.contains('/') && !pattern.starts_with('/') {
        format!("**/{}", pattern)
    } else {
        pattern.to_string()
    };
    let full_pattern = if normalized.starts_with('/') || normalized.contains(":/") {
        normalized
    } else {
        format!("{}/{}", search_path.display(), normalized)
    };

    match glob::glob(&full_pattern) {
        Ok(paths) => {
            let mut results: Vec<String> = Vec::new();
            for entry in paths {
                if results.len() >= max_results {
                    break;
                }
                if let Ok(path) = entry {
                    let relative = path
                        .strip_prefix(&search_path)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    if is_auto_ignored(&relative) {
                        continue;
                    }
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

    /// Pi-mono parity: build-output and VCS directories are auto-ignored.
    /// Pi's `fd` skips `**/node_modules/**` and `**/.git/**` by default;
    /// hand extends with target/dist/build/.next/.cache. The model
    /// expects clean source-only results from `**/*.rs` etc.
    #[test]
    fn test_find_auto_ignores_node_modules_and_git_and_target() {
        let dir = TempDir::new().unwrap();
        // Set up a representative mix of paths.
        for d in &[
            "src",
            "node_modules/foo",
            ".git/hooks",
            "target/debug/build",
            "dist",
            "build",
            ".next/cache",
        ] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("node_modules/foo/index.js"), "").unwrap();
        std::fs::write(dir.path().join("node_modules/foo/a.rs"), "").unwrap();
        std::fs::write(dir.path().join(".git/hooks/pre-commit"), "").unwrap();
        std::fs::write(dir.path().join("target/debug/build/junk.rs"), "").unwrap();
        std::fs::write(dir.path().join("dist/bundle.rs"), "").unwrap();
        std::fs::write(dir.path().join("build/out.rs"), "").unwrap();
        std::fs::write(dir.path().join(".next/cache/page.rs"), "").unwrap();

        let result = execute_find(dir.path(), json!({"pattern": "**/*.rs"}));
        let text = get_text(&result);
        assert!(
            text.contains("src/main.rs"),
            "real source must appear, got: {text}"
        );
        for ignored in [
            "node_modules/foo/a.rs",
            "target/debug/build/junk.rs",
            "dist/bundle.rs",
            "build/out.rs",
            ".next/cache/page.rs",
        ] {
            assert!(
                !text.contains(ignored),
                "auto-ignored path {ignored} leaked through, got: {text}"
            );
        }
    }

    /// Pure helper test for `is_auto_ignored`. Component-name match —
    /// any path with an ignored name in its components is dropped.
    #[test]
    fn test_is_auto_ignored_helper() {
        assert!(is_auto_ignored("node_modules/foo.js"));
        assert!(is_auto_ignored("a/b/.git/HEAD"));
        assert!(is_auto_ignored("target/debug/x"));
        // A bare component named like an ignored dir is also dropped —
        // the conventional interpretation is "this is the dir itself".
        assert!(is_auto_ignored("node_modules"));
        assert!(!is_auto_ignored("src/main.rs"));
        // `.gitignore` is a single literal filename; only the `.git`
        // *component* is ignored, not files that happen to share a prefix.
        assert!(!is_auto_ignored(".gitignore"));
        // The fragment `git` alone shouldn't trigger the `.git` match.
        assert!(!is_auto_ignored("git/log"));
    }

    /// Pi-mono parity (issue #3302): basename-only patterns like
    /// `*.spec.ts` must match files at ANY depth in the search tree.
    /// Pi-mono auto-prepends `**/` to patterns that don't contain a `/`
    /// so users get the conventional "find by basename" behavior without
    /// having to write `**/*.spec.ts` explicitly. Without this fix a
    /// model that does `find *.rs` only sees top-level files and misses
    /// the entire src/ tree.
    #[test]
    fn test_find_basename_pattern_matches_at_any_depth() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("top.spec.ts"), "").unwrap();
        std::fs::write(dir.path().join("a/mid.spec.ts"), "").unwrap();
        std::fs::write(dir.path().join("a/b/c/deep.spec.ts"), "").unwrap();
        std::fs::write(dir.path().join("noise.txt"), "").unwrap();

        let result = execute_find(dir.path(), json!({"pattern": "*.spec.ts"}));
        let text = get_text(&result);
        assert!(text.contains("top.spec.ts"), "top: {text}");
        assert!(text.contains("mid.spec.ts"), "mid: {text}");
        assert!(text.contains("deep.spec.ts"), "deep: {text}");
        assert!(!text.contains("noise.txt"));
    }

    /// Path-shaped patterns (containing `/`) must continue to work as
    /// rooted globs. Pi treats `src/**/*.spec.ts` as anchored at the
    /// search root, not at any depth.
    #[test]
    fn test_find_path_shaped_pattern_anchored_at_root() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/foo")).unwrap();
        std::fs::create_dir_all(dir.path().join("other/src/foo")).unwrap();
        std::fs::write(dir.path().join("src/foo/match.spec.ts"), "").unwrap();
        std::fs::write(dir.path().join("other/src/foo/skip.spec.ts"), "").unwrap();

        let result = execute_find(dir.path(), json!({"pattern": "src/**/*.spec.ts"}));
        let text = get_text(&result);
        assert!(text.contains("src/foo/match.spec.ts"), "{text}");
        // `src/` is anchored at root, so the nested src/ inside other/
        // must NOT match.
        assert!(
            !text.contains("other/src/foo/skip.spec.ts"),
            "anchored path shouldn't match nested src/: {text}"
        );
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
        // `--help` is taken as a literal glob pattern, not a CLI flag,
        // so it must produce a no-match result.
        assert!(
            text.contains("No files found") || text.contains("no files"),
            "expected no-match for --help glob, got: {text}"
        );
    }
}
