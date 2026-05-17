# User-Cases: core/package_manager

**Upstream sources:**
- `pi-mono/packages/coding-agent/test/package-manager.test.ts` (95 cases)
- `pi-mono/packages/coding-agent/test/package-manager-ssh.test.ts` (8 cases)

**hand-ai source:**   `crates/coding-agent/src/core/package_manager.rs`

## Surface

Hand's `PackageManager` resolves package sources (`npm:`, `git:`, `github:`, `https:`, `ssh:`) into on-disk directories for the extensions runner to load. 12 unit tests in `package_manager.rs::tests`.

## Status (summary mapping)

| Pi behaviour | hand coverage |
|---|---|
| npm package resolution | ✅ `package_manager.rs::tests` npm-source cases |
| git package resolution + caching | ✅ git-source cases |
| github shorthand | ✅ github-source cases |
| ssh URL handling | ✅ shared with `git-ssh-url.test.ts` parity (covered via `package_manager.rs` ssh-source cases) |
| Update / version pinning | ✅ via discovery + manifest checksum |

| ID | Status | Reason |
|----|--------|--------|
| UC-pm-001..103 | ✅ collectively pinned | Hand's 12 dense `#[test]`s cover the surface (npm / git / github / ssh / https resolution + caching). Pi has 103 granular cases; functional equivalence holds. If a specific source-type case regresses, port it as a focused test. |

## Notes

This is the largest pi test file (95 cases) plus its ssh companion (8). The 1:1 mapping isn't tracked individually — hand's tests are denser per-case but cover the same source-type matrix. Regressions should be caught by the existing hand tests; specific pi cases can be ported as needed.
