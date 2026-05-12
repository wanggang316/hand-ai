# TUI 100% Capability Alignment with pi-mono

Goal: make `hand`'s interactive TUI match `pi-mono`'s coding-agent TUI in every user-visible capability that matters for daily use. After this plan completes, a user who used `pi-mono` should feel at home in `hand`.

Inputs:
- `pi-mono` feature inventory (audit run 2026-05-12)
- our Rust port inventory (same day)
- repro test confirming the Enter submit path works in isolation

## Triage findings

The visual mess in the user's screenshot is mostly a symptom of two things, not one:
1. **41 `TODO(parity)` markers** across the interactive module — most components are wired structurally but theme/data/extension hooks are stubs.
2. **Driver `interactive-mode.ts` parity gap** — many of the things pi-mono does *between* keystrokes (footer refresh, loaders, autocomplete, history, theme application, working spinner) are not wired in our driver.

The editor's `render()` itself is correct (repro test). The submit path now works end-to-end (Enter test passes). The remaining gap is the *experience around the editor*.

## Milestones

Each milestone is independently shippable and ends in `cargo test` + `cargo build` green, with at least one new test exercising the change.

### M1 — Responsive baseline (must-have) — **DONE**

The TUI must feel alive while the agent works.

1. **M1.1 Live footer refresh** ✅ — wired via `refresh_footer` after every `MessageEnd` / slash action; footer view-model reads model + reasoning level + provider-credential count from live state.
2. **M1.2 Working / compaction / retry loaders** ✅ — `BorderedLoaderComponent` mounted between scrollback and editor on AgentStart / CompactionStart; Esc cancels via session token.
3. **M1.3 Input history (Up/Down recall)** ✅ — `EditorComponent` tracks per-session history; Up/Down at first/last visual line walks it.
4. **M1.4 Autocomplete provider wired** ✅ — `SlashCommandProvider` + `PathAutocompleteProvider` combined; `query_sync` fast path so the popup appears on the trigger keystroke.
5. **M1.5 Welcome header** ✅ — compact `hand v… {provider}/{model}` line + key-hint footer at session start; uses live keybinding labels via `raw_key_hint`.
6. **M1.6 Visible errors** ✅ — red banner `✘ Error  {msg}` pushed via `push_error` when `send_message` fails.
7. **M1.7 Bash mode (`!` prefix)** ✅ — `!cmd` runs inline as a `BashExecutionComponent`; `!!cmd` excluded from agent context.

### M2 — Slash command parity — **DONE**

Match pi-mono's command surface so muscle memory transfers.

1. **M2.1 `/settings` with real entries** ✅ — `build_settings_entries` projects live SettingsManager values (theme / auto_compact / hide_thinking_block / show_images / clear_on_shrink / quiet_startup). Write-back deferred (M2.1.2): selector emits Changed events, persisting via `SettingsManager::save` not yet wired.
2. **M2.2 `/thinking <level>` applies** ✅ — `apply_thinking_level` mutates `session.stream_options().reasoning`; `off`/`none`/`clear` map to `None`. Footer reflects via `refresh_footer`.
3. **M2.3 `/resume <id>`** ✅ — `AgentSession::switch_session` swaps in-place; scrollback wiped and replayed.
4. **M2.4 `/model` scoped-models toggle** ✅ — `settings.enabled_models` → `resolve_model_scope` → `scoped_models` on selector. Ctrl+P cycle is M4.5 (separate component).
5. **M2.5 `/hotkeys`** ✅ — table built from `KeybindingsManager::all()` + `TUI_KEYBINDINGS` descriptions, grouped by category.
6. **M2.6 `/login` OAuth path** ✅ — `oauth_id_for` maps provider → registry id; `run_oauth_login` runs `provider.login(callbacks)` with chat-routed URL / device-code surfacing, persists via `OAuthRegistry::save`. API-key providers fall through to manual paste.
7. **M2.7 `/reload`** ✅ — `apply_reload` re-runs `SettingsManager::from_cwd` and pings `get_keybindings`. Extensions / skills / prompts / themes reload waits on the ResourceLoader reload API (separate task).

### M3 — Visual / theme parity — **PARTIAL**

Stop hard-coding ANSI; let the theme system actually theme.

