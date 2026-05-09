# ExecPlan: Port `pi-coding-agent` (TypeScript) → `hand-coding-agent` (Rust)

**Status:** Draft
**Author:** Gump (drafted by Claude Opus 4.7)
**Date:** 2026-05-06

This is a living document. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective up to date as work proceeds. Every stopping point — even partial — must be reflected in Progress.

---

## Purpose

Today, `crates/coding-agent` is a thin REPL on top of `hand-agent` and `model`: 7 file/shell tools work, JSONL session persistence works, compaction runs. After this plan lands, a developer using `cargo run --bin hand` will have feature parity with `pi-coding-agent` for the four user-visible jobs that matter most:

1. Drive an agent **headlessly** via JSONL RPC, so `hand` is usable as an SDK backend and integration-testable without a TUI (Phase 1).
2. **Customize prompts and behavior at runtime** — discover Skills (markdown plugins) and prompt templates from disk, no recompile (Phase 2).
3. **Load extensions** that hook into the agent loop (before/after tool, slash commands, custom tools), via a Rust-friendly extension surface that does not require a JS engine in-process (Phase 3).
4. **Configure** themes, keybindings, retry/compaction behavior via per-user and per-project YAML files; resolve OAuth/auth via persisted credentials (Phases 4 + 6).
5. Use a **real interactive TUI** with selectors, diff rendering, message tree, and editor — built on `hand-tui` (Phase 5).

Phases ship in order; each one is independently usable and testable. Phase 1 alone unblocks SDK consumers; Phase 5 is what end-users see.

## Progress

Update at the start and end of every working session. Timestamp each state change.

- [ ] **Phase 0 — Foundation cleanup** (target: 2 days)
  - [ ] T0.1 Audit `main.rs` and split CLI/args into `cli/args.rs`
  - [ ] T0.2 Promote `core/mod.rs` to expose a stable `prelude`
  - [ ] T0.3 Add `tests/common/` mock harness (mock provider, in-mem session)
- [ ] **Phase 1 — Headless RPC mode** (target: 5 days)
  - [ ] T1.1 Define `rpc/types.rs` (request/response/event tagged enums)
  - [ ] T1.2 Implement `rpc/jsonl.rs` framed stdin/stdout codec
  - [ ] T1.3 Implement `rpc/server.rs` dispatcher over `AgentSession`
  - [ ] T1.4 Add `--rpc` mode wiring in `main.rs`
  - [ ] T1.5 Migrate `print` mode to share the same dispatcher core
  - [ ] T1.6 Integration tests against a fixture client
- [ ] **Phase 2 — Skills + prompt templates** (target: 4 days)
  - [ ] T2.1 `utils/frontmatter.rs` (YAML frontmatter parser)
  - [ ] T2.2 `core/resource_loader.rs` + `source_info.rs`
  - [ ] T2.3 `core/skills.rs` (SKILL.md discovery, validation, dedup)
  - [ ] T2.4 `core/prompt_templates.rs` (template parse + variable substitution)
  - [ ] T2.5 Wire skills/templates into `system_prompt::build_system_prompt`
- [ ] **Phase 3 — Extensions runtime** (target: 8 days)
  - [ ] T3.1 **ADR-001** Extensions runtime selection (writeup + decision)
  - [ ] T3.2 Define `extensions/api.rs` (Hook trait, ExtensionContext)
  - [ ] T3.3 Implement chosen runtime (subprocess JSON-RPC OR WASM)
  - [ ] T3.4 Wire hooks into `AgentSession::send_message`
  - [ ] T3.5 Slash-command + custom-tool registration via extensions
  - [ ] T3.6 Port 3 example extensions as fixtures (`hello`, `confirm-destructive`, `notify`)
- [ ] **Phase 4 — Settings + keybindings** (target: 3 days)
  - [ ] T4.1 YAML schema + parser for `settings.yaml`
  - [ ] T4.2 Per-project overlay + reload-on-change
  - [ ] T4.3 Keybindings file format + customization
  - [ ] T4.4 Migration: convert legacy JSON settings if found
- [ ] **Phase 5 — Interactive TUI v1** (target: 12 days)
  - [ ] T5.1 **ADR-002** TUI architecture (hand-tui capability audit + component contract)
  - [ ] T5.2 Layout shell: messages pane / editor / status bar
  - [ ] T5.3 Editor with multi-line + paste + slash trigger
  - [ ] T5.4 Message renderer (user/assistant/tool, with diff highlight)
  - [ ] T5.5 Selector primitives (model, session, settings)
  - [ ] T5.6 Streaming render loop bound to `AgentSessionEvent`
  - [ ] T5.7 Slash-command palette + keybinding hints
