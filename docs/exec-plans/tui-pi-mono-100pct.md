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

### M1 — Responsive baseline (must-have)

The TUI must feel alive while the agent works.

1. **M1.1 Live footer refresh** — wire `agent_footer` to refresh after every `MessageStart` / `MessageEnd` / `ToolEnd` / `TurnEnd` event. Populate `usage`, `context_percent`, `git_branch`, `thinking_level`, `available_provider_count` from real session/model state.
2. **M1.2 Working / compaction / retry loaders** — port `bordered-loader` mount points between scrollback and editor, with escape-to-abort.
3. **M1.3 Input history (Up/Down recall)** — add `set_history`, history index state, Up/Down at the first/last visual line walks history. Persist last 100 entries per session.
4. **M1.4 Autocomplete provider wired** — port pi-mono's `CombinedAutocompleteProvider`: slash commands + arg completions + `@path` walker. Attach to editor at startup.
5. **M1.5 Welcome header** — compact key-hint header at session start: interrupt / clear / exit / model select / slash / quit. Expandable with Ctrl+O.
6. **M1.6 Visible errors** — when `session.send_message` returns an error, append a red `BashStatus::Error`-style chat entry so the user can see it without scrolling the cargo output.
7. **M1.7 Bash mode (`!` prefix)** — typing `!command` recolors the editor border bash-color, `!!` excludes from context. Submit pushes a `BashExecutionComponent` instead of an agent message.

### M2 — Slash command parity

Match pi-mono's command surface so muscle memory transfers.

1. **M2.1 `/settings` with real entries** — general / theme / thinking / transport / auto-compact / show-images / hide-thinking-block / double-escape submenus. Hook into `core::settings`.
2. **M2.2 `/thinking <level>` applies** — `session.set_thinking_level(...)`, footer reflects it.
3. **M2.3 `/resume <id>`** — load the picked session into the current process (replace `AgentSession`).
4. **M2.4 `/model` scoped-models toggle** — Ctrl+P cycles through the curated list, full overlay shows all.
5. **M2.5 `/hotkeys`** — reads the real `KeybindingsManager` so the table is correct.
6. **M2.6 `/login` OAuth path** — provider list comes from registry; OAuth providers run the browser flow, API-key providers open the manual dialog.
7. **M2.7 `/reload`** — re-read settings, keybindings, extensions, skills, prompts, themes.

### M3 — Visual / theme parity

Stop hard-coding ANSI; let the theme system actually theme.

1. **M3.1 Theme bridge** — components consume a `ThemeRef` instead of hard-coded SGR. `/theme` swaps the active palette and triggers a force-render.
2. **M3.2 Markdown syntax highlighting** — feed pi-mono's 9 syntax colors through `MarkdownComponent::set_syntax_theme`.
3. **M3.3 Per-thinking-level editor border color** — bind `EditorComponent::focused_border_color` to the active thinking level.
4. **M3.4 Footer color warnings** — context % `>70%` yellow, `>90%` red; subscription `(sub)` suffix when OAuth-only.
5. **M3.5 User message bg + assistant body** — read `userMessageBg`, `userMessageText`, markdown body colors from theme.

### M4 — Extension surface (nice-to-have)

Get the dead components onto the screen.

1. **M4.1 `Ctrl+G` external editor** — launches `$VISUAL` / `$EDITOR` on a temp file, pipes back into the buffer.
2. **M4.2 Image paste + inline render** — `terminal-image` port; paste an image → `[image #1 800×600]` marker; inline render in scrollback.
3. **M4.3 Drop files** — drag-drop a file path onto the terminal → `@path` insertion.
4. **M4.4 Session tree selector** — port `tree-selector` with branch ASCII art and filter modes; opened via `/tree`.
5. **M4.5 Scoped-models multi-select** — port `scoped-models-selector` and wire `/scoped-models`.
6. **M4.6 User-message selector for `/fork`** — replace the current "fork from entry id" prompt with the selector overlay.
7. **M4.7 Extension widget mount points** — port `extension-{selector,input,editor}` mounts.

### M5 — Polish

1. **M5.1 OSC133 / OSC 9;4 progress** — emit zone markers and terminal progress bars while the agent runs.
2. **M5.2 Tmux extended-keys diagnostic** — warn if tmux config blocks modifier keys.
3. **M5.3 Subscription / model-fallback / package-update notifications** at startup.
4. **M5.4 Changelog auto-display on update**.
5. **M5.5 Hide-thinking-block / tool-output-expansion toggles** (`Ctrl+T`, `Ctrl+O`).

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
