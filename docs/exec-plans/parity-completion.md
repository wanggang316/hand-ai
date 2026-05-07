# pi-mono → hand-ai Parity Completion Plan

> Master plan to bring `model` / `agent` / `coding-agent` / `tui` to functional
> parity with `pi-mono`. Supersedes earlier per-branch ExecPlans, which are
> marked DONE.
>
> **Status anchor:** main @ `f92d9ee` (post-review fixes for feat/{agent,coding-agent,tui}).
> All 1426 workspace tests pass. Build, clippy, and fmt are clean.

## Source-of-truth comparison

```
pi-mono                                 hand-ai                  status
packages/ai/                       →   packages/model/         ~95% (small OAuth gaps)
packages/agent/                    →   packages/agent/         ~98% (per-spec; M3 deferred)
packages/coding-agent/             →   packages/coding-agent/  ~50% (interactive mode missing)
packages/tui/                      →   packages/tui/           ~90% (Important review items)
```

Out of scope: `web-ui`, `pods`, `mom`, `bun/*` runtime — explicitly excluded
by the user.

## Verified remaining work

### A. coding-agent — the bulk

#### A1. Interactive mode subsystem (BLOCKS USER UX)

Source: `pi-mono/packages/coding-agent/src/modes/interactive/` (5493-line
`interactive-mode.ts` + 35 components in `components/` + theme assets).
Target: `packages/coding-agent/src/modes/interactive/` (does not exist).

| Component | TS LOC | Notes |
|---|---:|---|
| `interactive-mode.ts` | 5493 | Top-level driver — owns Tui, dispatches to components |
| `components/tree-selector.ts` | 1246 | Used for model + session pickers |
| `components/footer.ts` | medium | Status bar (already partially in hand-tui) |
| `components/{config,settings,oauth,theme,model,session,thinking}-selector.ts` | medium | Selector dialogs |
| `components/{user,assistant,custom,branch-summary,compaction-summary}-message.ts` | medium | Message renderers |
| `components/diff.ts`, `bash-execution.ts`, `tool-execution.ts` | medium | Per-event renderers |
| `components/login-dialog.ts`, `extension-{editor,input,selector}.ts` | medium | Auth + extension UX |
| `components/{custom-editor,visual-truncate,user-message-selector,bordered-loader,countdown-timer,dynamic-border,keybinding-hints,show-images-selector,skill-invocation-message}.ts` | small | Misc widgets |
| Vanity components (`armin.ts`, `daxnuts.ts`, `earendil-announcement.ts`) | tiny | Easter eggs — port last |

**Strategy:**
1. Port `interactive-mode.ts` as `modes/interactive/mod.rs` skeleton — wires
   Tui, registers focus, drives the event loop. Stub component slots.
2. Port message renderers + footer (smallest functional surface).
3. Port the selector family in priority order: model, session, settings,
   theme, OAuth.
4. Port editors (custom-editor, extension-{editor,input}).
5. Port login + tree-selector last (largest).
6. Vanity components last.

Acceptance: `cargo run -p hand-coding-agent -- ` (no flags) launches the TUI,
displays the model selector, and accepts user input. Each component has
unit tests for rendering output and key dispatch.

Estimated effort: 3000–5000 LOC Rust + tests, ~6–10 days of agent work.

#### A2. CLI helpers

Source: `pi-mono/packages/coding-agent/src/cli/` (6 files).

| TS file | Rust target | Acceptance |
|---|---|---|
| `config-selector.ts` | `cli/config_selector.rs` | First-run TUI for API key entry / OAuth selection |
| `file-processor.ts` | `cli/file_processor.rs` | `--files`/`--urls` arg → context messages |
| `initial-message.ts` | `cli/initial_message.rs` | Generate first user message from cwd context |
| `list-models.ts` | `cli/list_models.rs` | `--list-models` formatted output (replace bare `model_cli.rs`) |
| `session-picker.ts` | `cli/session_picker.rs` | TUI fuzzy session picker for `--continue` |
| `args.ts` | `cli/args.rs` | DONE; verify all flags wired |

Estimated: ~1500 LOC Rust, ~2 days.

#### A3. Utility modules

Source: `pi-mono/packages/coding-agent/src/utils/` (19 files). Hand-ai has
none of these as a `utils/` module — some functionality lives inline in
`core/git_utils.rs` etc.

Priority order:

**Tier 1 (small, foundational, no deps):**
- `sleep.ts` → trivial alias to `tokio::time::sleep`
- `paths.ts` → path resolution helpers (consolidate `tools/*::resolve_path`)
- `mime.ts` → MIME-type lookup (use `mime_guess` crate)
- `version-check.ts` → check crates.io for newer hand-ai version
- `pi-user-agent.ts` → generate User-Agent string for HTTP requests
- `changelog.ts` → CHANGELOG.md parsing
- `frontmatter.ts` → already partial; complete