- [ ] **Phase 6 — Auth + telemetry** (target: 4 days)
  - [ ] T6.1 `core/auth_storage.rs` (encrypted at-rest token store)
  - [ ] T6.2 `core/auth_guidance.rs` (provider-specific setup hints)
  - [ ] T6.3 OAuth flow stub (Anthropic, GitHub Copilot — minimal)
  - [ ] T6.4 `core/telemetry.rs` + `core/timings.rs` (opt-in tracing spans)

## Surprises & Discoveries

(None yet.)

## Decision Log

(None yet — Phase 3 ADR and Phase 5 ADR will land here as their dedicated tasks complete.)

## Outcomes & Retrospective

(To be filled at milestone completion.)

## Context and Orientation

**Source of truth (TypeScript):** `/Users/wanggang/dev/opensource/pi-mono/packages/coding-agent` (~45.6k LOC, npm package `@mariozechner/pi-coding-agent`). Don't read it linearly — use it as a behavior reference for individual subsystems as we port them.

**Target (Rust, this repo):** `crates/coding-agent` — currently ~7.3k LOC. The crate exports `AgentSession`, 7 builtin tools, JSONL session manager, basic compaction.

**Conversion guidelines:** `docs/conversion-guidelines.md`. Treat as authoritative for type mappings (`Option<T>` for optional fields, tagged enums for unions, `thiserror` for crate errors, `tokio` for async, snake_case identifiers + `#[serde(rename_all = "camelCase")]` where the JSON wire format must match TS). Do not mechanically translate JS idioms — see Section 15 of that doc.

**Sibling crates we depend on (do not change without an explicit task):**

- `crates/model` — LLM provider abstraction. Exposes `Client`, `Model`, `Context`, `AssistantMessageEvent`, `SimpleStreamOptions`. Stable.
- `crates/agent` — Agent runtime. Exposes `AgentLoopConfig`, `AgentTool`, `AgentEvent`, `AgentEventSink`, `agent_loop::run_agent_loop`. Stable.
- `crates/tui` — Terminal UI primitives (used in Phase 5). Capability needs to be audited in T5.1.

**Existing Rust files relevant to this plan (full paths, what they do):**

- `crates/coding-agent/src/main.rs` — CLI + interactive readline loop + print-mode driver. 770 LOC, will be split in T0.1.
- `crates/coding-agent/src/core/agent_session.rs` — `AgentSession` orchestrates loop, persistence, compaction, and event fan-out. The pivot point for Phases 1, 3, 5.
- `crates/coding-agent/src/core/session_manager.rs` — JSONL append-only session, fork/branch surface partial.
- `crates/coding-agent/src/core/compaction.rs` — token estimation, split, prompt build, LLM-based summary. Branch summarization missing.
- `crates/coding-agent/src/core/extensions/{loader,runner,types,wrapper}.rs` — types-only scaffold today; gets a real runtime in Phase 3.
- `crates/coding-agent/src/core/system_prompt.rs` — system prompt builder; gets skills + templates injected in Phase 2.
- `crates/coding-agent/src/core/slash_commands.rs` — command enum + dispatch hook; expands in Phase 3 and 5.
- `crates/coding-agent/src/tools/*.rs` — 7 tools. `edit.rs` needs multi-edit + diff in Phase 5 (consumed by message renderer).
- `crates/coding-agent/src/lib.rs` — public re-exports. Each phase appends one or two re-exports; keep tidy.

**Repo-level docs we must respect:**

- `Agents.md` — change-quality bar (`./check.sh`, no warnings, atomic commits, no `git add -A`).
- `docs/conversion-guidelines.md` — type/idiom mapping.
- `docs/conversion-plan.md` — workspace-level roadmap (this plan refines its Phase 3.x for coding-agent).

**Terms used in this plan:**

- **JSONL RPC** — line-delimited JSON-RPC 2.0 over stdin/stdout. Frame = exactly one JSON object per line. Used for headless mode in Phase 1, and (decision pending) possibly for extensions in Phase 3.
- **Skill** — a markdown file `SKILL.md` whose YAML frontmatter declares `name`, `description`, `disable-model-invocation` (etc.); body is appended to the system prompt under a discovery section. Loaded from `~/.hand/skills/`, `<cwd>/.hand/skills/`, plus packaged defaults.
- **Extension** — a user-installable plugin that registers slash commands, tools, or hook callbacks. In TS these are `.js` modules; the Rust shape is decided in ADR-001.
- **ADR** — Architecture Decision Record. Saved under `docs/adr/NNNN-<slug>.md`. Both ADRs in this plan are first-class tasks, not throwaway notes.
- **Compaction** — replacing oldest messages with an LLM-written summary when the context window approaches a threshold.

