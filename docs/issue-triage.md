# Issue Triage

Working log for `wanggang316/hand-ai` GitHub issues. Updated as issues open, get diagnosed, or close. Source of truth is GitHub; this file is the agent's working summary so triage state survives across sessions.

## Conventions

- **Closing**: commit message trailer `Closes #N` (one per logical fix). GitHub auto-closes when the commit lands on `main`.
- **Partial fixes**: explicit `Closes part of #N` in the trailer, plus a comment on the issue explaining what's done and what remains. Do NOT use a recognised close keyword.
- **Cross-repo fixes**: when the root cause is upstream (`openai-rust`, `google-genai-rust`, …), open the upstream PR first, merge + tag, then bump `crates/model/Cargo.toml` to the new tag in the hand-ai commit. The hand-ai commit carries the `Closes #N` trailer.
- **Reproduction harness**: use isolated `HOME` + `--cwd <tempdir>` to avoid the user's real auth.json / settings.yaml leaking into the run. See [Repro fixture](#repro-fixture) below.
- **Tests**: every fix lands with at least one regression test pinning the bug shape. Test name should reference the issue number in a doc-comment.
- **Memory rule**: keep `pi` / `pi-mono` / `upstream-pi` references out of code, tests, and commit messages (memory `feedback_no_pi_mono_references`). Doc text in `docs/` may reference them when explaining historical context — be conservative.

## Open issues

(none — backlog cleared 2026-05-25)

## Recently closed

Rolling log, most recent first. When this list grows past ~20, rotate the oldest to a separate `docs/issue-triage-archive.md`.

