# ExecPlan: pi-mono ↔ hand-ai parity — final stretch

**Status:** Draft
**Author:** Gump (with Codex parity review)
**Date:** 2026-05-08
**Base commit:** `d532896` (origin/main is in sync)

This is a living document. The Progress, Surprises & Discoveries, Decision Log,
and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this plan lands, `hand-ai` will pass a deep parity audit against
`pi-mono` for the four in-scope crates (`model`, `agent`, `coding-agent`,
`tui`). Specifically:

1. A user running the Rust `hand-coding-agent` RPC server can be driven by an
   unmodified pi-mono TypeScript client — JSON event payloads will match
   byte-for-byte (today they don't: Rust ships snake_case fields while TS
   sends camelCase).
2. Every TypeScript slash command that exists in pi-mono's interactive mode
   will resolve to a working Rust handler — including `/export`, `/import`,
   `/fork`, `/clone`, `/theme`, `/skills`, `/extensions`, `/changelog`,
   `/name`, and the inline `/model <pattern>` form.
3. The Rust `Settings` struct will round-trip with TypeScript-format
   `settings.json` files written by pi-mono — every public TS field will
   have a Rust counterpart, matching the camelCase wire format.
4. Compaction will work on real session histories that include branch
   summaries, custom messages, bash executions, and thinking-level
   changes — i.e. the entry-tree path will be live, not TODO-stubbed.
5. Custom providers (any string outside the built-in `Api` / `Provider`
   enum) will be registrable at runtime, matching TS's open-string
   semantics.
6. Smaller utility files Codex flagged as missing (`file-mutation-queue`,
   `output-accumulator`, `path-utils`, `render-utils`, `tool-definition-wrapper`,
   `truncate`, TUI `undo-stack`, `export-html`) will have Rust counterparts
   and at least one binding usage.

The proof: `cargo test --workspace` continues to pass, plus a new
`tests/wire_parity_test.rs` in `model/` and `agent/` asserts each event's
serialized JSON matches a fixture captured from running pi-mono's TS
emitter.

## Progress

- [ ] **M1 — Wire format camelCase** (model + agent serde fix)
  - [ ] M1.T1: Audit every public type that crosses the RPC / proxy boundary
  - [ ] M1.T2: Apply `#[serde(rename_all = "camelCase")]` to enum variants in `AssistantMessageEvent`, `AgentEvent`, and transitive structs
  - [ ] M1.T3: Add `tests/wire_parity_test.rs` with TS-captured fixtures
  - [ ] M1.T4: Audit and fix `RpcMessage` enum + `MessagesData` shapes
  - [ ] M1.T5: Verify proxy.rs already-correct fields don't double-rename
- [ ] **M2 — Settings field expansion**
  - [ ] M2.T1: Add 25+ missing fields to `Settings` with `serde(rename_all = "camelCase")` (or per-field rename)
  - [ ] M2.T2: Port supporting types: `TerminalSettings`, `ImageSettings`, `MarkdownSettings`, `WarningSettings`, `RetrySettings`, `BranchSummarySettings`, `ThinkingBudgetsSettings`, `TransportSetting`
  - [ ] M2.T3: Wire new fields through `SettingsManager::merge` and `save(scope)`
  - [ ] M2.T4: Round-trip test against pi-mono YAML/JSON fixture
- [ ] **M3 — Slash command suite (10 new commands)**
  - [ ] M3.T1: `/export`, `/import`, `/copy` (already partial — finish writes)
  - [ ] M3.T2: `/fork`, `/clone`, `/name` (session ops — wire `SessionManager` calls)
  - [ ] M3.T3: `/theme` (overlay-driven — uses `ThemeSelectorComponent` already ported)
  - [ ] M3.T4: `/skills`, `/extensions`, `/changelog` (read-only listings)
  - [ ] M3.T5: Inline `/model <pattern>` form (currently only `/model` opens overlay)
- [ ] **M4 — Compaction entry-tree path**
  - [ ] M4.T1: Extend `SessionEntry` enum with `BranchSummary`, `CustomMessage`, `BashExecution`, `ThinkingLevelChange` variants + `parent_id: Option<String>`
  - [ ] M4.T2: Migrate `SessionManager::append_*` and JSONL parser to handle new variants
  - [ ] M4.T3: Port `prepareCompaction`, `findValidCutPoints`, `findTurnStartIndex`, `findCutPoint`, `getMessageFromEntry` from `compaction.ts`
  - [ ] M4.T4: Port `collectEntriesForBranchSummary` + branch-summary tree traversal
  - [ ] M4.T5: Replace `// TODO(parity): requires SessionEntry tree extension` markers with real impls
