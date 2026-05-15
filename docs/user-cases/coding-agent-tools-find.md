# User-Cases: tools/find

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts` (find-tool describe)
**hand-ai source:**   `crates/coding-agent/src/tools/find.rs`
**Surface:**          The `find` tool exposed to the model — glob over files,
auto-ignoring noisy build/VCS directories. pi backs this with `fd` (which
respects `.gitignore`); hand uses pure-Rust `glob` with a hard-coded
auto-ignore list. The behavioural delta is captured in UC-find-002 below.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-find-001 | ✅ pass | `test_find_files`, `test_find_recursive`, `test_find_basename_pattern_matches_at_any_depth` |
| UC-find-002 | ✅ pass | `test_find_respects_gitignore` |
| UC-find-003 | ✅ pass | `test_find_invalid_glob_returns_error` |
| UC-find-004 | ✅ pass | `test_find_flag_pattern_treated_as_glob_literal` |
| UC-find-005 | ✅ pass | `test_find_auto_ignores_node_modules_and_git_and_target` |
| UC-find-006 | ✅ pass | `test_find_basename_pattern_matches_at_any_depth` |
| UC-find-007 | ✅ pass | `test_find_path_shaped_pattern_anchored_at_root` |
| UC-find-008 | ✅ pass | `test_find_no_matches` |

## Cases

### UC-find-001 — hidden files (dotfiles) are included when not gitignored

**Given** a test directory containing a top-level `visible.txt` and a
hidden `.secret/hidden.txt`.
**When**  the user invokes the find tool with pattern `**/*.txt`.
**Then**  the output contains BOTH `visible.txt` and `.secret/hidden.txt`
— a `.secret` directory is not part of the auto-ignore list.

- Assertion: `visible.txt` appears in the output.
- Assertion: `.secret/hidden.txt` appears in the output.
- Probe: `cargo test -p hand-coding-agent test_find_files test_find_recursive -- --exact`.
- Note: hand explicitly drops only `node_modules`, `.git`, `target`,
  `dist`, `build`, `.next`, `.cache`. Other dotdirs (`.secret`,
  `.config`, etc.) are kept by design — see UC-find-005.

### UC-find-002 — `.gitignore` suppresses matched files

**Given** a test directory containing `.gitignore` with `ignored.txt`,
plus `ignored.txt` and `kept.txt`.
**When**  the user invokes the find tool with pattern `**/*.txt`.
**Then**  the output contains `kept.txt` but NOT `ignored.txt`.

- Assertion: `kept.txt` appears in the output.
- Assertion: `ignored.txt` does NOT appear in the output.
- Probe: `cargo test -p hand-coding-agent test_find_respects_gitignore -- --exact`.
- Resolution: `tools/find.rs` now walks the tree via
  `ignore::WalkBuilder` (the same crate ripgrep uses). The walker
  reads `.gitignore`, `.ignore`, `.git/info/exclude`, and the global
  git ignore. `require_git(false)` is set so `.gitignore` is honoured
  even when the tree isn't a git repo. The hard-coded auto-ignore
  list (UC-find-005) layers on top for build outputs that may not
  appear in any `.gitignore`.

### UC-find-003 — invalid glob patterns surface a clean error, not a panic

**Given** a test directory.
**When**  the user invokes the find tool with the unbalanced pattern `[`.
**Then**  the tool returns a `ToolResult` whose text contains a
glob-parse-error string (e.g. `Invalid glob pattern`), and does NOT
panic or hang.

- Assertion: the result's text contains `Invalid glob` (case-insensitive
  match).
- Assertion: the call returns within ~1 s.
- Probe: `cargo test -p hand-coding-agent test_find_invalid_glob_returns_error -- --exact`.

### UC-find-004 — flag-shaped patterns (`--help`) are treated as literal globs, not CLI flags

**Given** a test directory containing only `normal.txt`.
**When**  the user invokes the find tool with pattern `--help`.
**Then**  the tool returns a "no files found" message. It MUST NOT
shell out to an external binary with `--help` as the flag, MUST NOT
print help text, and MUST NOT execute `--help` as a glob-parser
directive.

- Assertion: the output contains the string `No files found`.
- Assertion: no panic, no subprocess invocation, no help text.
- Probe: `cargo test -p hand-coding-agent test_find_flag_pattern_treated_as_glob_literal -- --exact`.

### UC-find-005 — common build / VCS output directories are auto-ignored

**Given** a test directory containing the mix:
`src/main.rs`, `node_modules/foo/a.rs`, `.git/hooks/pre-commit`,
`target/debug/build/junk.rs`, `dist/bundle.rs`, `build/out.rs`,
`.next/cache/page.rs`.
**When**  the user invokes the find tool with pattern `**/*.rs`.
**Then**  the output contains `src/main.rs` only — every other path is
dropped because its leading directory component is in the auto-ignore
list: `node_modules`, `.git`, `target`, `dist`, `build`, `.next`,
`.cache`.

- Assertion: `src/main.rs` appears in the output.
- Assertion: none of the auto-ignored paths appear in the output.
- Probe: `cargo test -p hand-coding-agent test_find_auto_ignores_node_modules_and_git_and_target -- --exact`.

### UC-find-006 — basename-only patterns match files at any depth

**Given** a tree with `top.spec.ts`, `a/mid.spec.ts`,
`a/b/c/deep.spec.ts`, and a noise file `noise.txt`.
**When**  the user invokes the find tool with pattern `*.spec.ts` (no
slashes, no leading `**/`).
**Then**  all three `.spec.ts` files appear in the output; `noise.txt`
does not. Hand auto-prepends `**/` to basename-only patterns so the
search behaves like a conventional "find by basename".

- Assertion: `top.spec.ts`, `a/mid.spec.ts`, `a/b/c/deep.spec.ts` all
  appear.
- Assertion: `noise.txt` does NOT appear.
- Probe: `cargo test -p hand-coding-agent test_find_basename_pattern_matches_at_any_depth -- --exact`.

### UC-find-007 — path-shaped patterns are anchored at the search root

**Given** a tree with `src/foo/match.spec.ts` and
`other/src/foo/skip.spec.ts`.
**When**  the user invokes the find tool with pattern `src/**/*.spec.ts`
(contains a `/`).
**Then**  the output contains `src/foo/match.spec.ts` only — the leading
`src/` is anchored at the search root, so the nested `src/` inside
`other/` does NOT match.

- Assertion: `src/foo/match.spec.ts` appears.
- Assertion: `other/src/foo/skip.spec.ts` does NOT appear.
- Probe: `cargo test -p hand-coding-agent test_find_path_shaped_pattern_anchored_at_root -- --exact`.

### UC-find-008 — a pattern that matches nothing returns a clean "no files found"

**Given** any test directory.
**When**  the user invokes the find tool with a pattern that matches
nothing (e.g. `*.nonexistent`).
**Then**  the output is exactly `No files found matching the pattern.`
(or contains that string), not an empty result and not a panic.

- Assertion: the output contains `No files found`.
- Probe: `cargo test -p hand-coding-agent test_find_no_matches -- --exact`.