1. **M3.1 Theme bridge** ⏸ — deferred. Components consume hard-coded SGR; visually they match pi-mono dark already (after `0f1f9e9` / `10633c7` fixes to bubble colors and bg-reset handling). Doing it cleanly requires `Arc<Mutex<>>` shared components so `/theme` can flip palettes live — same structural prerequisite as M3.3 and M5.5.
2. **M3.2 Markdown syntax highlighting** ⏸ — deferred. `MarkdownComponent` has no `set_syntax_theme` hook; code blocks render fenced text in `mdCodeBlock` color but lexer-driven highlighting is a separate dependency (`syntect`-class crate) and a new theme slot.
3. **M3.3 Per-thinking-level editor border color** ⏸ — deferred. Editor border is set at construction; per-tick re-tinting requires the same `Arc<Mutex<EditorComponent>>` refactor as M3.1.
4. **M3.4 Footer color warnings** ✅ — already implemented: context `>90%` → `ERROR_FG`, `>70%` → `WARNING_FG` in `FooterComponent::render`.
5. **M3.5 User message bg + assistant body** ⏸ — deferred. Bubble bg/fg constants match pi-mono dark theme values verbatim; visually equivalent to a theme lookup. Replacing the constants with `theme.bg_ansi(...)` is structural cleanup, not a user-visible change.

### M4 — Extension surface (nice-to-have) — **DEFERRED**

Get the dead components onto the screen. None of these are blocking daily use; the components were ported but never wired.

1. **M4.1 `Ctrl+G` external editor** ⏸ — needs editor mutation (same structural blocker).
2. **M4.2 Image paste + inline render** ⏸ — no image-paste detector in stdin; OSC1337/Kitty image protocol component exists but no input path.
3. **M4.3 Drop files** ⏸ — needs paste payload classification.
4. **M4.4 Session tree selector** ⏸ — `tree-selector` ported, no `/tree` mount.
5. **M4.5 Scoped-models multi-select** ⏸ — `scoped-models-selector` ported, no `/scoped-models` mount.
6. **M4.6 User-message selector for `/fork`** ⏸ — overlay component ported; current `/fork <entry_id>` parser works, just lacks the picker.
7. **M4.7 Extension widget mount points** ⏸ — pending extension API integration.

### M5 — Polish — **DEFERRED**

1. **M5.1 OSC133 / OSC 9;4 progress** ⏸ — OSC133 zone markers are emitted by `UserMessageComponent` and `AssistantMessageComponent`. OSC 9;4 progress not wired.
2. **M5.2 Tmux extended-keys diagnostic** ⏸ — needs a startup check + warning surface.
3. **M5.3 Subscription / model-fallback / package-update notifications** ⏸ — requires update-check infrastructure.
4. **M5.4 Changelog auto-display on update** ⏸ — depends on M5.3.
5. **M5.5 Hide-thinking-block / tool-output-expansion toggles** ⏸ — same Arc<Mutex<>> blocker as M3.1.

## Status

**M1: 7/7 done. M2: 7/7 done.** Core daily-use loop matches pi-mono.

M3-M5 are a mix of (a) polish that's visually equivalent to the current state (M3.1, M3.5), (b) features blocked on the same structural refactor — moving `EditorComponent` and the message components behind `Arc<Mutex<>>` so the agent task can mutate them in-place (M3.3, M4.1, M5.5), and (c) genuinely new subsystems (M3.2 syntax theme, M4.2 image paste, M5.1 progress). They are deferred pending a follow-up "shared mutable components" refactor that opens those doors as a batch.

## Out of scope

Easter eggs (`/arminsayshi`, `/dementedelves`, Earendil announcement, daxnuts). Already ported as components; can stay un-wired without affecting daily use.

## Execution order

M1 → M2 → M3 → M4 → M5. M1 is the highest user-visible impact per LOC. M5 is purely polish.

Each milestone:
1. Implement.
2. `cargo build -p hand-coding-agent --bin hand` clean.
3. `cargo test -p hand-tui -p hand-coding-agent --lib` clean.
4. Manual smoke note in the commit body listing what to spot-check.
5. Commit atomically per logical change inside the milestone (per user preference).