- [ ] **M5 — Open `Api` / `Provider`**
  - [ ] M5.T1: Replace `pub enum Api { ... }` with `pub struct Api(String)` + a `BuiltIn` newtype constructor
  - [ ] M5.T2: Same for `Provider`
  - [ ] M5.T3: Migrate ~50 call sites; update parity tests
  - [ ] M5.T4: Wire `register_provider(Api, Provider, Box<dyn ApiProvider>)` to accept arbitrary identifiers
- [ ] **M6 — Smaller TS file backfills**
  - [ ] M6.T1: Port `coding-agent/utils/{path-utils, render-utils, truncate}.ts`
  - [ ] M6.T2: Port `coding-agent/core/{file-mutation-queue, output-accumulator, tool-definition-wrapper}.ts`
  - [ ] M6.T3: Port `tui/undo-stack.ts` (can be its own file or extension to existing editor)
  - [ ] M6.T4: Port `coding-agent/core/export-html/` pipeline
  - [ ] M6.T5: Wire each at least one usage so it isn't dead code

## Surprises & Discoveries

(None yet — to be filled as work proceeds.)

## Decision Log

**D-01 (2026-05-08): Closed enums vs open strings for Api/Provider.**
Decided to keep both representations: a `pub struct Api(String)` with const
lookup helpers for the 11 built-ins, plus an `Api::custom("foo")`
constructor. This matches TS's open-string semantics while still letting
match-style code use `Api::ANTHROPIC_MESSAGES`-style constants. Rationale:
fully open strings break `match` exhaustiveness; fully closed enums break
custom providers. The newtype-wrapping a string + const constants is the
ergonomic compromise.

**D-02 (2026-05-08): Wire format camelCase scope.**
Only types that cross JSON serialization boundaries need the
`#[serde(rename_all = "camelCase")]` treatment. Internal Rust-only types
(e.g. `AgentLoopConfig`, `RuntimeState`) keep snake_case for ergonomic
matching. The audit step in M1.T1 catalogues every type that participates
in a `serde_json::to_string` call site or an RPC `MessagesData` payload.

**D-03 (2026-05-08): SessionEntry shape extension is non-optional.**
M4 cannot land without M4.T1 (extending `SessionEntry`). Several existing
ports are stubbed `// TODO(parity): requires SessionEntry tree extension`
because of this. Not extending blocks compaction parity, branch
summarization on real branched sessions, and `/fork`/`/clone` correctness.

## Outcomes & Retrospective

(To be filled at milestone completion.)

## Context and Orientation

### Source-of-truth and target

- **pi-mono (TS reference)** at `/Users/wanggang/dev/opensource/pi-mono/`.
  All `pi-mono/packages/{ai, agent, coding-agent, tui}/src/` are in scope.
  `bun/`, `web-ui/`, `pods/`, `mom/` are out of scope.
- **hand-ai (Rust target)** at `/Users/wanggang/dev/00/hand-ai/`. Crates
  under `packages/{model, agent, coding-agent, tui}/`.

### Related documents (read these first)

- Existing parity plan: `docs/exec-plans/parity-completion.md` —
  high-level batches; this plan supersedes its remaining open items.
- Conversion guidelines: `docs/conversion-guidelines.md` — the rules of
  the road for TS → Rust shape mapping. §14 (design pattern conversion)
  and §15 ("not to be directly translated") are critical for M1, M5.
- Codex parity review (this session, in-conversation only) — the source
  of the gap inventory below.
- Master plan (all earlier batches): the conversation history of the
  current session is the authoritative log of what's been done.

### Current state

- `cargo test --workspace`: 2176 passed / 0 failed.
- All 35 interactive components ported. Theme system ported. Driver
  skeleton with 14 wired slash commands. RPC server with 22-method
  surface. Source registry with install / remove / update.
- Five Critical bugs from review fixed.
- 113 commits past the original review baseline.

### Key source files this plan touches

- `packages/model/src/types.rs` — wire types for `AssistantMessageEvent`,
  `Api`, `Provider`, `AssistantMessage`, `Usage`, `Cost`, `ToolCall`,
  `Compat`, `Message`. Currently `rename_all = "snake_case"` at the enum
  level, fields default-snake_case. Needs camelCase normalization for
  cross-process types (M1, M5).