| #  | Title                                                       | Fix commit  | Notes |
| -- | ----------------------------------------------------------- | ----------- | ----- |
| 16 | UAT-013 /settings dialog doesn't render visibly             | `00a33db`   | Root cause: PTY probe didn't set TIOCSWINSZ, kernel reported (0, 0), overlay compositor clamped to 0×0. Substitute 80×24 fallback when crossterm reports zero dimensions in `ProcessTerminal::new` / `refresh_size`. Backend wiring (`6ef1cc4` `82833f8` `810f6c0`) was already correct. |
| 14 | fs_watch watches files directly (FSEvents)                   | `c3d016a` (+ earlier `ee68d5a` for rg) | Switched both `fs_watch::watch_files` and `SettingsManager::watch` to `PollWatcher` (250ms interval, `compare_contents(true)`) and watch parent dirs with per-event target filtering. FSEvents was silently dropping events on `/tmp` paths; polling sidesteps it for a 250ms latency floor. |
| 21 | README documents settings.json, actual is settings.yaml      | `1f80935`   | README Settings section now lists kebab-case YAML keys and notes snake_case aliases. |
| 20 | --export --resume `<id>` "No session header found"           | `cf42943`   | Resume lookup falls back from primary `~/.hand/agent/sessions/<flat>/` to legacy `<cwd>/.hand/sessions/` before erroring. Not-found message names BOTH attempted paths. |
| 19 | --export HTML shows everything as User, no assistants        | `58c22bf`   | Two cooperating bugs: duplicate `"type"` JSON key killed assistant deserialization (`content_type` field collided with `tag = "type"` enum wrapper — fix: `#[serde(skip)]` on all `content_type` fields); plus `send_message` double-persisted the user prompt (fix: skip `prompts_len` entries when writing back `result.messages`). |
| 18 | --model `provider/model` ignores provider, falls back        | `810f6c0`   | Root cause: prior #16 fix read `settings_manager.current()` which folds in `Settings::defaults()` (baked `default_provider: anthropic`). Now reads raw `project_layer()` / `global_layer()`. |
| 16 | UAT-013 backend wiring (provider/model/thinking from YAML)   | `82833f8`   | Loads `SettingsManager::from_cwd` in `SessionSetup::resolve`. Accepts snake_case YAML aliases. Render side still open as partial. |
| 13 | --mode json reports zero token usage                         | `59fd620`   | Upstream fix in `wanggang316/openai-rust#1` + #2 (tag v0.2.1). hand-ai bumps tag and wires `chunk.usage` into the assistant message. |
| 15 | --list-models shows nothing without credentials             | `0af039a`   | Fall back to full unfiltered catalogue when `registry.available()` is empty. |
| 17 | /models not recognized in interactive TTY                   | `d2cbc91`   | Add `"models"` as alias for `"model"` in `SlashCommandTable::dispatch`. |
| 14 (1/8) | tools_manager rg detection                            | `ee68d5a`   | `EnsureOptions::allow_system_lookup` opt-out for tests. |
| 12 | --provider google returns 404                                | `5a9d1ad`   | Strip trailing `/v1beta` from catalogue baseUrl. |
| 11 | --list-models <search> over-broad fuzzy matching             | `3399b20`   | Unify on `cli::list_models` + three-pass filter (exact provider → substring → fuzzy). |
| 10 | --model `<id>` without --provider falls back to anthropic    | `4877c33`   | `model_resolver::infer_provider_for_model_id` with priority tie-break. |
| 9  | --help leaks internal implementation details                 | `eec8cc4`   | Scrub doc-comments shown in `--help`. |
| 8  | TUI /export silently overwrites existing files               | `deb5195`   | `path.exists()` guard at top of `apply_export`. |
| 7  | TUI /quit does not terminate the session                     | `b9e49d5`   | Hard-exit on /quit and Ctrl+D after `tui.stop()`. |
| 6  | --list-models openai over-matches                            | `adb1065`   | Three-pass filter (early commit; superseded by #11's unification). |

## Cross-repo PR map

| Upstream PR                                       | hand-ai consumer commit | Issue |
| ------------------------------------------------- | ----------------------- | ----- |
| `wanggang316/openai-rust#1` (chunk usage field)   | `59fd620`               | #13   |
| `wanggang316/openai-rust#2` (re-export)           | (folded into `59fd620`) | #13   |
| openai-rust tag bump v0.1.0 → v0.2.1              | `59fd620`               | #13   |

## Repro fixture

Common harness for issues that hinge on settings.yaml or auth.json. Adapt as needed.

```bash
# Isolated HOME + work dir so the real ~/.hand never leaks into the probe.
UAT_HOME=$(mktemp -d)
UAT_WORK=$(mktemp -d)
mkdir -p "$UAT_HOME/.hand/agent" "$UAT_WORK/.hand"

# Optional: fake an api_key so SessionSetup can resolve auth.
cat > "$UAT_HOME/.hand/agent/auth.json" <<'EOF'
{ "anthropic": { "type": "api_key", "key": "sk-test-fake" } }
EOF

# Optional: settings layers (canonical kebab-case; snake_case also accepted).
cat > "$UAT_HOME/.hand/agent/settings.yaml" <<'EOF'
default-provider: anthropic
default-thinking-level: low
EOF
cat > "$UAT_WORK/.hand/settings.yaml" <<'EOF'
default-thinking-level: high
EOF

# Run with the isolated env.
env -i PATH="$PATH" HOME="$UAT_HOME" \
  /Users/wanggang/dev/00/hand-ai/target/debug/hand --cwd "$UAT_WORK" \
  --print --no-tools --no-session --prompt "say ok"
```

Avoid sourcing real provider credentials — fake `sk-test-fake` is enough to exercise SessionSetup. The actual network call will fail at auth time, which is the natural cutoff for setup-side probes.

## Settings layer reminder

`SessionSetup::resolve` reads **raw** `project_layer()` / `global_layer()` for `default_provider` / `default_model` / `default_thinking_level`, NOT `current()`. The merged snapshot folds in `Settings::defaults()` which bakes `default_provider: Some("anthropic")` — using it as fallback would mask the slash-driven provider inference (regression target: #18). Honour this contract in any further session-setup changes.

## Investigation log

Append-only notes from issue investigation that don't fit elsewhere. Prefer one short paragraph per finding.

- **2026-05-25 — #16 split into backend + frontend halves.** Backend wiring (YAML → SessionSetup → effective model) confirmed working via 4 new tests. The `default_thinking_level: high` value reaches `setup.stream_options.reasoning = Some(ThinkingLevel::High)`. User's PTY probe still doesn't see "high" in the `/settings` overlay output — the next step is to verify the live overlay paint path (mounter / repaint timing), separate from the value-lookup path that is already green.
- **2026-05-25 — #14 root cause identified by user.** Updated title to "fs_watch watches files directly instead of parent directories, breaking macOS FSEvents". The fix shape: watch the parent directory and filter events to the target file, instead of registering a watch on the file inode directly. macOS FSEvents only fires on directory contents, not on individual file rewrites. Defer to focused pass.
- **2026-05-24 — openai-rust ownership.** `wanggang316/openai-rust` is owned by Gump, so upstream PRs can be merged + tagged within the same session. Bump the tag in `crates/model/Cargo.toml` and the hand-ai consumer change can ship in one commit.
- **2026-05-25 — pre-fix sessions are unrecoverable.** Sessions written before `58c22bf` have duplicate `"type"` keys in their assistant content blocks. The post-fix code can't deserialize those — `serde_json` rejects duplicate JSON keys at the value-parse level, before serde sees fields. New sessions are fine; old sessions silently load with 0 assistant messages. If users complain, a migration utility (read raw lines, strip duplicate `"type"` keys, rewrite) would unblock them.
- **2026-05-25 — session path layouts diverge.** Two on-disk layouts coexist: primary `~/.hand/agent/sessions/<flattened-cwd>/` (set by `default_session_dir` when HOME resolves) and legacy `<cwd>/.hand/sessions/` (set when HOME doesn't resolve, or written by old binaries). Issue #20 surfaced the read-side gap — `--resume` only checked primary. `cf42943` adds the legacy fallback. WRITE side still goes to primary only, which is correct for new sessions but means new writes never reach the legacy location even if the user is using the legacy convention by habit. If anyone writes to legacy on purpose, document it.
- **2026-05-25 — FSEvents on macOS is unreliable on tempfs.** Even with parent-dir watching (per #14's root cause), `notify::RecommendedWatcher` (FSEvents on macOS) silently dropped events on `/tmp`-style paths used in tests. Both `fs_watch` and the settings watcher now use `PollWatcher` (250ms, `compare_contents(true)`). 250ms latency floor is acceptable for settings reload / doc tracking; gains 100% reliability across platforms.
- **2026-05-25 — kernel-reported zero PTY size.** `crossterm::terminal::size` returns `Ok((0, 0))` (not Err) when TIOCGWINSZ comes back zero, which happens with `pty.fork()` from test harnesses that don't call TIOCSWINSZ. The overlay compositor then clamped to 0×0 and rendered nothing. `ProcessTerminal::new` + `refresh_size` now substitute `(80, 24)` for zero dimensions. Root cause behind issue #16's PTY-only failure mode (#16 backend wiring was already correct, just invisible).
