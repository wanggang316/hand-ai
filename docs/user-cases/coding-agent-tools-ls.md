# User-Cases: tools/ls

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts` (ls-tool describe)
**hand-ai source:**   `crates/coding-agent/src/tools/ls.rs`
**Surface:**          The `ls` tool — list directory entries with size,
sorted directories-first, hidden entries included. The model relies on
seeing `.gitignore`, `.env.local`, `.hand/` etc., so dotfiles MUST
surface (unlike a default `ls`).

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-ls-001 | ✅ pass | `test_ls_lists_dotfiles_and_dotdirs` |
| UC-ls-002 | ✅ pass | `test_ls_basic` |
| UC-ls-003 | ✅ pass | `test_ls_dirs_first` |
| UC-ls-004 | ✅ pass | `test_ls_empty_dir` |
| UC-ls-005 | ✅ pass | `test_ls_nonexistent` |

## Cases

### UC-ls-001 — dotfiles and dot-directories are listed (Unix `ls -a` style)

**Given** a test directory containing:
- a hidden file `.hidden-file` (any content),
- a hidden directory `.hidden-dir`,
- any number of normal entries.
**When**  the user invokes the ls tool with `path=<test-dir>`.
**Then**  the output includes both `.hidden-file` and `.hidden-dir/`
(the trailing slash marks the dir).

- Assertion: the output contains `.hidden-file`.
- Assertion: the output contains `.hidden-dir/`.
- Probe: `cargo test -p hand-coding-agent test_ls_lists_dotfiles_and_dotdirs -- --exact`.

### UC-ls-002 — regular files appear with their sizes

**Given** a test directory with a regular file `foo.txt` containing
`hello`.
**When**  the user invokes the ls tool.
**Then**  `foo.txt` appears in the output, accompanied by a size annotation
(e.g. `5B`, formatted via the byte-size helper).

- Assertion: the output contains `foo.txt`.
- Assertion: the output contains a recognisable size token near the
  filename.
- Probe: `cargo test -p hand-coding-agent test_ls_basic -- --exact`.

### UC-ls-003 — directories sort before files

**Given** a directory containing a subdir `a_dir/` and a file
`z_file.txt`, where alphabetical sorting would put `a_dir` first
anyway BUT separately, with `z_dir/` and `a_file.txt`, the dir still
goes first.
**When**  the user invokes the ls tool.
**Then**  every directory entry appears in the output BEFORE every
regular-file entry — directory-first sort overrides alphabetical sort.

- Assertion: the position of the directory entry precedes the position
  of the file entry.
- Probe: `cargo test -p hand-coding-agent test_ls_dirs_first -- --exact`.

### UC-ls-004 — listing an empty directory returns a clean empty marker

**Given** an empty directory.
**When**  the user invokes the ls tool.
**Then**  the output contains a marker such as `(empty)` (or returns no
entries cleanly) — no panic, no spurious entries.

- Assertion: the output renders cleanly with no entries listed.
- Probe: `cargo test -p hand-coding-agent test_ls_empty_dir -- --exact`.

### UC-ls-005 — listing a non-existent path returns a clean error

**Given** a path that does not exist on disk.
**When**  the user invokes the ls tool.
**Then**  the output is an error message; no panic.

- Assertion: the result is an error-typed `ToolResult`.
- Probe: `cargo test -p hand-coding-agent test_ls_nonexistent -- --exact`.