**Tier 2 (medium, OS-specific):**
- `clipboard.ts` / `clipboard-native.ts` → use `arboard` crate
- `clipboard-image.ts` / `image-clipboard` → image read from clipboard
- `child-process.ts` → wrap `tokio::process::Command` with kill_on_drop tree
- `shell.ts` → cross-platform shell escape + exec

**Tier 3 (heavy, external deps):**
- `image-convert.ts` / `image-resize.ts` / `exif-orientation.ts` / `photon.ts`
  → use `image` + `kamadak-exif` crates
- `fs-watch.ts` → `notify` crate
- `tools-manager.ts` → external-tool installer (downloads prettier, eslint)
- `git.ts` → finish (origin URL parsing, current state already in `core/git_utils.rs`)

Estimated: ~2000 LOC Rust, ~3 days.

#### A4. Partial core/* completion

These files exist in hand-ai but are meaningfully smaller than their TS
counterparts. Each needs a diff-and-fill audit.

| File | Rust LOC | TS LOC | Ratio | Action |
|---|---:|---:|---:|---|
| `core/compaction.rs` | 284 | 1371 (dir) | 4.8× | Split into `core/compaction/{branch_summarization,compactor,utils}.rs`; port branch-summary logic |
| `core/session_manager.rs` | 593 | 1425 | 2.4× | Add fork/clone/migrate paths, search APIs |
| `core/package_manager.rs` | 189 | — | n/a | NOT a port target — see correction below. |
| `core/model_registry.rs` | 223 | 952 | 4.3× | Port custom-model registration, scoped overrides |
| `core/resource_loader.rs` | 608 | 918 | 1.5× | Port skills + docs path resolution |
| `core/model_resolver.rs` | 338 | 636 | 1.9× | Port scope-priority resolution + alias map |

Estimated: ~3000 LOC Rust, ~4 days.

##### Correction: `package_manager.rs` ↔ `package-manager.ts` is a name collision

These files share a name but have **disjoint purposes** — there is no port relationship between them.

- **TS `core/package-manager.ts`** (2428 lines) is a *pi-extension package-source registry*: resolves npm packages / git URLs / local paths into directory trees of pi extensions / skills / prompts / themes. Public surface includes `resolve()`, `install()`, `update()`, `removeFromSettings()`, `listConfiguredPackages()`. It depends on `parseGitUrl`, `canonicalizePath`, `isLocalPath`, `shouldUseWindowsShell`, plus `Settings.{packages,extensions,skills,prompts,themes}` fields that don't exist in the Rust `Settings` struct.

- **Rust `core/package_manager.rs`** (189 lines) is a *project-language detector*: returns `PackageManager::{Cargo,Npm,…}` / `Language::{Rust,TypeScript,…}` for project-introspection. No relationship to extension distribution.

The "12× ratio" framing in the original §A4 table was an artifact of identical filenames; treat the Rust file as a finished, separate utility.

The TS contract is still relevant if hand-ai needs to support pi-extension packages from remote sources. Track it as a **new** module:

- [ ] **Port pi-extension source registry** — TS source: `pi-mono/packages/coding-agent/src/core/package-manager.ts`. Rust target: `coding-agent/src/core/extensions/source_registry.rs` (alongside the existing `extensions/{api,dispatch,manifest,registry,subprocess}.rs`).
  - Prerequisites: `Settings` must gain `packages`, `extensions`, `skills`, `prompts`, `themes` fields with project + global setters; `utils/git.rs` must expose `parse_git_url` / `GitSource`. Both prerequisites are covered by Tier 2 utils + a small Settings extension.
  - Estimated: ~1500 LOC Rust + tests, ~2 days.

#### A5. Missing core/* modules

Source files in pi-mono `core/` with NO Rust counterpart:

- `output-guard.ts` — RPC stdout containment (intercept println! during RPC)
- `footer-data-provider.ts` — pluggable token/elapsed-time/model display
- `agent-session-runtime.ts` — high-level session orchestration
- `agent-session-services.ts` — service container injected into the session
- `event-bus.ts` — typed event broadcast (likely subsumed by the agent's listener API; verify)
- `messages.ts` — Message factory helpers (createCompactionSummaryMessage etc.)
- `provider-display-names.ts` — humanized provider strings
- `session-cwd.ts` — cwd persistence across resumes
- `resolve-config-value.ts` — env-var/setting fallback chain
- `defaults.ts` — default settings constants
- `sdk.ts` — top-level SDK assembly (likely subsumed by `lib.rs`; verify)
- `index.ts` — re-exports

Estimated: ~1500 LOC Rust, ~2 days.

### B. model — small OAuth gap

- Verify `oauth/pkce.rs`, `oauth/oauth_page.rs` against TS reference content;
  fill any missing helpers.
- `oauth/registry.rs` — confirm public `oauth_providers()` enumerator.

Estimated: ~200 LOC Rust, ½ day.

### C. agent — review follow-ups only

The reviewer's M3 omissions (AgentMessage::Custom, stream_proxy,
thinking_budgets/transport/session_id) are deferred per the original spec.
Apply the agent reviewer's Important findings before continuing:

