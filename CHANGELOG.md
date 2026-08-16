# Changelog

All user-facing changes to the `hand` binary are documented here. The
format roughly follows [Keep a Changelog](https://keepachangelog.com/).
As of 0.3.0 every workspace crate shares one version, declared once in
the root `[workspace.package]` and inherited via `version.workspace =
true`; these entries track that unified version.

The `/changelog` slash command and the M5.4 startup auto-display both
read this file. Add new entries above the previous version with a
`## [X.Y.Z] - YYYY-MM-DD` header — the parser only accepts that shape.

## [Unreleased]

### Added

- `HAND.md` is inherited from ancestor directories. The lookup read the working directory alone, so a crate nested in a monorepo — or any subdirectory you happened to start `hand` from — saw none of the conventions declared above it, and the only workaround was to run from the root or duplicate the file. The walk now climbs from the working directory to the filesystem root, collecting each directory's `HAND.md` (or `HAND.MD`) plus its `.hand/context.md`, and orders them furthest ancestor first so the most specific context sits closest to the model's instructions. A linked worktree checked out inside its own repository is the one case where the walk would double up — the worktree and the main repository root hold the same tracked file — and that shared copy is now applied once ([#154](https://github.com/wanggang316/hand-ai/pull/154))

### Fixed

- Summarization requests no longer pay for a prompt-cache write. Compaction summaries, turn-prefix summaries, and branch summaries each wrap a transcript that is never sent again, so the entry they cached could never be hit — but they inherited the default cache retention and were billed at the provider's cache-write premium anyway (25% over base input on Anthropic). Every long session paid it on each compaction. They now opt out and are billed as plain input; nothing else about the summaries changes
- A session no longer reports itself as compacting after compaction is over. Writing the compaction entry to the session log could fail — a full disk, a revoked permission — and the in-progress flag was never cleared on that path, so `get_state` reported a compaction still running for the rest of the session. The flag is also cleared before the `compaction_end` event is emitted rather than after, so an RPC client that reacts to that event sees an idle session instead of the one it was just told had finished
- Sessions stored behind a symlink are listed again. The scan over stored projects tested each entry with a check that describes the entry itself rather than what it points at, so a session directory that was a symlink — sessions relocated to another volume, or a project directory that is itself a link — reported as "not a directory" and was skipped. Every session under it was invisible to `/resume` and to session listings, with no error to suggest anything had been dropped

## [0.4.2] - 2026-08-06

### Added

- Extensions can rewrite a tool result before the model sees it. `on_after_tool_call` returns a `ResultDecision`, and a `Replace` changes both what the model reads and what the transcript records — which is what makes redaction, truncation, and annotation expressible at all. The chain is sequential and each extension observes its predecessor's replacement, so a summariser registered behind a scrubber cannot reintroduce what the scrubber removed; a replacement that fails to parse is dropped and the tool's own output is kept ([#146](https://github.com/wanggang316/hand-ai/issues/146))
- Extensions can put context in front of the model without editing the user's prompt. `on_user_message` returns a `UserMessageOutcome` carrying an optional `additional_context` alongside its decision, so informing the model no longer costs the turn (previously `Cancel` was the only channel that reached it). Contributions are attributed to their extension and land as their own message ahead of the prompt, never merged into the user's, and are recorded in the transcript so a resumed session replays what the model actually read ([#147](https://github.com/wanggang316/hand-ai/issues/147))
- `on_turn_end` hook, fired once when the agent finishes working and is about to hand control back, carrying the assistant's closing text and the stop reason. `Cancel` keeps the agent working with the reason handed to the model as its next instruction. Re-entry is bounded at three continuations per turn so a hook that always refuses cannot bill an unbounded number of model round-trips. Gated on `capabilities.on-turn-end`; user-typed follow-ups still take priority over anything an extension asks for. The bundled `auto-commit-on-exit` example now derives its commit subject from what the agent actually said instead of a static string ([#148](https://github.com/wanggang316/hand-ai/issues/148))

### Changed

- **Breaking (embedders only):** two `Extension` trait methods changed their return type — `on_user_message` now returns `UserMessageOutcome` and `on_after_tool_call` returns `ResultDecision`. Extensions that override either must be updated; `HookDecision` converts into `UserMessageOutcome`, so `Ok(HookDecision::Continue.into())` is usually the whole change. Extensions that only take the trait defaults, and all Tier 2 (subprocess) extensions, are unaffected — the new wire fields are optional and existing responses parse unchanged. The `hand` binary itself is not affected.

## [0.4.1] - 2026-08-05

### Added

- `sqlite` cargo feature on `hand-coding-agent`, on by default. Embedders can now build with `default-features = false` to drop rusqlite — and with it the `links = "sqlite3"` claim cargo permits only once per dependency graph, which previously made the crate unusable alongside sqlx, diesel, or a different rusqlite version. The `hand` binary is unaffected; `SessionBackend::Sqlite` exists in every build and behaves exactly as before. Without the feature, selecting the sqlite backend fails with a message naming it instead of silently falling back to JSONL ([#141](https://github.com/wanggang316/hand-ai/issues/141))

### Fixed

- A session that fails to load says why. A corrupt log now names the line that could not be parsed; previously every such failure was reported as "No session header found", which blamed the header even when the header was intact and left no way to find the damaged line short of bisecting the file by hand. Applies to opening a session and to forking from one ([#142](https://github.com/wanggang316/hand-ai/issues/142))

## [0.4.0] - 2026-08-04

### Changed

- The interactive `hand` TUI is rebuilt on [ratatui](https://github.com/ratatui/ratatui) + crossterm, replacing the previous hand-rolled string-diff renderer. It runs in an inline viewport (the chat scrolls in your terminal's native scrollback instead of an alternate screen) with synchronized-output frames, which fixes the flicker, stale-frame, and resize/scrollback-leak problems of the old stack. Terminal images (Kitty graphics / iTerm2), the Kitty keyboard protocol, overlays/toasts, the theme system, and user keybinding config are all preserved.

### Added

- `@`-path autocomplete in the chat editor: typing `@<prefix>` opens a completion popup of matching working-directory paths; Tab or Enter accepts the highlighted candidate (Enter only submits when the popup is closed).
- Custom themes now recolour the UI: a `~/.hand/themes/<name>.json` theme named in settings (`theme: <name>`) is applied to the rendered interface; an unknown or malformed theme name falls back to the default palette with a startup notice instead of failing to start.
- Terminal progress signalling: OSC `9;4` progress state (indeterminate while a turn runs, error on failure, cleared afterwards) and OSC `133` prompt marks around each turn, enabling terminal prompt-jump and taskbar progress where supported.
- A custom `submit` keybinding (e.g. `submit: alt+enter`) is now honoured by the editor — the bound chord submits and the other Enter variants insert a newline.
- Slash-command autocomplete: typing `/` opens a completion popup over the same command registry `/help` is pinned against; matching is case-insensitive on the name prefix, and accepting an argument-taking command splices a trailing space.
- The model catalog is primed at startup from the local cache (`~/.hand-ai/models.json`) and refreshed in the background from the rolling catalog release, so `/model` and model resolution see newly published models without waiting on the network. `HAND_CATALOG_URL` overrides the source and `HAND_OFFLINE` skips the fetch; every fetch error is swallowed and the previous catalog keeps serving.
- `/settings` cycles a row's value in place with Tab / Shift+Tab and persists each change without closing the dialog; Enter confirms, or hands off to the model / login-provider picker on those rows. `default_thinking_level` and `default_provider` are now cycle-selectable enum rows (providers drawn from the live catalog) instead of free-text fields.

### Fixed

- Inline `!command` bash output larger than 64 KiB is truncated in-view with a "Full output: <path>" footnote pointing at the on-disk capture; empty added/removed lines and unified-diff `+`/`-` rows in tool results are now colour-coded (green/red) correctly.
- `/import <file>` replays the imported session into the transcript; `/login <unknown-provider>` reports a readable error instead of opening a dead credential dialog.
- An explicit provider prefix in a `--model` pattern (e.g. `openrouter/openai/gpt-4o-mini`) now wins over a configured `default_provider`, which previously rerouted the explicitly named provider silently.
- Selector search (`/model`, `/theme`, the pickers) matches whitespace-separated query tokens as contiguous substrings instead of scattered subsequences, so a query like `glm-5` no longer surfaces unrelated model ids.
- With the completion popup open, a fully typed command submits on a single Enter — accepting a candidate that would change nothing no longer costs an extra keypress.
- CJK and other wide-character prompts render as uniformly tinted user bubbles; padding is measured in display columns rather than Unicode scalars, and wrapped continuation rows keep their fill.
- A hangup (SIGHUP, e.g. the controlling terminal closing) takes the clean-exit path and restores the terminal — cooked mode, cursor, kitty keyboard, bracketed paste — instead of leaving the shell in raw mode.
- Models served over the Anthropic Messages API (Anthropic's own models, MiniMax, and any provider whose base URL points at an `/anthropic` gateway) stream their reply as it is produced instead of buffering the whole response and delivering it in one burst when the turn ends; interrupting such a turn now keeps the text already streamed ([#135](https://github.com/wanggang316/hand-ai/issues/135))
- `on_user_message` is a real hook instead of a manifest flag that did nothing: extensions declaring `capabilities.on-user-message` see each prompt before it is appended to the transcript and can rewrite it (`Replace` with the new text) or refuse the turn (`Cancel`, surfaced to the user with nothing persisted). Extensions that do not declare the capability are never called ([#134](https://github.com/wanggang316/hand-ai/issues/134))
- Subprocess (Tier 2) extension hooks run under a per-hook timeout instead of being able to hang a session forever. Budgets are configurable per extension via a `[timeouts]` table in `extension.toml` (defaults: 5s for `before_tool_call`, 2s for `after_tool_call`, 5s for lifecycle, 30s for custom tools and slash commands). A `before_tool_call` timeout fails closed — the call is blocked — unless the manifest opts into `on-before-tool-call-timeout = "continue"`; the child is killed and the extension is skipped for the rest of the session instead of costing a timeout per call ([#131](https://github.com/wanggang316/hand-ai/issues/131))
- Extension lifecycle hooks actually run: `on_load` fires once per extension before the first tool call of a session, and `on_shutdown` fires when the session is disposed. An extension whose `on_load` fails is dropped from the chain and reported instead of running degraded; Tier 2 subprocess children are killed at shutdown rather than lingering until the host exits ([#130](https://github.com/wanggang316/hand-ai/issues/130))
- Extension data directories are per extension and honor the host's `base_dir`: an extension now gets `<base_dir or cwd/.hand>/extensions/<name>/data` instead of one shared `<cwd>/.hand/extensions` for everybody. Embedders that pin `base_dir` no longer have extension state written into the user's repository ([#132](https://github.com/wanggang316/hand-ai/issues/132))
- Extension chain: a `Replace` verdict now re-runs the whole `before_tool_call` chain from the head, so an extension registered ahead of a rewriter re-inspects the arguments that actually reach the tool. The chain is bounded to three passes and cancels the call if it never converges ([#133](https://github.com/wanggang316/hand-ai/issues/133))
- `HookDecision::Replace` actually rewrites the tool call instead of being logged and dropped: `BeforeToolCallResult` carries `replace_args` again, and the agent loop runs the tool with the extension chain's arguments. The replacement is re-validated against the tool's JSON Schema — an invalid rewrite fails the call rather than reaching the tool — and the transcript keeps the model's original arguments ([#133](https://github.com/wanggang316/hand-ai/issues/133))

### Known Issues

- Inline `!command` bash renders a live header and loader while running but commits the command's output as a single finalized box on completion rather than streaming it chunk-by-chunk. This is because `AgentSession::run_bash` currently hard-codes `on_chunk = None`; per-chunk streaming needs a core-layer change deferred from this migration. Output is never lost — only its arrival is batched.
- The `/model` scoped/all-models Tab toggle and `/scoped-models` reordering operate on a session subset, but populating that subset from persistent `enabled_models` configuration is not yet wired (session-only). Selecting a theme via `/theme` is limited to the built-in names (dark/light/high-contrast/system), which all render the default palette; visible recolouring is via custom theme JSON as above.

## [0.3.1] - 2026-07-22

### Added

- `session-backend` setting (`jsonl` | `sqlite`, default `jsonl`, global or per-project). With `sqlite`, sessions live in a single `sessions.db` database per session directory — the directory layout itself (per-project subdirs, `--session-dir`) is unchanged. On first use the database imports every existing JSONL session found in the directory; the `.jsonl` files are never modified or deleted. Resume, continue, the `/resume` picker, and fork all work against the database. Note: sessions created while on `sqlite` are not visible after switching back to `jsonl` — the import is one-way.
- `max` thinking level above `xhigh`. Anthropic's adaptive-thinking Claudes (Opus 4.6/4.7, Sonnet 4.6) send their native top `max` effort; budget-based and effort-capped providers clamp it to `high` exactly like `xhigh`; models with an explicit thinking-level map advertise it only when the map carries a `max` entry (DeepSeek V4's native `max` effort now surfaces as this level). Selectable via `/thinking max`, `--thinking max`, model patterns like `sonnet:max`, and the `default_thinking_level` setting.
- `Ctrl+X` in interactive mode copies the last assistant message to the clipboard — the keyboard shortcut for what `/copy` already does. Both paths share one routine, so status feedback and the OSC 52 remote-session fallback behave identically. Listed under `/hotkeys`; the `copy-last-message` action is declared in the keybindings config layer for remapping once runtime chord translation lands.

### Fixed

- Tool calls from responses cut off by the output token limit are no longer executed with possibly truncated arguments; each is failed with an explanatory result so the model re-issues it ([#97](https://github.com/wanggang316/hand-ai/pull/97))
- `--continue` reads the resumed session file once instead of fully parsing every session in the directory plus the winner three times; discovery now scans bounded headers ([#116](https://github.com/wanggang316/hand-ai/pull/116))
- `Ctrl+V` falls back to clipboard text when there is no image or the image read fails, instead of erroring or doing nothing ([#113](https://github.com/wanggang316/hand-ai/pull/113))
- `shell_path` setting expands a leading `~` ([#115](https://github.com/wanggang316/hand-ai/pull/115))
- Editor paste markers stay consistent through marker deletion, undo, and redo — no more literal `[paste #N]` leaking into submissions ([#107](https://github.com/wanggang316/hand-ai/pull/107))
- CRLF and CR line endings wrap correctly in rendered output ([#108](https://github.com/wanggang316/hand-ai/pull/108)); tabs render at the editor's tab width without corrupting terminal hyperlinks ([#111](https://github.com/wanggang316/hand-ai/pull/111))
- `alt+<symbol>` keybindings (e.g. `alt+,`) fire on legacy terminal protocols ([#109](https://github.com/wanggang316/hand-ai/pull/109)); no phantom cursor is left on screen after exit ([#110](https://github.com/wanggang316/hand-ai/pull/110))

## [0.3.0] - 2026-06-08

### Changed

- All workspace crates now share a single version, unified at `0.3.0` and inherited from the root `[workspace.package]` via `version.workspace = true`. Previously the `hand` binary (`hand-coding-agent`) and the `model` crate versioned independently (0.1.1 / 0.3.0) while the remaining crates sat at 0.1.0. A single source of truth makes coordinated releases a one-line bump.

## [0.1.1] - 2026-05-29

### Added

- `--workspace-sessions` flag opts session storage into the project-local `<cwd>/.hand/sessions/` layout. Explicit `--session-dir` still wins; the global default remains the home-based layout. (#24)
- `model::ClientBuilder` lets embedders register an arbitrary subset of built-in providers (or plug in a custom one) instead of always paying the binary-size cost of the full provider list. `Client::new()` is now sugar for `Client::builder().with_all_builtins().build()`. (#33)

### Changed

- `/help`, the autocomplete dropdown, and `SlashCommandTable::dispatch` share a single registry. Ten commands that previously worked but were missing from autocomplete (`/clear`, `/clone`, `/diagnostics`, `/extensions`, `/import`, `/login`, `/logout`, `/skills`, `/theme`, `/keybindings`) now show up. Two commands missing from `/help` (`/reload`, `/scoped-models`) are documented. (#37)
- `/session` reports session id, label, message count, model, provider, token totals + cost, and session duration — previously just model + provider. (#50)
- Extension- and skill-contributed slash commands now appear in the autocomplete dropdown alongside built-ins. Built-ins still shadow conflicting names. (#51)
- `/compact <custom instructions>` now forwards the steering text into the summary prompt instead of dropping it. Whitespace-only arguments fall back to the legacy bare-`/compact` behaviour. (#46)
- `/export` no longer advertises Markdown output (the implementation was never wired up); help text, the parse hint, and the runtime all agree on `jsonl / json / html`. The Markdown writer can return alongside the M6 batch. (#40)
- `/login` validates the provider argument against the live catalogue before opening the paste dialog, and matches case-insensitively so `/login Anthropic` reaches the OAuth flow that `/login anthropic` already used. (#47, #52)
- `/theme <name>` now persists the pick to `settings.yaml` via the same path `/settings → theme` uses (so the next session starts with the chosen theme) instead of silently dropping the resolved theme object. Live in-session colour swap is still pending the TUI-wide theme registry refactor. (#43)
- `/reload` actually swaps the session's `SettingsManager` instead of constructing a fresh one and discarding it — out-of-band edits to `~/.hand/agent/settings.yaml` are now visible to the running session without restart. (#48)
- `/settings` writes the pick to disk via `SettingsManager::apply_setting_by_id` + `save` so theme, auto_compact, hide_thinking_block, show_images, clear_on_shrink, and quiet_startup survive a restart. (#45)
- Slash command names are case-insensitive: `/HELP`, `/Quit`, `/MODEL` dispatch identically. Arguments stay case-sensitive because paths, model patterns, and theme names carry meaningful case. (#42)
- `/export` and `/import` expand a leading `~` (with or without `/`) to `$HOME`, matching shell semantics. (#44)
- `--resume <id>` with a literal `.jsonl` path uses it verbatim instead of re-appending the extension. (#25)
- Bare `--resume` (no value) is promoted to `--continue` semantics across the interactive, print, and legacy entry points so users resume the most-recent session instead of seeing `Session "" not found`. (#30)
- `--fork <id>` resolves the same way `--resume` does — exact id or prefix match against both the home-based and legacy session directories. (#27)
- The CLI rejects an explicit `--cwd` that does not exist (or points at a regular file) with a single clear error before any session is built. (#54)

### Fixed

- `--diagnostics` no longer prints the prefix/suffix of configured API keys — only "set (from $ENV_VAR)" so a diagnostics paste in an issue thread cannot leak stable key fragments. (#26)
- Google's `gemini-2.5-pro` default model works with `--print` and no `--thinking` flag. The disabled-thinking config now emits `thinkingBudget: -1` (dynamic) for the Pro family instead of the `0` Google's API rejects for thinking-only models. (#22)
- The TUI `DiffRenderer` honours the viewport height when computing cursor moves so the loader / editor shrink path no longer scrolls chat content into permanent scrollback.
- `/skills`, `/extensions`, `/changelog`, the `/compact` summary, and inline `!cmd` bash output now repaint immediately. Every push to the chat list is paired with a `request_render()` poke via the shared `push_component` helper. (#38, #49, #53)
- Bordered overlays (`/settings`, `/model`, `/thinking`, `/tree`, `/scoped-models`) size their inner components to `viewport_width - 2` so a full-width child like the horizontal-rule separator no longer overflows the box and wraps `│` onto the next line. (#39)
- Dismissing an overlay wipes the area it occupied — the residual box outline no longer lingers until the next unrelated command scrolls it off. (#41)
- `--list-models <provider>` returns the full provider catalogue again when `<provider>` exactly matches a known provider key, instead of intersecting with the partial substring match. (#6)
- `hand --print` exits non-zero on every error path it prints (`--cwd`, missing `@path`, unknown model, provider auth failure) so shell pipelines and `&&` chains see the failure. Pinned with an integration test that spawns the binary. (#54, #55)

### Documentation

- `crates/coding-agent/README.md` corrects the `-p` / `-v` flag bindings: `-p` is `--print`, `-v` is `--version`. The long-form `--prompt` and `--verbose` are documented as the long-only forms. (#29)