- `packages/agent/src/types.rs` — `AgentEvent` enum, `AgentLoopConfig`,
  `BeforeToolCallHook` etc. `AgentEvent` is what the RPC layer ships
  (M1.T4).
- `packages/agent/src/proxy.rs` — already does camelCase right via
  `#[serde(rename = "contentIndex")]` per-field. M1.T5 verifies no
  double-renaming after the global change.
- `packages/coding-agent/src/rpc/server.rs` — line 109's
  `RpcMessage::Agent(Box<AgentEvent>)` is the cross-process boundary.
- `packages/coding-agent/src/core/settings.rs` — currently 72 fields
  across all types in this file; TS has 35+ on `Settings` alone plus 7+
  supporting types. M2 is the largest single file expansion.
- `packages/coding-agent/src/core/session_manager.rs` —
  `SessionEntry` enum is the data structure compaction needs extended
  (M4.T1).
- `packages/coding-agent/src/core/compaction/{compactor, branch_summarization, utils}.rs` —
  contain the `// TODO(parity): requires SessionEntry tree extension`
  markers M4 closes.
- `packages/coding-agent/src/modes/interactive/{driver, slash_commands, event_dispatch}.rs` —
  M3 lands here.
- `packages/coding-agent/src/utils/` — M6 adds new util files.
- `packages/coding-agent/src/core/export.rs` — M6.T4 expands this from
  the current minimal export to a full HTML pipeline.

### Terms

- **Wire format**: the JSON shape of a struct as seen by another process.
  Distinct from the in-Rust shape; the same struct can have a Rust field
  `content_index` and a JSON field `contentIndex` via `#[serde(rename)]`.
- **Entry-tree path**: pi-mono's session history is a *tree* of
  `SessionEntry` nodes (each with a `parentId`), not a flat list. Branch
  summarization and compaction walk this tree. Today our `SessionEntry`
  is flat.
- **Open vs closed enum**: TS allows arbitrary string values for
  `Api` / `Provider`; our Rust port locked them down to a closed
  variant set. Custom providers can't materialize as `Model` today.
- **By design** (per `docs/conversion-guidelines.md` §15): patterns we
  intentionally diverge from TS on. TypeBox helpers, Bun-native APIs,
  prototype-based extension — these are not in scope for parity.

## Plan of Work

The work is sliced into six milestones. **Each milestone is independently
shippable** — landing M1 alone closes the wire-format correctness gap;
landing M3 alone closes the user-visible slash-command gap. Order is by
risk-adjusted dependency, not by topic.

### Milestone 1 — Wire format camelCase (highest leverage, smallest change)

After this milestone, an unmodified pi-mono TypeScript client driving the
Rust RPC server will receive JSON events whose every field name matches
the TS source. The fix is mechanical (serde annotations) but the audit
step is non-trivial because we need to decide which types are wire-types
and which are internal-Rust-only.

**M1.T1 — Wire-type audit.** Open `packages/model/src/types.rs`,
`packages/agent/src/types.rs`, and `packages/coding-agent/src/rpc/types.rs`.
Tag each `Serialize`/`Deserialize`-deriving type as either *wire* (used
in `serde_json::to_string` somewhere that crosses a process boundary) or
*internal* (used only inside one Rust process for in-memory persistence
or test fixtures). Capture the catalog at the bottom of this plan's
"Artifacts" section.

Acceptance: a markdown table in the Artifacts section listing every
exported type and its tag, with the call-site grep evidence inline.

**M1.T2 — Apply camelCase to wire types.** For every type tagged *wire*,
add `#[serde(rename_all = "camelCase")]` at the struct/enum level. Where
specific TS fields use non-standard names (`accountId` for the OAuth blob
rather than `account_id`), add per-field `#[serde(rename = "...")]`.
Verify `cargo test --workspace` still passes; fix any tests that
asserted the snake_case wire shape (those tests are wrong).

Acceptance: `cargo test --workspace` green. No existing test fails for
data-shape reasons.

**M1.T3 — Wire parity tests.** Capture JSON fixtures from a running
pi-mono `coding-agent` RPC server (or extract from TS unit tests) for
each event type. Add `packages/{model,agent}/tests/wire_parity_test.rs`
that round-trips each fixture through the Rust serde and asserts
byte-equality (or at least field-set equality if key ordering differs).

Acceptance: 12+ new passing tests, one per `AssistantMessageEvent`
variant + a handful for `AgentEvent` and key wire structs.

