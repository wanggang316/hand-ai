# User-Cases: utils/paths

**Upstream source:** `pi-mono/packages/coding-agent/test/paths.test.ts` (10 cases — 5 `canonicalizePath`, 5 `isLocalPath`)
**hand-ai source:**   `crates/coding-agent/src/utils/paths.rs`
**Surface:**          `canonicalize_path(path) -> PathBuf` (best-effort, falls back to the input on failure) + `is_local_path(value) -> bool` (rejects `npm:` / `git:` / `github:` / `http:` / `https:` / `ssh:` prefixes; everything else is local).

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-paths-001 | ✅ pass | `canonicalize_existing_path` — a regular file resolves to its absolute canonical path |
| UC-paths-002 | ✅ pass | `canonicalize_follows_file_symlink_to_target` — file symlinks dereference to the target |
| UC-paths-003 | ✅ pass | `canonicalize_follows_directory_symlink_to_target` — directory symlinks dereference too |
| UC-paths-004 | ✅ pass | `canonicalize_nonexistent_path_returns_input` — missing target falls back to the raw input |
| UC-paths-005 | ✅ pass | `canonicalize_dangling_symlink_falls_back_to_input` — symlink whose target doesn't exist falls back |
| UC-paths-006 | ✅ pass | `local_paths_recognized` — bare names (`my-package`) count as local |
| UC-paths-007 | ✅ pass | `local_paths_recognized` — `./foo` relative paths count as local |
| UC-paths-008 | ✅ pass | `url_and_package_prefixes_are_not_local` — `npm:` rejected |
| UC-paths-009 | ✅ pass | `url_and_package_prefixes_are_not_local` — `git:` rejected |
| UC-paths-010 | ✅ pass | `url_and_package_prefixes_are_not_local` — `https:` rejected (and `github:` / `http:` / `ssh:`) |

## Bonus coverage hand carries beyond pi

- `canonicalize_empty_path_returns_input` — `""` is a no-op rather than an error.
- `leading_whitespace_is_trimmed_before_prefix_check` — `"   https://..."` still classified non-local.
- `unknown_protocol_is_treated_as_local` — `file:`/`ftp:` are NOT in the deny-list so they pass through; matches pi semantics exactly.

## Notes

- pi tests are TS / vitest; the `tempDir` lifecycle here is replaced by `tempfile::tempdir()` which auto-cleans on drop.
- Symlink tests are `#[cfg(unix)]` because Windows symlink creation requires elevated privileges; on Windows the runner skips them rather than asking for the privilege.

- Probe: `cargo test -p hand-coding-agent --lib utils::paths -- --exact`.
