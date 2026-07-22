---
name: release
description: Cut a hand-ai workspace release — version bump, CHANGELOG finalization, release commit, tag, and release-workflow verification. Use when the user asks to release/cut/tag a version (e.g. "/release 0.3.1", "release 0.4.0").
---

# Release Skill

Cut a unified workspace release. Since 0.3.0 every crate shares one version declared in the root `[workspace.package]`; a release is one version bump, one CHANGELOG section, one commit on `main`, and one `vX.Y.Z` tag whose push triggers `.github/workflows/release.yml` (darwin-arm64 + linux-x86_64 tarballs, SHA-256 checksums, GitHub Release).

## When to Use

Only when the user explicitly asks to release a version: `/release <version>`, "release X.Y.Z", "cut X.Y.Z", "tag X.Y.Z". The version number is the user's decision — never re-litigate it (no "this should really be a minor bump" pushback; at most note semver expectations in one sentence and proceed).

**Don't use** for `model`-crate-only tag rituals from the pre-0.3.0 era (`model-v*` tags are historical), or for publishing to crates.io (nothing in this workspace publishes there).

## Preconditions (verify, abort with findings if violated)

1. On `main`, synced with `origin/main` (`git fetch` + compare). Tracked working tree clean except for the release edits themselves; untracked files are fine.
2. Tag `v<version>` does not already exist (local or remote). An existing tag is a hard abort — never re-tag or force-move a released tag.
3. `<version>` is `X.Y.Z` and greater than the current `[workspace.package] version` in the root `Cargo.toml`.
4. Latest CI on `main` (or the merge commits comprising the release) is green. If unverifiable, run the full local gate (step 4) before committing anyway — it is mandatory either way.

## Workflow

### 1. Establish the range

`PREV=$(git describe --tags --abbrev=0 --match 'v*')` and review `git log $PREV..HEAD --oneline`. This is what ships; the CHANGELOG section must be an honest summary of its user-perceivable part.

### 2. Finalize CHANGELOG.md

- Rename `## [Unreleased]` to `## [<version>] - <date>` — date from `date "+%F"`, never from memory. The parser only accepts that header shape; entries left under `[Unreleased]` are invisible to `/changelog` and the startup banner.
- Audit the range from step 1 for user-perceivable `hand` changes missing from the section (fixes and features often land without entries when they live in the tui/agent/model crates but still change what users see). Add concise entries under the proper subsection (`### Added` / `### Fixed` / …) with PR links in the established format. Library-internal changes stay out.
- Never touch already-released sections.

### 3. Bump the version

Root `Cargo.toml` `[workspace.package] version` only — every crate inherits it. Run `cargo check --workspace --features model/faux` afterward so the build graph re-resolves cleanly.

### 4. Full gate (mandatory, evidence required)

`./check.sh` at the repo root, plus `cargo clippy --workspace --all-targets --features model/faux -- -D warnings` and `cargo fmt --all -- --check` if `check.sh` doesn't cover them, plus `cargo test -p hand-agent --features sqlite`. All green or the release stops here.

### 5. Release commit on main

```
git add Cargo.toml CHANGELOG.md
git commit -m "release(workspace): cut <version>

<3-6 line summary of what ships: headline features, notable fixes.
Technical prose, no emoji, no attribution trailers.>"
git push origin main
```

Wait for CI on this commit to go green before tagging (the tag should point at a verified commit).

### 6. Tag and push

```
git tag -a v<version> -m "Release <version> — <one-line headline>"
git push origin v<version>
```

### 7. Verify the release workflow

- Watch the `Release` workflow run for the tag (`gh run list --workflow release.yml`, then watch the run) until completion.
- On success, verify the GitHub Release exists with all four assets: two `.tar.gz` (darwin-arm64, linux-x86_64) and their `.sha256` files (`gh release view v<version>`).
- Verify the release body is the CHANGELOG `[<version>]` section — the workflow extracts it via `body_path` and fails when the section is missing; it must never be an auto-generated PR/commit list. If the body is wrong anyway, fix with `gh release edit v<version> --notes-file <file>` and treat it as a workflow regression to investigate.
- On failure: report the failing job with its log excerpt and stop. Do NOT delete the tag or the partial release; fixing forward (new commit, new patch version if the tag content itself is wrong) is the recovery path — a pushed tag is public history.

### 8. Report

Version, release commit SHA, tag, workflow result, release URL, asset list, and the CHANGELOG section as shipped.

## Red Flags (stop immediately)

- Re-tagging, tag deletion, or force-push of anything already on the remote
- Editing an already-released CHANGELOG section
- Tagging a commit whose checks were not actually run and green
- Fabricating CHANGELOG entries for changes that didn't ship in the range
- Leaving the release half-done silently (bumped but untagged, tagged but unverified) — always report the exact stopping point

## Rationalization Table

| Excuse | Reality |
|---|---|
| "CI passed on all the PRs, skip the local gate." | Merge combinations break in ways per-PR CI can't see. The gate is cheap; a broken tag is not. |
| "The CHANGELOG can be fixed after tagging." | Released sections are immutable and the tarball ships the file. Finalize first. |
| "Just delete the bad tag and re-push." | Consumers may have fetched it. Fix forward with a new patch version. |
| "The user probably meant the next minor." | The version is the user's call. Ship what was asked. |