**M1.T4 — RPC envelope audit.** `packages/coding-agent/src/rpc/types.rs`
defines `RpcMessage` and `MessagesData`. Verify these use camelCase for
fields, with proper `tag = "type"` discrimination. The dispatch is what
TS clients consume.

Acceptance: a TS client message captured from pi-mono parses cleanly via
`serde_json::from_str::<RpcMessage>(...)` in a new round-trip test.

**M1.T5 — Proxy double-rename check.** `proxy.rs` already does
camelCase via per-field `#[serde(rename)]`. After M1.T2 makes the
container-level rename `camelCase`, the per-field renames become
redundant but should not become harmful. Verify by running
`cargo test -p hand-agent --test proxy_test` and inspecting one
event's serialization manually. Either remove the redundant per-field
renames (cleaner) or leave them (defensive — fine).

Acceptance: proxy_test still green.

### Milestone 2 — Settings field expansion

After this milestone, `~/.hand/agent/settings.yaml` (or the project
override) can carry every field a pi-mono user might write, and the
Rust binary respects them. Notably: terminal preferences, image
handling, model-cycle list, double-escape behavior, theme name,
markdown rendering preferences.

**M2.T1 — Field inventory and serde shape.** From
`/Users/wanggang/dev/opensource/pi-mono/packages/coding-agent/src/core/settings-manager.ts`,
extract every field of `interface Settings` and its supporting types.
Add them to `packages/coding-agent/src/core/settings.rs` as
`Option<T>` fields with appropriate serde shape. The container-level
attribute should be `#[serde(rename_all = "camelCase")]` since pi-mono
serializes settings as JSON (TS default is camelCase).

Pi-mono uses JSON; the Rust port currently uses YAML — this is a
deliberate divergence per existing decision log. Keep YAML, add a
JSON-import path (M2.T4 covers it).

Acceptance: every TS Settings field has a Rust counterpart. No
behavioral change yet.

**M2.T2 — Port supporting types.** Define:
- `TerminalSettings` (paste threshold, etc.)
- `ImageSettings` (max width, encoding)
- `MarkdownSettings` (render style toggles)
- `WarningSettings` (which warning toasts to show)
- `RetrySettings` (max retry, backoff)
- `BranchSummarySettings` (auto-summarize trigger)
- `ThinkingBudgetsSettings` (per-level token caps)
- `TransportSetting` (auto / api / cli / sdk)

Each in `core/settings.rs` or — if it's >50 lines — its own submodule
under `core/settings/`. Use serde camelCase.

Acceptance: each type has at least one field round-trip test.

**M2.T3 — Plumb through SettingsManager.** The `SettingsScope::{Global, Project}`
merge logic (added recently) needs to handle the new fields. Most are
project-overridable; some (like `lastChangelogVersion`) are global-only.
Update `recompute_merged()` to handle each new field's merge semantics
(usually project takes precedence, except for global-only fields).

Acceptance: layered round-trip test covering at least 5 of the new
fields.

**M2.T4 — JSON import compatibility.** Add a `Settings::from_json_str`
constructor (or `serde_json::from_str` directly works if camelCase is
right). Add a CLI flag `--import-settings <path>` that reads either
JSON or YAML and merges into the active settings. Use this to
verify pi-mono-written settings files load without modification.

Acceptance: a fixture file
`packages/coding-agent/tests/fixtures/pi-mono-settings.json` parses
cleanly into the Rust `Settings` struct.

### Milestone 3 — Slash command suite

After this milestone, the 10 missing slash commands work end-to-end.
Most are thin wrappers around already-ported subsystems
(`SessionManager` for fork/clone/name, source_registry for skills/
extensions, theme_selector for theme).

**M3.T1 — Read-write session ops.** Wire:
- `/export <path>` → call existing
  `core::export::export_session(format)` and write to `<path>`.
  Format inferred from extension (`.html`, `.json`, `.md`).
- `/import <path>` → load JSONL/JSON, validate, append to current session
  via `SessionManager::import_from_jsonl`.
- `/copy [n]` → already partial; finish by wiring last-n-message
  selection to `utils::clipboard::copy_to_clipboard`.

In `packages/coding-agent/src/modes/interactive/slash_commands.rs`,
add `Action::{Export(PathBuf), Import(PathBuf), CopyN(u32)}`. In
`driver.rs`, wire dispatch.

Acceptance: each command has a unit test that runs through the
dispatcher with a mock session and verifies the side effect.

**M3.T2 — Branch / fork / clone / name.** Wire:
- `/fork [<entry-id>]` → `SessionManager::fork(entry_id)` (already
  ported in `feat-coding-agent` PR).
