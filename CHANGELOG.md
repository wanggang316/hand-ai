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

### Fixed

- Models served over the Anthropic Messages API (Anthropic's own models, MiniMax, and any provider whose base URL points at an `/anthropic` gateway) stream their reply as it is produced instead of buffering the whole response and delivering it in one burst when the turn ends; interrupting such a turn now keeps the text already streamed ([#135](https://github.com/wanggang316/hand-ai/issues/135))
- Extension lifecycle hooks actually run: `on_load` fires once per extension before the first tool call of a session, and `on_shutdown` fires when the session is disposed. An extension whose `on_load` fails is dropped from the chain and reported instead of running degraded; Tier 2 subprocess children are killed at shutdown rather than lingering until the host exits ([#130](https://github.com/wanggang316/hand-ai/issues/130))
- Extension data directories are per extension and honor the host's `base_dir`: an extension now gets `<base_dir or cwd/.hand>/extensions/<name>/data` instead of one shared `<cwd>/.hand/extensions` for everybody. Embedders that pin `base_dir` no longer have extension state written into the user's repository ([#132](https://github.com/wanggang316/hand-ai/issues/132))
- Extension chain: a `Replace` verdict now re-runs the whole `before_tool_call` chain from the head, so an extension registered ahead of a rewriter re-inspects the arguments that actually reach the tool. The chain is bounded to three passes and cancels the call if it never converges ([#133](https://github.com/wanggang316/hand-ai/issues/133))

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
