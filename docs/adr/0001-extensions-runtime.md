# ADR-001: Extensions Runtime Architecture

**Status:** Accepted
**Date:** 2026-05-07
**Decision-makers:** Gump (project lead), Claude Opus 4.7 (controller)

## Context

`hand-coding-agent` is a Rust port of `pi-coding-agent`. In the TypeScript original, extensions are JS modules loaded in-process: each module has full access to the runtime API (lifecycle hooks, custom providers, dynamic tool registration, slash commands, TUI overlays). The reference set under `pi-mono/.../examples/extensions/` contains 80+ examples ranging from one-line audit hooks to full-screen overlays (`doom-overlay`, `snake`, `status-line`) and provider shims.

Porting this to Rust is non-trivial because the JS-style "drop a file in `extensions/` and reload" model has no direct Rust equivalent. The agent is a compiled binary; loading arbitrary user code at runtime requires picking an extension runtime explicitly. We must make that choice now, before Phase 3 work lands, because it shapes both the public `Extension` API and the `extension.toml` manifest format.

Constraints driving the choice:

- **Coding agent, not a general plugin host.** We do not need a marketplace, a sandboxed third-party ecosystem on day one, or hot-swap-everything semantics. We need to support the patterns that already exist in `pi-mono/.../examples/extensions/` and that Gump writes for himself.
- **Match the pi standard for depth.** Pi extensions can register custom LLM providers, intercept tool calls, mutate session state, and draw to the TUI. Anything narrower than that is a regression for power users.
- **Rust native.** No embedded JS engine (V8, QuickJS, `deno_core`). Embedding a JS runtime would re-introduce the dependency surface the Rust port is meant to remove, and it would split the type system between Rust core and JS extensions.
- **Phase 3 budget is ~8 days.** A solution that demands 13+ days of foundational plumbing (e.g., a complete WASI Preview 2 host) is out of scope for this phase.

## Options Considered

### Option A: Subprocess JSON-RPC alone

**What it is.** Every extension runs as its own OS process. The agent spawns the subprocess at session start and communicates over stdio using the JSONL JSON-RPC protocol introduced in Phase 1 (`src/rpc/{types,jsonl,server}.rs`). The extension declares its capabilities (hooks, slash commands, custom tools) in `extension.toml`; the agent forwards relevant events as RPC requests; the extension responds.

**What it covers well.** Polyglot extensions (Bash, Python, Bun, Node, anything that can read/write JSONL on stdio). Strong fault isolation — a panicking extension cannot crash the agent. Natural sandboxing boundary if combined with OS-level confinement later. Battle-tested pattern (LSP, MCP).

**Why rejected as the *only* runtime.** Roughly 40% of pi extensions need deeper integration than IPC can serve cleanly:

- **Custom LLM providers.** Pi extensions register provider shims that intercept the *streaming* token loop. Bridging a streaming provider through stdio JSONL adds a per-token RPC hop and stalls under load.
- **In-process TUI overlays.** Examples like `doom-overlay`, `snake`, and `status-line` paint directly into the agent's `ratatui` frame. Doing that across IPC requires a framebuffer-shaped RPC primitive that does not exist yet (Tier 4 in the future-work list).
- **Synchronous mutation of session state.** Pi extensions sometimes mutate the message log mid-turn before the next LLM call. A round-trip RPC for every mutation is awkward and slow.

Subprocess-only would force these cases into either a degraded form or a "host-side helper" that defeats the purpose of an extension. We keep subprocess as Tier 2 but not as the only tier.

### Option B: WASM via wasmtime

**What it is.** Extensions compile to `wasm32-wasi` modules. The agent embeds `wasmtime`, instantiates each module, and exports a host API as WASI-style imports. Extensions get sandboxed memory, deterministic execution, and language portability (any source language with a wasm32-wasi target).