- `/clone` → `SessionManager::clone()` — same session, fresh ID.
- `/name <new-name>` → `SessionManager::set_label(name)` (already there;
  just wire the command).

Acceptance: 3 dispatch tests.

**M3.T3 — `/theme [name]`.** With no arg, mount `ThemeSelectorComponent`
via `OverlayMounter` (already wired pattern). With arg, call
`Theme::load_named(name)` (theme system port from earlier batch) and
broadcast a `ChatUpdate::ThemeChanged` so all components re-render with
new colors.

Acceptance: `/theme dark` switches the displayed theme and the next
diff includes the new ANSI sequences. One end-to-end test using the
test backend.

**M3.T4 — Read-only listings.**
- `/skills` → list `core::skills::list_skills()` output.
- `/extensions` → list `core::extensions::registry::list_loaded()`.
- `/changelog` → read `CHANGELOG.md` from the agent's distribution dir
  and render via `MarkdownComponent`.

Each emits an inline `CustomMessage` into the chat history.

Acceptance: 3 dispatch tests rendering expected line counts.

**M3.T5 — Inline `/model <pattern>`.** Currently bare `/model` opens
the overlay. Add: `/model <pattern>` does a lookup via
`core::model_resolver::parse_model_pattern_full(pattern, available, opts)`
(already ported), and either resolves uniquely or surfaces the
ambiguity message in chat.

Acceptance: 3 dispatch tests covering exact match, fuzzy match, and
ambiguous-error.

### Milestone 4 — Compaction entry-tree path

After this milestone, sessions with branches, forks, custom messages,
bash executions, and thinking-level changes will compact correctly.
This is the deepest architectural change in the plan. **It must come
before any feature that relies on accurate auto-compaction.**

**M4.T1 — Extend `SessionEntry`.** In
`packages/coding-agent/src/core/session_manager.rs`, the existing
`SessionEntry` enum has variants for `Message`, `ModelChange`,
`Compaction`, `Label`. Add:
- `BranchSummary { id: String, parent_id: Option<String>, summary: String, ... }`
- `CustomMessage { id, parent_id, body: String, message_type: String }`
- `BashExecution { id, parent_id, command: String, output: String, exit_code: Option<i32> }`
- `ThinkingLevelChange { id, parent_id, level: ThinkingLevel }`
- Add `parent_id: Option<String>` to all existing variants too (today
  the entries are flat — this is what unlocks the tree).

Acceptance: existing tests pass after migration. Any old JSONL fixture
without `parent_id` deserializes by defaulting it to None (use
`#[serde(default)]`).

**M4.T2 — JSONL parser + writer.** In
`session_manager.rs::parse_session_entries` and the corresponding
writer: add round-trip support for the new variants. Validate that
`SessionInfo::all_messages_text` still works (it walks Message-only).

Acceptance: round-trip test that inserts each new variant kind and
reads back.

**M4.T3 — `prepareCompaction`.** Port from
`pi-mono/.../core/compaction/compaction.ts`. The Rust counterpart
goes in `core/compaction/compactor.rs`. It walks entries, resolves
the cut point, identifies the previous compaction (if any), and
returns a `PreparedCompaction` struct ready for `compact()`.

Functions to port: `prepareCompaction`, `findValidCutPoints`,
`findTurnStartIndex`, `findCutPoint`, `getMessageFromEntry`,
`getMessageFromEntryForCompaction`.

Acceptance: 6+ unit tests covering each cut-point edge case TS has.

**M4.T4 — Branch-summary entry collection.** Port
`collectEntriesForBranchSummary` and `prepareBranchEntries` from
`branch-summarization.ts`. These are currently `// TODO(parity)` in
`compaction/branch_summarization.rs`.

Acceptance: a branched session (created via `/fork`) compacts and
produces a branch summary that references the correct ancestor chain.

**M4.T5 — Replace TODO markers.** Open every file under
`core/compaction/` and search for `// TODO(parity): requires SessionEntry tree extension`.
Replace each with the real implementation now that the tree is live.

Acceptance: `git grep "TODO(parity): requires SessionEntry"` returns
zero hits in `packages/coding-agent/src/core/compaction/`.

### Milestone 5 — Open `Api` / `Provider`

After this milestone, custom providers (e.g. a user's local LLM running
on a private endpoint with a custom name) can be registered at runtime
and materialize as a `Model`. Today the closed enum drops them on the
floor.