- I1: doc-only — clarify `AbortHandle::abort()` semantics between runs
- I2: decide & document `Agent::abort()` return contract (`Ok` vs `Err`)
- I3: rewrite `examples/agent_abort.rs` to register a tool and demonstrate
  the documented `tool_execution_start` without `tool_execution_end`
- I4: doc-only — threading model on `Agent`

Estimated: ~50 LOC Rust changes + doc updates, ½ day.

### D. tui — Important review follow-ups

From the tui reviewer's report (already addressed: C-1, C-2, C-3 + I-2):

- I-1: capturing-overlay dispatch divergence — choose semantics (modal vs
  TS focused-routing) and document
- I-3: overlay border uses byte-len for width — switch to `visible_width`
- I-4: synchronous stdout writes on the async path — wrap in
  `spawn_blocking` or document the multi-thread-runtime requirement
- I-5: word-boundary helpers in editor walk raw bytes — switch to
  `unicode-segmentation` for grapheme-aware word movement
- S-2: `autocomplete_debounce_until` field never set — wire or remove

Estimated: ~150 LOC Rust changes, ½ day.

## Execution batches

Each batch is a self-contained ExecPlan suitable for `/hs-team`. Batches
are ordered by risk-adjusted dependency (smallest, lowest-risk first; then
foundation work; then interactive mode).

### Batch 1 — Cleanup (review follow-ups across all crates)

1. agent I-1/I-2/I-3/I-4 (doc + example)
2. tui I-1/I-3/I-4/I-5/S-2

Acceptance: review findings closed; tests still pass; clippy clean.

### Batch 2 — Tier 1 utils + model OAuth verification

Files to add:
- `coding-agent/src/utils/{sleep,paths,mime,version_check,pi_user_agent,changelog,frontmatter}.rs`

Audits:
- `model/src/oauth/{pkce,oauth_page,registry}.rs` — fill against TS reference

Acceptance: each file has rustdoc + unit tests; coding-agent compiles
with new utils accessible at `crate::utils::*`.

### Batch 3 — CLI helpers

- `cli/{config_selector,file_processor,initial_message,list_models,session_picker}.rs`
- Wire flags in `cli/args.rs` and dispatch in `main.rs`.

Acceptance: `--list-models`, `--files`, `--continue` end-to-end.

### Batch 4 — Tier 2 utils

- `clipboard.rs`, `clipboard_image.rs` (arboard)
- `child_process.rs`, `shell.rs`

Acceptance: e2e clipboard round-trip test (gated by display env), child
process tree-kill test on Unix.

### Batch 5 — Partial core/* completion

Audit each file in §A4 against its TS source; port missing logic.
Order: compaction → session_manager → model_registry/resolver →
package_manager → resource_loader.

### Batch 6 — Missing core/* modules (§A5)

### Batch 7 — Tier 3 utils (image processing, fs-watch, tools-manager)

### Batch 8 — Interactive mode skeleton

Port `modes/interactive/mod.rs` driver. Stub all 35 components as
placeholders that render their TS title.

### Batch 9 — Interactive components tier 1 (selectors)

model/session/settings/theme/oauth/thinking selectors.

### Batch 10 — Interactive components tier 2 (messages + executions)

user/assistant/custom messages, bash/tool/diff renderers.

### Batch 11 — Interactive components tier 3 (editors + auth)

custom-editor, extension-editor/input/selector, login-dialog, tree-selector.

### Batch 12 — Interactive components tier 4 (vanity)

armin/daxnuts/earendil-announcement, dynamic-border, countdown-timer,
keybinding-hints, etc.

### Batch 13 — End-to-end parity sweep

Re-run gap analysis against fully-ported tree; clean up dangling stubs;
ensure `cargo run -p hand-coding-agent` reaches the same screens as
`bun start` in pi-mono for the golden flows: list models, start session,
send message, fork, save, exit.

## Success criteria (the ruler)

For each batch:
1. `cargo build --workspace` passes.
2. `cargo test --workspace` passes (no skipped tests except those gated by
   missing system deps like X11 clipboard).
3. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
4. `cargo fmt --all -- --check` is clean.
5. New code has doctest or `#[test]`-style coverage.
6. Each ported TS module has at least one parity-style test verifying
   behavioral equivalence.
7. Public API surface follows `docs/conversion-guidelines.md` (snake_case,
   `Option<T>` for null, enums for discriminated unions, `Result<T, E>` for
   throws, `thiserror` for errors).

## Open questions parked for execution

- **OAuth flow**: how do we drive the local HTTP callback server during
  `Pkce` redemption? pi-mono uses `tiny-http`; hand-ai already pulls
  `tiny_http` into the model crate.
- **Database**: pi-mono uses Bun's SQLite for session indexing. Per
  guidelines, defer to JSONL + dir scanning until the indexing performance
  matters. Re-evaluate after Batch 13.
- **Theme assets**: pi-mono ships terminal images (`assets/`); hand-ai
  needs a shipping strategy (`include_bytes!`) for these in Batch 8.