**How the parts fit together:** A user invocation enters via `main.rs`, which constructs `AgentSession` (Phase 0/1) and dispatches to one of three modes — `interactive` (TUI; Phase 5), `print` (one-shot; today), or `rpc` (headless JSONL; Phase 1). All three drive the same `AgentSession::send_message` loop. The session reads context files + skills + templates (Phase 2) into the system prompt, applies extension hooks (Phase 3) before/after tool calls, persists to JSONL (today), and emits events to subscribers. Settings (Phase 4) and auth/telemetry (Phase 6) sit below the session as supporting services.

## Plan of Work

The work is sliced **vertically per phase**: each phase delivers a complete user-observable capability end-to-end, rather than building horizontal layers in isolation. Within a phase, tasks are sized so that each touches at most ~5 files and lands in a single commit. Tasks marked **(parallel)** can run on a separate worktree concurrently with peers in the same phase.

### Phase 0 — Foundation cleanup

**Why first:** Everything else needs a clean place to plug in. `main.rs` at 770 LOC mixes args parsing, mode dispatch, and the interactive REPL — splitting it now keeps every later phase's diff small.

**At the end:** `main.rs` is < 200 LOC and routes to `cli::args::Args` + a mode dispatcher; `tests/common/` has a `MockTextProvider` and `MockToolProvider` for use across all later phases; `cargo test -p hand-coding-agent` still reports the same green count.

| ID | Title | Scope (files) | TDD focus | Acceptance |
|---|---|---|---|---|
| T0.1 | Extract CLI args | `src/cli/mod.rs`, `src/cli/args.rs`, slim `src/main.rs` | Unit-test `Args::parse_from_iter` covering `--rpc`, `--print`, `-p`, `--model`, `--tools`, `--no-tools` | `cargo test -p hand-coding-agent cli::args` ≥ 6 cases pass |
| T0.2 | Stable prelude + lib re-exports | `src/lib.rs`, `src/core/mod.rs` | Compile test that downstream consumer can import only via `hand_coding_agent::prelude::*` | `cargo build` clean; one doc-test in `lib.rs` |
| T0.3 | Test harness | `tests/common/mod.rs`, `tests/common/mocks.rs` | Provide `mock_text_provider`, `mock_tool_provider`, `temp_session_dir`. Mirror sibling `crates/agent/tests/common` patterns. | One smoke test using the harness exercises `AgentSession::in_memory` + a mocked turn |

**Integration points unchanged.** No new public API on `model` or `agent`.

### Phase 1 — Headless RPC mode