**M5.T1 — `Api` newtype.** Replace
`pub enum Api { OpenAICompletions, ... }` with:
```rust
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Api(String);

impl Api {
    pub const OPENAI_COMPLETIONS: &str = "openai-completions";
    pub const ANTHROPIC_MESSAGES: &str = "anthropic-messages";
    // ... 11 built-ins as &'static str constants
    
    pub fn custom(s: impl Into<String>) -> Self { Self(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
    
    pub fn is_built_in(&self) -> bool {
        matches!(self.0.as_str(), Self::OPENAI_COMPLETIONS | ... )
    }
}
```

Plus const accessors: `pub const fn openai_completions() -> Api { ... }`.

This breaks every `match api { Api::OpenAICompletions => ..., ... }`.
Replace with `match api.as_str() { Api::OPENAI_COMPLETIONS => ..., _ => ... }`
(non-exhaustive via the `_` arm, which is the whole point — custom
providers fall through to a default handler).

Acceptance: `cargo build --workspace` passes.

**M5.T2 — `Provider` newtype.** Same pattern.

**M5.T3 — Migrate call sites.** Run a migration script:
```sh
rg "Api::([A-Z][A-Za-z]+)" --type rust -l | xargs sed -i ...
```
Then walk every match-arm by hand to add `_ => ...` defaults.

Acceptance: `cargo test --workspace` green.

**M5.T4 — Wire registry.** `Client::register_provider` already takes
`Api` and `Provider`. After M5.T1/T2 it accepts custom strings without
modification. Add a test that registers a fake custom provider and
streams through `Client::stream`.

Acceptance: 1 end-to-end test using a custom-named API.

### Milestone 6 — Smaller TS file backfills

Codex's review listed 8 small TS files with no Rust counterpart. Most
are utility-grade and can be ported quickly. They land last because
each one is independent and low-impact.

**M6.T1 — `coding-agent/utils/` backfills.**
- `path-utils.ts` → `utils/path_utils.rs`. Functions: `resolveReadPath`
  (the macOS NFD / curly-quote / NBSP variant probe), `isPathSafe`,
  others.
- `render-utils.ts` → `utils/render_utils.rs`. Functions: tool result
  formatting, ANSI strip helpers.
- `truncate.ts` → `utils/truncate.rs`. Word/line-boundary aware
  truncation. (Don't conflate with existing `visual_truncate.rs` which
  is TUI-specific.)

