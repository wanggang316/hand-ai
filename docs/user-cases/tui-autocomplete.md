# User-Cases: tui/autocomplete

**Upstream source:** `pi-mono/packages/tui/test/autocomplete.test.ts` (25 cases)
**hand-ai source:**   `crates/tui/src/components/autocomplete.rs`
**Surface:**          `CombinedAutocompleteProvider` — routes the
editor's current cursor context (`AutocompleteContext`) to one of
several `AutocompleteProvider` implementations:
- **slash command** (`/he`, `/list`, etc.) — prefix-match against the
  registry of slash commands
- **`@path`** — fd-backed fuzzy file completion
- **dot-slash** (`./src/main.rs`) — preserves the `./` prefix
- **quoted paths** (`"path with spaces"`) — closes the quote properly
- **path prefix extraction** — finds the `/abs/...` or `./rel/...`
  span in the current line

## Major API delta

pi's autocomplete is backed by the host's `fd` binary (Rust crate
or system install). hand's `PathAutocompleteProvider` does a manual
BFS over the project tree with `DEFAULT_PATH_MAX_DEPTH = 3` and a
small auto-ignore list. The functional surface diverges:

| pi capability | hand support |
|---------------|--------------|
| fd-backed @ fuzzy file completion | partial — manual BFS, depth-capped, less fuzzy |
| Includes hidden paths but excludes .git | ⚠️ unverified — hand auto-ignores common build dirs |
| Follows symlinked directories | ❌ no |
| Quoting paths with spaces (auto-add `"`) | ❌ no |
| Continuing autocomplete inside quoted paths | ❌ no |
| Preserving `./` prefix on completion | ⚠️ unverified |

## Status

| ID | pi case | hand status |
|----|---------|-------------|
| UC-ac-001 | extracts `/` from `hey /` when forced | ⚠️ pending |
| UC-ac-002 | extracts `/A` from `/A` when forced | ⚠️ pending |
| UC-ac-003 | does NOT trigger for slash commands | ✅ pass — `test_slash_command_provider_*` covers prefix vs path triggers |
| UC-ac-004 | triggers for absolute paths after slash command arg | ⚠️ pending |
| UC-ac-005 | @-fd: returns all files+folders for empty `@` query | ⚠️ pending |
| UC-ac-006 | @-fd: matches file with extension in query | ⚠️ pending |
| UC-ac-007 | @-fd: case insensitive | ⚠️ pending |
| UC-ac-008 | @-fd: ranks directories before files | ⚠️ pending |
| UC-ac-009 | @-fd: returns nested file paths | ⚠️ pending |
| UC-ac-010 | @-fd: deeply nested paths | ⚠️ pending |
| UC-ac-011 | @-fd: dir-in-middle match (`--full-path`) | ❌ fail — hand has no equivalent flag |
| UC-ac-012 | @-fd: scopes to relative dirs, searches recursively | ⚠️ pending |
| UC-ac-013 | @-fd: quotes paths with spaces | ❌ fail — hand emits paths unquoted |
| UC-ac-014 | @-fd: includes hidden but excludes .git | ✅ pass — `test_path_provider_includes_dotfiles_excludes_git` |
| UC-ac-015 | @-fd: follows symlinked directories | ❌ fail — hand's BFS does not traverse symlinks |
| UC-ac-016 | @-fd: returns symlinked dirs matched by name | ❌ fail (same) |
| UC-ac-017 | @-fd: returns symlinked files without `type l` | ❌ fail (same) |
| UC-ac-018 | @-fd: same suggestions when cwd path contains the query | ⚠️ pending |
| UC-ac-019 | @-fd: continues autocomplete inside quoted `@` paths | ❌ fail |
| UC-ac-020 | @-fd: applies quoted `@` completion without duplicating quote | ❌ fail |
| UC-ac-021 | dot-slash: preserves `./` prefix when completing files | ⚠️ pending |
| UC-ac-022 | dot-slash: preserves `./` prefix for directory completions | ⚠️ pending |
| UC-ac-023 | quoted path: quotes paths with spaces (direct, not `@`) | ❌ fail |
| UC-ac-024 | quoted path: continues completion inside quoted paths | ❌ fail |
| UC-ac-025 | quoted path: applies quoted completion without duplicating quote | ❌ fail |

## Cases (load-bearing detail; rest are pinned by the table)

### UC-ac-003 — `/<cmd>` triggers the slash-command provider, NOT the path provider

**Given** the editor's current text is `/he`.
**When**  the user types another character or asks for suggestions.
**Then**  the suggestions come from the slash-command registry
(prefix match `/he` → `help`, `hotkeys`, etc.). The path provider
is NOT consulted; absolute paths starting with `/` do not pollute
the suggestion list.

- Probe: `cargo test -p hand-tui test_slash_command_provider_filters_by_prefix test_slash_command_provider_empty_query_returns_all -- --exact`.
- Why: the leading slash triggers two competing providers if not
  disambiguated. The combined provider routes by the explicit
  trigger enum (Slash | At | None), not by the raw character.

### UC-ac-005..018 — `@<query>` triggers the fd-backed file completion

**Given** the editor's text contains `@partial-query`.
**When**  autocomplete is requested.
**Then**  the suggestions are files/folders under the project root
matching `partial-query` fuzzy, with various ordering and inclusion
rules captured per case in the table above.

- Probe (largely PENDING / FAILING): hand's path provider does a
  manual BFS (depth ≤ 3) with an auto-ignore list. The pi tests
  exercise behaviour that depends on `fd` features (fuzzy, symlinks,
  hidden inclusion). Closing the gap requires either pulling in the
  `ignore` crate's WalkBuilder (used by ripgrep) or shelling out to
  `fd`.
- Resolution proposal: rebuild the path provider on top of
  `ignore::WalkBuilder` with `.follow_links(true)`, gitignore
  awareness, and fuzzy matching via `skim` or `fuzzy-matcher`.

### UC-ac-013/019/020/023/024/025 — quoted path support

**Given** a filename containing spaces (e.g.
`my documents/report.txt`).
**When**  the user types `@my doc` and tabs, or types
`"my docu` (already-quoted prefix).
**Then**  the completion auto-wraps the path in double quotes,
preserves the existing opening quote, does NOT double-emit the
closing quote, and continues completing inside the quoted span.

- Probe (FAILS): hand emits completions as raw paths; no quoting
  logic exists.
- Resolution proposal: in `PathAutocompleteProvider`, detect spaces
  in the candidate and wrap with `"..."`; in the consumer (editor),
  detect whether the cursor is already inside an open quote and
  splice accordingly.