**Why next:** Without RPC, every later phase has no clean way to integration-test the agent end-to-end (interactive can't be scripted; print is fire-and-forget). RPC lets us drive a session from Rust tests and external SDK clients.

**At the end:** `cargo run --bin hand -- --rpc` reads JSON-RPC 2.0 requests from stdin and writes events + responses to stdout, one JSON object per line. Methods: `session.create`, `session.send_message` (returns immediately, streams events), `session.cancel`, `session.compact`, `session.get_state`. A fixture client in `tests/rpc_smoke.rs` drives a mocked turn end-to-end.

Reference for protocol shape: `pi-mono/packages/coding-agent/src/modes/rpc/{rpc-types.ts,rpc-mode.ts,rpc-client.ts,jsonl.ts}`. Match wire-format field names exactly (use `#[serde(rename_all = "camelCase")]`).

| ID | Title | Scope (files) | TDD focus | Acceptance | Depends on |
|---|---|---|---|---|---|
| T1.1 | RPC types | `src/modes/rpc/mod.rs`, `src/modes/rpc/types.rs` | Roundtrip serde tests for every request/response/event variant against captured TS payloads | All variants compile + roundtrip; `#[serde(tag="method")]` for requests | T0.2 |
| T1.2 | JSONL codec **(parallel with T1.3 design)** | `src/modes/rpc/jsonl.rs` | Property test: arbitrary JSON values survive frame→parse roundtrip; partial-line buffering across reads | Stream `impl Stream<Item = Result<RpcRequest, RpcError>>` from any `AsyncBufRead` | T1.1 |
| T1.3 | RPC dispatcher | `src/modes/rpc/server.rs` | Mock-driven test: `session.send_message` emits start/delta/end/done events in order; `session.cancel` halts within one turn | Dispatcher owns one `AgentSession` and serializes events from it | T1.1, T1.2 |
| T1.4 | `--rpc` wiring | `src/main.rs`, `src/cli/args.rs` | Test that `--rpc` and `--print` are mutually exclusive | `hand --rpc` enters loop; SIGINT closes cleanly | T1.3 |
| T1.5 | Refactor print to share dispatcher | `src/modes/print.rs` (new), trim `main.rs` print path | Existing print-mode tests still pass | No regression in `cargo test -p hand-coding-agent` | T1.4 |
| T1.6 | E2E RPC integration | `tests/rpc_smoke.rs` | Spawn `hand --rpc` as subprocess (or use in-process dispatcher), run a 3-turn scripted session, assert event sequence | Test passes in CI without API keys (mocked provider) | T1.5 |

**Integration points:** No `model` or `agent` API changes. New public API: `hand_coding_agent::rpc::{RpcRequest, RpcResponse, RpcEvent, run_rpc_server}`.

### Phase 2 — Skills + prompt templates

**Why now:** Skills + templates are pure data; they need no extensions runtime. Landing them now lets users customize prompts immediately, and removes a dependency from Phase 5 (interactive mode that exposes skills via slash command).

**At the end:** A `SKILL.md` placed under `<cwd>/.hand/skills/<name>/SKILL.md` shows up in the system prompt's discovery block; an invalid SKILL.md surfaces in `cli --diagnostics` output without crashing the session. Prompt templates from `<cwd>/.hand/templates/*.md` can be invoked with `{{var}}` substitution from slash commands.

Reference: `pi-mono/.../core/skills.ts`, `core/prompt-templates.ts`, `core/resource-loader.ts`, `core/source-info.ts`, `utils/frontmatter.ts`. Use the existing test fixtures at `pi-mono/.../test/fixtures/skills/` as our test corpus.

| ID | Title | Scope (files) | TDD focus | Acceptance | Depends on |
|---|---|---|---|---|---|
| T2.1 | Frontmatter parser | `src/utils/frontmatter.rs` | Port the 17-case test matrix from TS `frontmatter.test.ts` (multi-line desc, no frontmatter, invalid YAML, etc.) | `parse_frontmatter(&str) -> Result<(Yaml, body), Error>`; pure function | T0.3 |
| T2.2 | Resource loader + source info | `src/core/resource_loader.rs`, `src/core/source_info.rs` | Fixture-based: `tests/fixtures/skills/` mirrors TS layout; assert correct precedence (project > user > builtin) and dedup | `ResourceLoader::discover_skills(cwd)` returns sorted, deduped list with source attributed | T2.1 |
| T2.3 | Skills domain | `src/core/skills.rs` | Use the TS `test/fixtures/skills/` set verbatim; cover collision precedence, name validation, multiline-desc, invalid-yaml degradation | `Skill { name, description, body, source }`; `discover()` is fallible only on IO, never on bad metadata | T2.2 |
| T2.4 | Prompt templates | `src/core/prompt_templates.rs` | Variable substitution `{{var}}` (no logic); missing-var → error with template name | `Template::render(vars: &HashMap<&str, &str>) -> Result<String>` | T2.1 |
| T2.5 | Wire into system prompt | `src/core/system_prompt.rs` | Snapshot test: given fixture skills, generated system prompt contains a "Skills" section with names alphabetized | `build_system_prompt` accepts `&[Skill]`; default loader plugs in via `AgentSessionConfig` | T2.3, T2.4 |

**Integration points:** `AgentSessionConfig` gains `skills: Vec<Skill>` and `templates: Vec<Template>`. `system_prompt::build_system_prompt` signature extended (additive). No change to `model`/`agent`.

### Phase 3 — Extensions runtime

**Why now:** With RPC + Skills landed, the only major customization gap is *runtime hooks* (cancel a tool call, mutate a payload, register a slash command). This phase unlocks community plugins.

**At the end:** A user can install an extension by dropping a manifest into `~/.hand/extensions/<name>/extension.toml`, and on startup it registers slash commands, tools, and hook subscriptions. Three TS example extensions (`hello`, `confirm-destructive`, `notify`) are reimplemented in the chosen runtime as fixtures and pass smoke tests.

| ID | Title | Scope | TDD focus | Acceptance | Depends on |
|---|---|---|---|---|---|
| T3.1 | **ADR-001 — Extensions runtime selection** | `docs/adr/0001-extensions-runtime.md` | n/a (decision doc) | Document evaluates 4 options (subprocess JSON-RPC, WASM via `wasmtime`, native dylib via `libloading`, declarative-only TOML) on five axes (security, dev ergonomics, perf, dependency cost, parity with TS API). Picks one. Records why others were rejected. Reviewed and approved before T3.2 starts. | T2.5 |
| T3.2 | Extension API surface | `src/core/extensions/api.rs`, `src/core/extensions/types.rs` | Trait/data-shape unit tests; serde for the manifest | `pub trait Extension`: `on_load`, `on_before_tool_call -> Decision`, `on_after_tool_call`, `on_message_user`, `slash_commands()`, `custom_tools()`. Manifest: `name`, `version`, `entry_point`, `permissions[]` | T3.1 |
| T3.3 | Runtime impl (chosen in T3.1) | `src/core/extensions/runtime/<chosen>.rs` | Roundtrip an event through the runtime; isolate from session state on panic; timeout on hook calls | One extension can be loaded, its `on_load` invoked, and a slash command registered. | T3.2 |
| T3.4 | Hook wiring in session | `src/core/agent_session.rs` (small surgical edits) | Mock extension cancels a tool call → session reports refusal to model; mock returns substitute message → loop sees substitute | Hooks fire at documented points; failure in one extension does not break the session | T3.3 |
| T3.5 | Slash + tool registration | `src/core/slash_commands.rs`, `src/tools/mod.rs` | Test: extension registers `/foo` and a tool named `bar`; both resolve via the same registries as builtins | Extensions tools appear in agent loop's tool list; `/foo` dispatches to the extension | T3.4 |
| T3.6 | Example extensions as fixtures | `examples/extensions/{hello,confirm_destructive,notify}/` | Each fixture has its own integration test driving a mocked session | Three examples pass; documented in `examples/README.md` | T3.5 |

**Integration points:** `AgentLoopConfig` already has `before_tool_call` / `after_tool_call` hook slots — extensions plug into these via the dispatcher. No `agent` API change. `tools::ToolRegistry` (new in T3.5) wraps both static and extension-supplied tools.

### Phase 4 — Settings + keybindings

**Why now:** Phases 1–3 introduced 5+ new tunables (RPC port, extension allowlist, skill paths, compaction overrides). Pulling them into a coherent settings file before Phase 5 means the TUI's settings selector has something real to drive.

**At the end:** `~/.hand/settings.yaml` and `<cwd>/.hand/settings.yaml` both load and merge (project overrides user); `SettingsManager::watch()` reloads on change without restart; legacy `~/.hand/agent/settings.json` is migrated on first read with a `.bak` left behind.

Reference: `pi-mono/.../core/settings-manager.ts`, `core/keybindings.ts`, `core/defaults.ts`.

| ID | Title | Scope | TDD focus | Acceptance | Depends on |
|---|---|---|---|---|---|
| T4.1 | YAML schema + parser | `src/core/settings.rs` (rewrite), Cargo dep `serde_yaml` | Roundtrip every documented field; default-value substitution for missing keys; reject unknown top-level keys with a warning, not an error | `SettingsManager::load(&cwd) -> Settings`; `Settings::merge(global, project)` | T0.3 |
| T4.2 | Reload + watcher | `src/core/settings.rs`, dep `notify` | Test: write file → callback fires within 200ms with new value | Subscribers receive `SettingsChanged(diff)` | T4.1 |
| T4.3 | Keybindings | `src/core/keybindings.rs` | Port TS `keybindings.test.ts` cases (chord parsing, conflict detection) | `KeyBindings::resolve(action) -> KeyChord`; user override merges over default | T4.1 |
| T4.4 | Migration | `src/core/settings.rs::migrate_from_json` | Fixture: a legacy JSON file → migrates to YAML; .bak preserved | Idempotent: second run is no-op | T4.1 |

**Integration points:** `AgentSession` now consumes `Arc<RwLock<Settings>>` instead of an owned `SettingsManager` snapshot. Public types added to `prelude`.

### Phase 5 — Interactive TUI v1

**Why later:** TUI is the largest, most architecturally novel chunk (TS uses ink/React; Rust must use the imperative `hand-tui`). Doing it after the data plane is settled means the TUI is a thin presentation layer, not a place where business logic accumulates.

**At the end:** `hand` (no flags) launches an interactive session with a message pane (with diff rendering for edit tool calls), a multi-line editor with paste support and `@path`/`!cmd` hot-syntax, a status bar (model, token count, cost), a slash-command palette, and selectors for `/model`, `/resume`, `/settings`. Streaming responses render incrementally without flicker.

| ID | Title | Scope | TDD focus | Acceptance | Depends on |
|---|---|---|---|---|---|
| T5.1 | **ADR-002 — TUI architecture** | `docs/adr/0002-tui-architecture.md` | n/a | Audit `hand-tui` capabilities (does it have: line buffer, focus model, paste capture, raw-mode hooks, image protocol?). Compare against the 12 TS interactive components we need. Decide for each: reuse hand-tui primitive, build on top, or upstream a new primitive. Output: a component-by-component table. | T4.4 |
| T5.2 | Layout shell | `src/modes/interactive/mod.rs`, `src/modes/interactive/layout.rs` | Snapshot of empty layout at three terminal sizes (80×24, 120×40, 200×60) | Three regions render; resize redraws cleanly | T5.1 |
| T5.3 | Editor | `src/modes/interactive/editor.rs` | Unit tests: cursor movement, multi-line paste, slash-trigger detection at column 0, `@path` and `!cmd` parse | Editor emits `EditorEvent::Submit(String)` / `SlashTrigger` / `BashTrigger` | T5.2 |
| T5.4 | Message renderer | `src/modes/interactive/messages.rs`, extend `tools/edit.rs` with diff output | For each `Message` variant, snapshot rendered output; edit tool diff matches `similar` Unified format | Streaming `TextDelta` updates a single message in-place without redrawing scrollback | T5.2 |
| T5.5 | Selector primitives | `src/modes/interactive/selectors.rs` | Port a representative subset of TS `tree-selector.test.ts`, `session-selector-search.test.ts` | `Selector::run(items, options) -> Option<T>` with fuzzy filter and arrow-key nav | T5.2 |
| T5.6 | Streaming render loop | `src/modes/interactive/runtime.rs` | Drive a mocked session through 3 turns; assert no full-pane redraw between deltas (use a render counter) | Renders only the dirty region per delta; tested at 200 deltas/sec | T5.3, T5.4 |
| T5.7 | Slash palette + keybindings | `src/modes/interactive/palette.rs` | `/` opens palette, fuzzy-filters commands, `Esc` cancels, `Enter` runs | Keybindings from Phase 4 are honored | T5.5, T5.6, T4.3 |

**Integration points:** This phase may need *additive* extensions to `hand-tui` — file the contract in T5.1 ADR; do not change `hand-tui` in flight without that decision.

### Phase 6 — Auth + telemetry

**Why last:** Lower urgency; everything functional already works with env-var keys. This phase makes onboarding nicer and gives us observability for production users.

**At the end:** First-run with no `ANTHROPIC_API_KEY` sets prompts user with provider-specific guidance, optionally launches OAuth (Anthropic-only initially) and persists tokens encrypted with platform keychain (`keyring` crate); `tracing` spans cover session lifecycle and tool calls; emit anonymous, opt-in events to a configurable sink.

| ID | Title | Scope | TDD focus | Acceptance |
|---|---|---|---|---|
| T6.1 | Auth storage | `src/core/auth_storage.rs`, dep `keyring` | Roundtrip a token through the platform keystore; fallback file storage when keystore unavailable | `AuthStorage::set/get/delete(provider)` |
| T6.2 | Auth guidance | `src/core/auth_guidance.rs` | One snapshot per provider | `auth_guidance(provider) -> Vec<GuidanceStep>` |
| T6.3 | OAuth (Anthropic) | `src/core/auth_oauth.rs`, dep `oauth2` | Mock the IDP roundtrip | A captured token is stored via T6.1 |
| T6.4 | Telemetry + timings | `src/core/telemetry.rs`, `src/core/timings.rs` | `tracing` spans assert with `tracing-test`; opt-in flag respected | All session events emit a span; sink is pluggable |

**Integration points:** No changes to sibling crates.

## Concrete Steps

Run from `crates/coding-agent` unless noted.

```bash
# Per task — start of work
git checkout -b feat/coding-agent/<phase>-<slug>
git pull --rebase origin feat-coding-agent

# Run every commit
cd /Users/wanggang/.touch-code/repos/hand-ai/feat-coding-agent
./check.sh        # cargo check + cargo test workspace-wide

# Tighter loop while iterating in this crate
cargo check -p hand-coding-agent
cargo test  -p hand-coding-agent <test_filter>
cargo clippy -p hand-coding-agent --all-targets -- -D warnings

# Commit after each logical change
git add crates/coding-agent/<specific-file> ...
git commit -m "feat(coding-agent): <what>"

# At phase end — full sweep
./check.sh
```

Expected `./check.sh` output before any phase merges:

```
[OK] cargo check
[OK] cargo test (NN tests passed)
[OK] cargo clippy (zero warnings on touched files)
```

## Validation and Acceptance

Phase-level acceptance checks (run from repo root):

- **Phase 1 done when:** `cargo test -p hand-coding-agent --test rpc_smoke` passes; manually `echo '{"jsonrpc":"2.0","method":"session.create","id":1}' | cargo run --bin hand -- --rpc` returns a session id.
- **Phase 2 done when:** Place `test/fixtures/skills/valid-skill/SKILL.md` (ported from TS) under `<cwd>/.hand/skills/`; `cargo run --bin hand -- --print --prompt "list skills"` mentions it.
- **Phase 3 done when:** Drop `examples/extensions/hello/` into `~/.hand/extensions/`; `/hello` slash command produces the example output.
- **Phase 4 done when:** Edit `<cwd>/.hand/settings.yaml`, change `theme: dark` → `light`; running session reflects the change without restart (logged via tracing).
- **Phase 5 done when:** `cargo run --bin hand` launches a TUI; typing `/model` opens a selector; submitting a prompt streams output incrementally; an `edit` tool call renders a colored unified diff inline.
- **Phase 6 done when:** With no `ANTHROPIC_API_KEY` set, first run launches the OAuth flow (or falls back gracefully); subsequent runs find the token via `keyring`.

Each phase additionally requires:

- All new public symbols documented (`/// …`) with at least one example.
- ≥ 1 test per `pub fn`; ≥ 1 roundtrip test per serializable struct/enum.
- `cargo clippy --all-targets -- -D warnings` on touched files.
- CHANGELOG.md updated under `[Unreleased]` with the appropriate subsection.

## Idempotence and Recovery

- All file I/O in `core/{settings,session_manager,resource_loader}` is **idempotent**: write-then-fsync-then-rename on save; loaders tolerate missing files. Re-running `hand` against the same `cwd` is always safe.
- The session JSONL is append-only — replaying a partial session reads up to the last well-formed line and discards trailing garbage with a warning.
- RPC dispatcher: a malformed request emits an `RpcError` and continues; an unhandled internal panic terminates the session cleanly with exit code 2.
- Settings migration (T4.4) is **idempotent**: the second run finds the YAML and skips the JSON path; the `.bak` is never overwritten.
- Extension load failure is non-fatal: the extension is logged and skipped; session continues with builtins.
- For Phase 5, all rendering is buffered through `hand-tui`'s diff renderer — if the TUI panics, the panic hook restores cooked mode before unwinding.

## Artifacts and Notes

Reference snippets to keep on hand while implementing:

- **Existing in-memory test setup** — `crates/coding-agent/src/core/agent_session.rs` lines 113–135 and 372–435 show the shape of a mocked session and the pattern for wiring the event sink in tests. Reuse this in Phase 1's RPC tests.
- **TS fixture corpus** — `pi-mono/packages/coding-agent/test/fixtures/skills/` has ~14 fixtures covering every pathological frontmatter case. Mirror them under `crates/coding-agent/tests/fixtures/skills/` for Phase 2.
- **Conversion-plan.md test catalogue** — `docs/conversion-plan.md` already lists the C-001 .. C-113 test IDs for this crate; tasks here should add tests under those IDs where they overlap (e.g. T1.6 contributes to C-110-class scenarios).

## Interfaces and Dependencies

The following symbols **must exist** at the end of each phase. Use these as a grading rubric.

**End of Phase 1, in `src/modes/rpc/types.rs`:**

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum RpcRequest {
    SessionCreate { params: SessionCreateParams, id: RequestId },
    SessionSendMessage { params: SessionSendMessageParams, id: RequestId },
    SessionCancel { params: SessionCancelParams, id: RequestId },
    SessionCompact { params: SessionCompactParams, id: RequestId },
    SessionGetState { params: SessionGetStateParams, id: RequestId },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RpcEvent { /* mirrors AgentSessionEvent over the wire */ }

pub async fn run_rpc_server<R, W>(reader: R, writer: W) -> Result<(), RpcError>
where R: AsyncBufRead + Send + Unpin, W: AsyncWrite + Send + Unpin;
```

**End of Phase 2, in `src/core/skills.rs`:**

```rust
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub source: SourceInfo,
    pub disable_model_invocation: bool,
}

pub fn discover_skills(cwd: &Path) -> Result<Vec<Skill>, SkillError>;
```

**End of Phase 3, in `src/core/extensions/api.rs`:**

```rust
#[async_trait::async_trait]
pub trait Extension: Send + Sync {
    fn manifest(&self) -> &ExtensionManifest;
    async fn on_load(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_before_tool_call(&self, _: &ToolCallEvent) -> HookDecision { HookDecision::Continue }
    async fn on_after_tool_call(&self, _: &ToolResultEvent) -> Result<(), ExtensionError> { Ok(()) }
    fn slash_commands(&self) -> Vec<SlashCommandSpec> { vec![] }
    fn custom_tools(&self) -> Vec<AgentTool> { vec![] }
}

pub enum HookDecision { Continue, Cancel(String), Replace(serde_json::Value) }
```

**End of Phase 4, in `src/core/settings.rs`:**

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Settings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<ThinkingLevel>,
    pub theme: Theme,
    pub compaction: CompactionSettings,
    pub retry: RetrySettings,
    pub keybindings: HashMap<Action, KeyChord>,
    pub extensions: ExtensionSettings,
    pub rpc: RpcSettings,
    pub quiet_startup: bool,
}

pub struct SettingsManager { /* ... */ }
impl SettingsManager {
    pub fn load(cwd: &Path) -> Result<Self, SettingsError>;
    pub fn watch(&self) -> broadcast::Receiver<SettingsChanged>;
    pub fn current(&self) -> Settings;
}
```

**End of Phase 5, in `src/modes/interactive/mod.rs`:**

```rust
pub struct InteractiveMode { /* ... */ }
impl InteractiveMode {
    pub fn new(session: AgentSession, settings: Arc<RwLock<Settings>>) -> Result<Self, InteractiveError>;
    pub async fn run(self) -> Result<(), InteractiveError>;
}
```

**End of Phase 6, in `src/core/auth_storage.rs`:**

```rust
pub trait AuthStorage: Send + Sync {
    fn get(&self, provider: &str) -> Result<Option<AuthToken>, AuthError>;
    fn set(&self, provider: &str, token: AuthToken) -> Result<(), AuthError>;
    fn delete(&self, provider: &str) -> Result<(), AuthError>;
}
pub fn default_storage() -> Box<dyn AuthStorage>; // keyring with file fallback
```

**New crate dependencies (in order of phase introduction):**

| Phase | Crate | Reason |
|---|---|---|
| 1 | `tokio = { features = ["io-util", "process"] }` (already present) | RPC stdin/stdout |
| 2 | `serde_yaml`, `pulldown-cmark` (skills body) | Frontmatter + skill body |
| 3 | (per ADR-001) one of: `wasmtime`, `libloading`, `tokio::process` only | Extension runtime |
| 3 | `async-trait` | Extension trait async methods |
| 4 | `serde_yaml`, `notify` | Settings YAML + watcher |
| 5 | (per ADR-002) — likely `crossterm` features already in `hand-tui` | TUI primitives |
| 6 | `keyring`, `oauth2`, `tracing-subscriber` (already present) | Auth + telemetry |

## Risk Register and Mitigations

| # | Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|---|
| R1 | TUI architecture (Phase 5) needs upstream changes to `hand-tui` that block coding-agent's timeline | High | Medium | T5.1 ADR audits before any Phase 5 code is written; the table-of-needs result drives whether `hand-tui` work is split off as a parallel stream |
| R2 | Extensions runtime decision (ADR-001) chooses an option that turns out infeasible (e.g. `wasmtime` startup cost too high for short sessions) | High | Medium | ADR-001 includes a 1-day prototype for the chosen runtime before T3.3 commits; if prototype fails, fall back to subprocess JSON-RPC (lowest-risk choice) |
| R3 | RPC wire format diverges from TS, breaking SDK consumers who script both | Medium | Medium | T1.1 includes a captured-payload roundtrip test (capture from a live `pi-coding-agent --rpc` session, store under `tests/fixtures/rpc-payloads/`) |
| R4 | Skills frontmatter parser disagrees with TS on edge cases (multiline desc, escaped colons) | Low | High | Port the entire TS test fixture set; treat any divergence as a blocker before T2.5 |
| R5 | Settings YAML migration loses user data on first run | High | Low | T4.4 always writes `.bak` first; idempotency tested with a fixture; never delete user files |
| R6 | Streaming render loop (T5.6) flickers or drops events under high delta rate | Medium | Medium | Drive with a synthetic 1k-events/sec test in CI; render counter assertion ensures only dirty regions update |
| R7 | OAuth flow blocks first-run on systems without a browser | Medium | Medium | T6.3 always falls back to "paste your token" path; OAuth is opt-in |
| R8 | Sibling crate API drift (`hand-agent`, `model`) breaks coding-agent during a phase | High | Low | Pin specific commits in `Cargo.toml` while a phase is in flight; lift the pin only when joining changes are explicit |
| R9 | Test corpus size grows so much (skills/RPC/extensions fixtures) that CI runtime becomes painful | Low | Medium | Group fixtures under a `slow` test feature flag; keep the default `cargo test` under 30s |
| R10 | Schema evolution: adding fields to `RpcEvent` breaks deployed RPC clients | Medium | Medium | Lock the wire format with a `RPC_PROTOCOL_VERSION` constant; bump on any non-additive change; document in the `--rpc` help output |

---

**Approval gate:** Do not begin Phase 1 implementation until this plan is reviewed and the two ADR tasks (T3.1, T5.1) are confirmed as required-before-implementation gates. Reply with approve / change request.