Acceptance: each ported file has at least one usage from another module
(otherwise it's dead code).

**M6.T2 — `coding-agent/core/` backfills.**
- `file-mutation-queue.ts` → `core/file_mutation_queue.rs`. Batches
  file edits, rolls back on partial failure.
- `output-accumulator.ts` → `core/output_accumulator.rs`. Streaming
  truncation for tool output.
- `tool-definition-wrapper.ts` → `core/tool_definition_wrapper.rs`.
  JsonSchema validation layer for tool args.

Acceptance: `tools/edit.rs` and `tools/write.rs` use the file-mutation
queue. `tools/bash.rs` uses the output accumulator.

**M6.T3 — TUI undo-stack.** TS `tui/undo-stack.ts` is a generic
undo/redo stack used by the editor. The Rust editor likely already has
some undo wired internally; check `packages/tui/src/components/editor.rs`
and either extract to a standalone `tui/undo_stack.rs` or document the
behavior already in `editor.rs`.

Acceptance: `Tab` / `Ctrl-Z` / `Ctrl-Shift-Z` in the editor restore
prior edit states across at least 5 ops.

**M6.T4 — `export-html` pipeline.** TS
`coding-agent/core/export-html/` is a directory of templates +
renderers that turn a session into a standalone HTML page. The current
Rust `core/export.rs` only handles JSONL. Port the HTML template, CSS
asset, and the rendering pipeline.

Acceptance: `/export session.html` produces a valid HTML file that
opens in a browser and renders all session messages with markdown
formatting and syntax highlighting.

**M6.T5 — Wire usage.** Ensure each new module has at least one caller.
Dead-code lints will surface anything unwired.

Acceptance: `cargo build --workspace -- -D dead-code` passes.

## Concrete Steps

Each milestone is independent. To start any milestone, branch from main:

```sh
cd /Users/wanggang/dev/00/hand-ai
git fetch origin && git checkout -B feat/parity-final-stretch-m1 origin/main
```

For each milestone, work task-by-task with atomic commits. Verification
after each task:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --features model/faux -- -D warnings
cargo fmt --all -- --check
```

Expected output for the test command (post-baseline):

```
... 2176 passed; 0 failed; 0 ignored; ...
```

Each milestone bumps the test count by approximately:
- M1: +12 to +20 (wire-fixture round-trips)
- M2: +25 to +40 (per-field round-trips and merge tests)
- M3: +15 to +25 (10 dispatch tests + integration)
- M4: +20 to +30 (cut-point edge cases, branch traversal)
- M5: +5 to +10 (custom provider registration)
- M6: +15 to +25 (8 new utilities, each with ≥2 tests)

**Final expected test count after all milestones: ~2300–2400.**

## Validation and Acceptance

The plan is complete when all of the following are observable:

1. **Wire interop**: a TS pi-mono client started against the Rust RPC
   server in `--rpc` mode receives 0 unknown-field errors.
   ```sh
   cd /Users/wanggang/dev/00/hand-ai
   cargo run -p hand-coding-agent -- --rpc &
   cd /Users/wanggang/dev/opensource/pi-mono
   bun run packages/coding-agent/src/modes/rpc/rpc-client.ts --connect-stdio
   # Send a few prompts; observe that AssistantMessageEvent payloads
   # parse cleanly into the TS client's typed handlers.
   ```

2. **Settings round-trip**: a pi-mono `settings.json` file copies
   verbatim to the Rust agent's settings location and loads cleanly:
   ```sh
   cp /Users/wanggang/.pi/agent/settings.json /Users/wanggang/.hand/agent/settings.json
   cargo run -p hand-coding-agent -- --print "echo hello"
   # No "unknown field" warnings in stderr.
   ```

3. **Slash commands**: each of the 10 commands works in interactive mode:
   ```sh
   cargo run -p hand-coding-agent
   # In TUI: /export tmp.html  → file appears
   # In TUI: /import tmp.jsonl → messages appended
   # In TUI: /fork              → new branch session created
   # ... etc.
   ```

4. **Compaction on branched session**: create a forked session, hit the
   compaction threshold, verify a `BranchSummary` entry appears in the
   resulting JSONL.

5. **Custom provider**: register an HTTP-mock provider with name `local`
   via the registry, stream from it via the Rust binary.

6. **Test count**: `cargo test --workspace` reports ≥2300 passing.

7. **TODO sweep**: `git grep "TODO(parity):" packages/` returns ≤5
   matches, all of them in `// TODO(parity): theme integration deferred`
   form (that's the pre-existing one for component theming).

## Idempotence and Recovery

Each milestone branch can be discarded and re-attempted with no global
state damage — all changes are file-level edits in the Rust workspace.
If a cherry-pick conflicts (likely on `mod.rs` files when multiple
milestones land in parallel), the union of `pub mod` lines is always
the right resolution.

For M5 (open Api/Provider) specifically: the migration touches ~50
call sites. If `cargo build` breaks midway, `git stash`, `git reset
--hard origin/main`, and start over with a fresh branch.

## Artifacts and Notes

### Wire-type catalog (M1.T1 deliverable, to be filled)

| Type | Crate | Wire? | RenameAll | Notes |
|---|---|---|---|---|
| `AssistantMessageEvent` | model | yes | snake_case (variant) → camelCase (fields) | enum-tag is `type` |
| `AgentEvent` | agent | yes | snake_case (variant) → camelCase (fields) | crosses RPC |
| `Settings` | coding-agent | yes (M2 changes this) | YAML kebab today; JSON camelCase target | |
| `ProxyAssistantMessageEvent` | agent | yes (already correct) | already per-field renamed | |
| `RpcMessage` | coding-agent | yes | check current shape | top of envelope |
| ... | | | | |

(Fill the rest during M1.T1.)

### Migration script for M5.T3

```sh
#!/usr/bin/env bash
set -euo pipefail
cd /Users/wanggang/dev/00/hand-ai
# After M5.T1/T2 land, this script rewrites match arms:
rg -l 'Api::OpenAICompletions' packages/ --type rust | while read f; do
  sed -i.bak \
    -e 's/Api::OpenAICompletions/Api::openai_completions()/g' \
    -e 's/Api::AnthropicMessages/Api::anthropic_messages()/g' \
    "$f"
done
find packages -name "*.bak" -delete
cargo fmt --all
cargo build --workspace
# Manual cleanup of `match` exhaustiveness errors follows.
```

## Interfaces and Dependencies

After this plan lands, the following public surface must exist:

### `model` crate

```rust
// In src/types.rs

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Api(String);

impl Api {
    // Built-in identifier constants (string form)
    pub const OPENAI_COMPLETIONS: &str = "openai-completions";
    pub const ANTHROPIC_MESSAGES: &str = "anthropic-messages";
    // ... 11 built-ins
    
    pub fn openai_completions() -> Self { Self(Self::OPENAI_COMPLETIONS.into()) }
    pub fn anthropic_messages() -> Self { Self(Self::ANTHROPIC_MESSAGES.into()) }
    // ... constructors for each built-in
    
    pub fn custom(s: impl Into<String>) -> Self { Self(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn is_built_in(&self) -> bool { /* match table */ }
}

// Same shape for Provider.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    #[serde(rename_all = "camelCase")]
    Start { partial: AssistantMessage },
    
    #[serde(rename_all = "camelCase")]
    TextStart { content_index: u32, partial: AssistantMessage },
    // ... rest of variants get the same per-variant rename_all
}
```

### `agent` crate

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    AgentStart,
    
    #[serde(rename_all = "camelCase")]
    AgentEnd { result: AgentResult },
    // ... etc.
}
```

### `coding-agent` crate

```rust
// In src/core/settings.rs

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    // existing fields ...
    
    pub last_changelog_version: Option<String>,
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    pub transport: Option<TransportSetting>,
    pub steering_mode: Option<SteeringMode>,
    pub follow_up_mode: Option<FollowUpMode>,
    pub theme: Option<String>,
    pub branch_summary: Option<BranchSummarySettings>,
    pub retry: Option<RetrySettings>,
    pub hide_thinking_block: Option<bool>,
    pub quiet_startup: Option<bool>,
    pub shell_command_prefix: Option<String>,
    pub npm_command: Option<Vec<String>>,
    pub collapse_changelog: Option<bool>,
    pub enable_install_telemetry: Option<bool>,
    pub enable_skill_commands: Option<bool>,
    pub terminal: Option<TerminalSettings>,
    pub images: Option<ImageSettings>,
    pub enabled_models: Option<Vec<String>>,
    pub double_escape_action: Option<DoubleEscapeAction>,
    pub tree_filter_mode: Option<TreeFilterMode>,
    pub thinking_budgets: Option<ThinkingBudgetsSettings>,
    pub editor_padding_x: Option<u32>,
    pub autocomplete_max_visible: Option<u32>,
    pub show_hardware_cursor: Option<bool>,
    pub markdown: Option<MarkdownSettings>,
    pub warnings: Option<WarningSettings>,
    pub session_dir: Option<PathBuf>,
}