**What it covers well.** Sandboxing. Distribution (one `.wasm` file per extension, no toolchain on the user's machine at install time). Deterministic resource accounting.

**Why rejected.** WASI Preview 1 is too narrow for ~50% of pi use cases:

- **Filesystem watching** (`notify`-style fs events) — no WASI primitive; needs custom host imports.
- **Process spawning** — not in WASI Preview 1; arrived in Preview 2 (`wasi:cli`) which is still maturing and not all guests support it.
- **Raw HTTP** beyond a host-provided client — needs `wasi:http` host imports, written and maintained by us.
- **Raw terminal access** — pi overlays read/write the terminal directly; no WASI surface for that.
- **Framebuffer-style overlays** — same problem as Tier 2, only with an additional layer of indirection.

Each gap is solvable by writing custom host imports, but the cost compounds: a Phase-3-grade WASM host with the imports pi extensions need is roughly five engineer-days *before* a single extension is ported. That blows the phase budget. The benefit (sandboxed third-party extensions) is real but not Phase 3's problem — Gump's own extensions and trusted contributors do not need to be sandboxed from his own agent.

We park WASM as **Tier 3, Phase 6+**, when sandboxed third-party distribution becomes a live requirement.

### Option C: Native dylib via libloading

**What it is.** Extensions compile to `cdylib` crates. The agent uses `libloading` to `dlopen` them at runtime and resolve a known C-ABI symbol that returns an `Extension` vtable.

**What it covers well.** Out-of-tree extensions without recompiling the host. Full performance (native code). Full Rust ecosystem access for extension authors.

**Why rejected.**

- **Safety.** A dylib runs as the host process with no boundary at all. "Load this `.so` from the internet" is equivalent to "run this binary as me." For a coding agent that already has shell and filesystem access, there is no security story to tell here.
- **ABI fragility.** Rust has no stable ABI. Every `rustc` upgrade can silently break every shipped extension. We would either pin `rustc`, or ship a C ABI shim that re-derives most of `wasmtime`'s typing problems by hand.
- **Maintenance disaster.** Version skew between the host's `Extension` trait and a third party's compiled dylib produces undefined behavior, not a compile error. The debugging surface is enormous.

Native dylib is the wrong tool for this job. We do not pursue it.

### Option D: Declarative manifest only

**What it is.** Each extension is a TOML/JSON file describing static contributions: a list of slash commands that map to shell command templates, a list of static prompt fragments, a list of allow/deny tool patterns. No code, only data.

**What it covers well.** Trivial cases — "add `/deploy` that runs `./scripts/deploy.sh`" — and the set of extensions that pi calls "hooks" but are actually static `if matches: run command` rules.

**Why rejected.** Roughly 70% of pi extensions express *dynamic* logic that no manifest schema can capture without becoming a programming language:

- Conditional hook firing based on tool arguments (e.g., "block `Edit` when the path matches a glob *and* the repo is dirty *and* a file is open in the user's editor").
- Custom LLM providers (impossible — providers are code, not config).
- Dynamic tool registration whose schema is computed at session start.
- TUI overlays.

A manifest-only system would force the "real" extensions to live somewhere else, defeating the goal of a single extension model. We do support a manifest (`extension.toml`) but only as the *declarative front matter* of code-bearing extensions, not as the runtime itself.

## Decision: Hybrid Tier 1 + Tier 2

We adopt a two-tier architecture. Every extension is one or the other; both share the same `extension.toml` schema for capability declaration.

### Tier 1 — Compiled-in Rust trait extension

Tier 1 is the default. An extension is a small Rust crate under `packages/coding-agent/examples/extensions/<name>/` that depends on `hand-coding-agent` and implements:

```rust
pub trait Extension: Send + Sync {
    fn manifest(&self) -> &ExtensionManifest;

    // Lifecycle
    fn on_load(&self, ctx: &ExtensionContext) -> Result<()> { Ok(()) }
    fn on_session_start(&self, ctx: &SessionContext) -> Result<()> { Ok(()) }
    fn on_session_end(&self, ctx: &SessionContext) -> Result<()> { Ok(()) }

    // Tool-call interception
    fn before_tool_call(&self, call: &mut ToolCall, ctx: &SessionContext)
        -> Result<HookOutcome> { Ok(HookOutcome::Continue) }
    fn after_tool_call(&self, call: &ToolCall, result: &mut ToolResult,
        ctx: &SessionContext) -> Result<()> { Ok(()) }

    // Registration (called once, at on_load)
    fn slash_commands(&self) -> Vec<Box<dyn SlashCommand>> { vec![] }
    fn custom_tools(&self) -> Vec<Box<dyn Tool>> { vec![] }
    fn custom_providers(&self) -> Vec<Box<dyn Provider>> { vec![] }
}
```

(The exact signatures are T3.2; the shape above is illustrative, not normative.)

Registration is **static**, at compile time. We use cargo features to gate each extension and the `inventory` crate (or an equivalent linker-collected static registry) to enumerate active extensions at startup. Building `hand` with `--features ext-auto-commit,ext-permission-gate` includes those two; the binary contains nothing for the others.

**When to use Tier 1.** Any extension that needs to:
- Register a custom LLM provider.
- Hook tool calls in the hot path.
- Render TUI overlays.
- Mutate session state synchronously.
- Share types with the agent (e.g., `Message`, `ToolCall`, `Provider`) without serialization.

This is the 85% case. It mirrors how pi extensions sit in-process today, but with Rust type-checking instead of JS duck-typing.

### Tier 2 — Subprocess JSON-RPC extension

Tier 2 is for everything Tier 1 cannot or should not do. An extension lives in any directory with an `extension.toml` and an entry-point command (any executable). At session start the agent spawns the entry point as a child process and exchanges JSONL JSON-RPC messages over stdio, reusing `src/rpc/types.rs`, `src/rpc/jsonl.rs`, and `src/rpc/server.rs` from Phase 1.

The protocol is symmetric: the agent sends events (`session.started`, `tool.before_call`, `tool.after_call`, `slash.invoke`, …) and the extension can both *respond* (e.g., return a `HookOutcome`) and *call back* into the agent (e.g., `agent.add_message`, `agent.read_file`). This is the same inversion-of-control pattern LSP and MCP use.

`extension.toml` declares the entry point, the protocol version, and the subscribed capabilities:

```toml
schema_version = 1
name = "notify"
tier = "subprocess"
entry = ["python3", "main.py"]
hooks = ["after_tool_call"]
slash_commands = ["notify"]
```

**When to use Tier 2.** Any extension that:
- Is written in a language other than Rust (Python, Bash, Bun, Node, Go).
- Is a thin wrapper around an external CLI and does not benefit from in-process integration.
- Comes from a less-trusted source where process isolation is desirable.
- Wants to be hot-reloaded without rebuilding `hand` (kill the subprocess, restart it).

This is the 15% case.

### What's covered

| Use case | Tier 1 | Tier 2 |
|---|---|---|
| Lifecycle hooks (`on_load`, `on_session_start`) | yes | yes (RPC notification) |
| Tool-call interception (`before/after`) | yes, sync | yes, RPC round-trip |
| Custom slash commands | yes | yes |
| Custom tools (static schema) | yes | yes |
| Custom tools (schema computed at runtime) | yes | yes |
| Custom LLM providers (streaming) | yes | not recommended (per-token RPC) |
| TUI overlays / framebuffer rendering | yes | deferred to Tier 4 |
| Synchronous session-state mutation | yes | awkward (RPC round-trip) |
| Polyglot / non-Rust authors | no | yes |
| Sandbox boundary | no | process-level only |

Coverage of the pi reference set:

- Tier 1 alone handles ~85%, including all hook-only extensions (`auto-commit-on-exit`, `permission-gate`, `dirty-repo-guard`), provider shims, and overlay extensions.
- Tier 2 handles the remaining ~15%, dominated by polyglot scripts (`notify` shelling out to `osascript`, `hello-bun`) and extensions that wrap an external service.
- Combined coverage is ~100% of the pi extension catalog. WASM (Tier 3) is not required for any current pi extension.

## Rationale

Three reasons drive the hybrid choice over any single-runtime alternative:

1. **Tier 1 is strictly better than pi's JS model for the 85% case.** Rust gives us type-checked hook signatures, async/IO with zero-cost abstractions, no GC pauses inside the streaming loop, and no need to invent a parallel "extension type system." Pi extensions catch hook-shape mismatches at runtime; ours catch them at `cargo check` time. The cost (recompile to add an extension) is paid by the developer, not the user, and is sub-30-seconds for a small crate on a warm cache.

2. **Tier 2 reuses Phase 1's RPC stack.** The protocol exists, the framing exists, the server exists. Tier 2 is mostly a spawner plus an event mapper; we estimate ~90% code reuse on the host side. Extensions become inverted RPC clients of the agent — the same pattern Microsoft chose for LSP and Anthropic chose for MCP. We are not inventing wire format or framing; we are renaming endpoints.

3. **Hybrid keeps Phase 3 in budget.** Subprocess-only would force re-architecting the streaming provider path (~3 days). WASM-only would cost ~5 days of host-imports work before the first extension lands. Hybrid spends those days on the actual `Extension` trait and the example ports, where the marginal value to the user is highest.

## Trade-offs accepted

- **No JS-style hot reload.** Tier 1 changes need a recompile; Tier 2 changes need a subprocess restart. Subprocess restart is sub-second; Tier 1 recompile of a small extension crate is a few seconds on a warm cache. Acceptable.
- **No "any extension can patch any other."** Pi's JS model lets one extension monkey-patch another's exports. Rust does not, and we do not try to emulate it. Cross-extension coordination must go through the agent (events, shared session state) rather than through prototype tricks. We treat this as a feature: every interaction is explicit and traceable.
- **Pi reference set needs re-authoring.** The 80+ pi examples will not run unchanged. Most port directly to Tier 1 Rust crates (the hook logic is small); a few become Tier 2 polyglot scripts. T3.5 ports three Tier 1 + two Tier 2 examples; the rest port over Phase 3.x in batches. This is real work but it is one-time and shrinks the codebase (Rust hook in 30 lines beats JS hook in 80 lines plus a `package.json`).

## Implementation Plan

Phase 3 ships:

- **T3.2** — Define `Extension` trait, `ExtensionContext`, `SessionContext`, `HookOutcome`, and the `extension.toml` schema (`ExtensionManifest`, `Tier`, etc.). Both tiers share the manifest type; tier-specific fields are tagged unions.
- **T3.3a** — Tier 1 hook wiring inside `agent_session`. Static registry via `inventory` (or a hand-rolled equivalent), feature-gated per extension. `on_load` fires once at startup; `before/after_tool_call` fires inside the tool dispatcher.
- **T3.3b** — Tier 2 subprocess spawner and RPC bridge. Lazy-spawn (a Tier 2 extension's process starts the first time one of its subscribed events fires, not at session boot) and clean shutdown on EOF or session end.
- **T3.4** — Slash-command and custom-tool registration via the `Extension` trait. Slash commands surface in the existing `/`-completion menu; custom tools surface in the LLM tool list with the extension's name as a namespace prefix.
- **T3.5** — Port three representative pi examples to Tier 1 (`auto-commit-on-exit`, `permission-gate`, `dirty-repo-guard`) and two to Tier 2 (`notify`, `hello-bun`). These exercise hooks, slash commands, and custom tools across both tiers.
- **T3.6** — End-to-end integration test: a fixture session with one Tier 1 and one Tier 2 extension wired into a mocked agent, exercising lifecycle, hooks, slash commands, and clean shutdown.

Future phases:

- **Phase 3.x** — Port the remaining ~75 pi examples in batches, prioritized by user demand.
- **Phase 6+ Tier 3 (WASM)** — Add a `wasmtime`-backed runtime when sandboxed third-party distribution becomes a live requirement. The `Extension` trait stays; the change is a new `tier = "wasm"` arm.
- **Phase 6+ Tier 4 (framebuffer RPC primitives)** — If Tier 2 ever needs in-process TUI rendering, define a framebuffer-shaped RPC surface and let subprocess extensions paint into it. Until then, TUI overlays stay Tier 1.

## Risks & Mitigations

**R-EXT-1 — Tier 1 binary size.** Each compiled-in extension adds N KB to the `hand` binary. With 80+ extensions this becomes meaningful.
*Mitigation.* Every extension is feature-gated and **off by default**. Users opt in with `--features ext-foo,ext-bar` at build time. The default `hand` ships with no extensions; common bundles (`ext-recommended`) are convenience meta-features.

**R-EXT-2 — Tier 2 subprocess overhead.** A session with N Tier 2 extensions spawns N subprocesses, each holding file descriptors and memory.
*Mitigation.* Lazy-spawn — a Tier 2 process starts only when one of its subscribed hooks first fires or its slash command is first invoked, not at session boot. Kill on stdin EOF. Document the per-process cost in the extension authoring guide.

**R-EXT-3 — Manifest schema drift.** `extension.toml` will evolve, but old extensions still need to load on new agents (and the converse, with graceful failure).
*Mitigation.* `schema_version` field at the top, mandatory. Parser uses `deny_unknown_fields` and emits a structured error when it sees a field from a future version. Bumping the schema version is a documented event with a migration note.

**R-EXT-4 — Tier 1 ABI compatibility across `hand` versions.** A user's example crate compiled against `hand-coding-agent 0.1` may not link against `0.2` if the `Extension` trait shifts.
*Mitigation.* Tier 1 extensions ship **in-tree** under `packages/coding-agent/examples/extensions/`, pinned to the exact `hand` version they live alongside. Out-of-tree third-party extensions are a Tier 2 use case (immune to ABI drift, by design). When/if Tier 3 (WASM) lands, it offers the same out-of-tree story with a stable wasm interface contract.
