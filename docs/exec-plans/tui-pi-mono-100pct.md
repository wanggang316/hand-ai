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

### M3 — Visual / theme parity — **MOSTLY DONE**

Stop hard-coding ANSI; let the theme system actually theme.

1. **M3.1 Theme bridge** ⏸ — deferred. Components consume hard-coded SGR; visually they match pi-mono dark already. Doing it cleanly is now unblocked by the shared-mutable-components refactor but the user-visible payoff is zero until `/theme` (light/high-contrast/etc.) ships, so it's parked.
2. **M3.2 Markdown syntax highlighting** ✅ — `MarkdownTheme::highlight` slot + lightweight tokenizer for rust/ts/js/python/json/bash/yaml/toml. Unknown languages fall back to a flat `code_fg`. Wired in `assistant_message.rs` text and thinking blocks. See `crates/coding-agent/src/modes/interactive/syntax_highlight.rs`.
3. **M3.3 Per-thinking-level editor border color** ✅ — `refresh_editor_border` runs each agent event tick and re-tints the focused-border SGR via the shared `Arc<Mutex<EditorComponent>>`.
4. **M3.4 Footer color warnings** ✅ — `>90%` → `ERROR_FG`, `>70%` → `WARNING_FG`.
5. **M3.5 User message bg + assistant body** ⏸ — deferred. Bubble bg/fg constants match pi-mono dark verbatim; visually equivalent.

### M4 — Extension surface — **DONE (modulo extension API)**

Components onto the screen.

1. **M4.1 `Ctrl+G` external editor** ✅ — `$VISUAL`/`$EDITOR`/`vi`, tempfile round-trip on a worker thread.
2. **M4.2 Image paste + inline render** ✅ — Ctrl+V reads `arboard` clipboard image, writes `$TMPDIR/hand-clipboard-…png`, inserts the path at cursor. Agent picks up the path through existing @-handling; `ImageComponent` renders it when the agent message references it.
3. **M4.3 Drop files** ✅ — `EditorComponent::set_paste_transform` hook; the driver wires `transform_dropped_file_paste` which strips quotes / `file://` / percent-decodes, checks the path exists, and rewrites to `@<relative>` (inside cwd) or `@<absolute>` (outside).
4. **M4.4 Session tree selector** ✅ — `/tree` mounts `TreeSelectorComponent` with a BFS-walked, dirs-first, noise-skipping row set.
5. **M4.5 Scoped-models multi-select** ✅ — `/scoped-models` mounts `ScopedModelsSelectorComponent` against `settings.enabled_models`.
6. **M4.6 User-message selector for `/fork`** ✅ — `/fork` with no arg mounts `UserMessageSelectorComponent`.
7. **M4.7 Extension widget mount points** ⏸ — pending the extension API. Not in scope for this plan.

### M5 — Polish — **DONE**

1. **M5.1 OSC133 / OSC 9;4 progress** ✅ — OSC133 zone markers from the message components; OSC 9;4 emitted at agent-event boundaries via `emit_terminal_progress`.
2. **M5.2 Tmux extended-keys diagnostic** ✅ — startup `check_tmux_keyboard_setup` shells `tmux show -gv extended-keys[/-format]` and surfaces a yellow warning when misconfigured.
3. **M5.3 Package-update notification** ✅ — startup async probe against crates.io via `HttpVersionFetcher` + `check_for_new_version`; yellow "Update available" banner with install command and changelog URL.
4. **M5.4 Changelog auto-display on update** ✅ — `maybe_show_changelog_on_update` compares `settings.last_changelog_version` against the running version, mounts a `CustomMessageComponent` with the new entries, bumps the recorded version. Fresh installs record silently. Resumed sessions skip.
5. **M5.5 Hide-thinking-block toggle** ✅ — Ctrl+T flips a process-wide atomic; every `AssistantMessageComponent` subscribes via `with_shared_hide_flag` so the change is live.

## Status

**M1: 7/7. M2: 7/7. M3: 3/5 done (M3.1/M3.5 deferred as visually equivalent). M4: 6/7 done (M4.7 pending extension API). M5: 5/5 done.**

The interactive TUI now matches pi-mono's daily-use capability surface. The two remaining `⏸` items are pure structural cleanup (M3.1/M3.5 swap constants for theme lookups) with zero user-visible change today, and M4.7 which blocks on a separate extension-API design.

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