// In src/core/session_manager.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionEntry {
    Message { id: String, parent_id: Option<String>, message: model::Message, .. },
    BranchSummary { id: String, parent_id: Option<String>, summary: String, .. },
    CustomMessage { id: String, parent_id: Option<String>, body: String, message_type: String },
    BashExecution { id: String, parent_id: Option<String>, command: String, output: String, exit_code: Option<i32> },
    ThinkingLevelChange { id: String, parent_id: Option<String>, level: ThinkingLevel },
    ModelChange { /* existing */ },
    Compaction { /* existing */ },
    Label { /* existing */ },
}

// In src/core/compaction/compactor.rs

pub fn prepare_compaction(
    entries: &[SessionEntry],
    settings: &CompactionRuntimeSettings,
) -> Result<PreparedCompaction, CompactionError>;

pub fn find_valid_cut_points(entries: &[SessionEntry]) -> Vec<usize>;
pub fn find_turn_start_index(entries: &[SessionEntry], from: usize) -> usize;
pub fn find_cut_point(entries: &[SessionEntry], target_tokens: usize) -> usize;

// In src/utils/

pub mod path_utils;       // resolveReadPath + variants
pub mod render_utils;     // tool result formatting + ANSI strip
pub mod truncate;         // word/line-boundary truncation

// In src/core/

pub mod file_mutation_queue; // batched, rollback-able edits
pub mod output_accumulator;  // streaming truncation
pub mod tool_definition_wrapper; // JsonSchema validation
```

---

PLAN READY FOR REVIEW:
- Title: parity-final-stretch
- Plan structure: 6 milestones (independent, can land in any order; M4 has prerequisite for compaction-dependent features)
- Open risks: M5 (`Api`/`Provider` newtype migration touches ~50 call sites — non-trivial)
- Estimated total effort: 4–6 implementer batches, +200 tests
- Plan committed to: `docs/exec-plans/parity-final-stretch.md`

→ Approve, or tell me what to change.
