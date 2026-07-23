# User-Test Patterns

> Project-wide conventions for runtime user-level validation. Read this before writing
> or probing any plan's validation contract. Contract assertions live in
> `.harness-runtime/plans/<slug>/validation-contract.md`.

## Status

**Status:** Approved
**Last updated:** 2026-07-23

## Platforms in Scope

- **Terminal TUI** — the `hand` binary (interactive coding agent) and `examples/`
  demo binaries; this is the only end-user surface.
- **Rust library** — the `hand-tui` crate API, exercised through `cargo test`
  (unit + integration with ratatui `TestBackend`).

Not in scope: Web UI (`crates/web-ui` has its own conventions), HTTP APIs, mobile.

## Tooling per Platform

### Terminal TUI (interactive probing)

- **Primary:** `tmux` — scripted PTY sessions.
  - Start: `tmux new-session -d -s <case-id> -x 100 -y 30 'env HAND_HOME=<tmpdir> <cmd>'`
  - Drive: `tmux send-keys -t <case-id> <keys>` (use `-l` for literal text)
  - Observe: `tmux capture-pane -pt <case-id>` (plain text) or `-e` (with SGR styling)
  - Cursor: `tmux display -pt <case-id> '#{cursor_x},#{cursor_y}'`
  - Teardown: `tmux kill-session -t <case-id>`
- **Fallback:** `script -q <outfile> <cmd>` for raw byte capture (see protocol probes).
- **Ready signal:** poll `capture-pane` every 200ms (max 25 tries) until the editor
  border or the footer status line is visible. Note: `hand`'s chat editor renders no
  placeholder text (IME-collision avoidance) — do not wait for one. Never use fixed
  long sleeps.
- **Test seams (built into the deliverables):** the M1 demo exposes a deliberate
  panic key, a token-flood mode, and forced-capability env switches (incl. a
  force-kitty-keyboard flag); the M2 gallery exposes timed toast dismissal and an
  image-drop gesture; M3 ships a `mock-provider` fixture. Probes use these seams
  instead of inventing timing hacks.
- **Geometry:** default 100×30. Cases about narrow/short terminals set their own
  size explicitly (`-x 40 -y 10`) and say so.

### Terminal protocol emission (escape-sequence probes)

Inside tmux, the outer terminal's capabilities are tmux's — Kitty graphics and
enhanced keyboard flags do not pass through. To assert *protocol emission*
deterministically, run the binary with a forced capability env and capture raw
bytes, then grep for the sequence prefix:

- Kitty graphics: capture via `script -q out.raw <cmd>`, assert bytes contain `\x1b_G`
- iTerm2 images: assert bytes contain `\x1b]1337;File=`
- Kitty keyboard push/pop: assert bytes contain `\x1b[>...u` / `\x1b[<u` pairs
- Synchronized output: assert draw cycles are wrapped in `\x1b[?2026h` … `\x1b[?2026l`

Visual verification in a real graphical terminal (kitty/iTerm2 rendering an actual
image) is **expensive** tier — manual, milestone boundaries only.

### Rust library

- **Primary:** `cargo test --workspace --features model/faux` (baseline: green).
  Component-level rendering asserts use ratatui `TestBackend` buffer snapshots.
- **Lint gate:** `cargo clippy --workspace --all-targets --features model/faux -- -D warnings`
  and `cargo fmt --all -- --check`.
- **Ready signal:** none needed (hermetic).

## Case Dimensions

| Dimension | Mandatory? | What to check |
|---|---|---|
| Happy path | Mandatory | The case's primary success flow |
| Error path | Mandatory | At least one declared failure mode (bad input, missing state) |
| Edge values | Mandatory | Empty input; very long lines; CJK/emoji/regional-indicator width; narrow (≤40 cols) and short (≤10 rows) terminals |
| Resize | Mandatory for full-screen/inline UI cases | Behaviour on terminal width/height change mid-interaction |
| Degraded terminal | Mandatory for protocol-dependent cases | No kitty keyboard, no graphics, inside tmux — graceful fallback |
| Exit cleanliness | Mandatory for session-lifecycle cases | Raw mode restored, cursor visible, no stray escape bytes after exit |
| Performance budget | Optional | Token-flood streaming stays smooth; CPU sane under idle spinner |
| Accessibility | N/A (terminal) | Covered by keyboard-only operation, which is inherent |
| Security | Optional | No secrets echoed in UI or logs (login flows) |

## Selector and Assertion Rules

### Allowed selectors / probes

- Visible pane text: `tmux capture-pane -p` output contains `"Working"` on the last 3 lines
- Cursor position: `tmux display -p '#{cursor_x},#{cursor_y}'`
- Raw byte captures grepped for protocol prefixes (`\x1b_G`, `\x1b]1337;File=`, `\x1b[?2026h`)
- Process exit code; `stty -a < /dev/tty` state after exit (in the probe harness)
- Files the app writes under `HAND_HOME` (sessions, auth.json)
- `TestBackend` buffer contents in `cargo test`

### Forbidden selectors

- Function names, module paths, source file paths — implementation detail
- Exact SGR byte sequences for *styling* asserts (`\x1b[38;5;114m…`) — theme-dependent,
  brittle; assert visible text, and styling only coarsely via `capture-pane -e` when the
  contract explicitly demands it
- Spinner frame glyphs at a specific instant — animation timing is not deterministic
- Row/column magic numbers tied to incidental layout (assert relative order/containment
  of text, not absolute coordinates, unless the case is *about* positioning)

### Allowed assertions

- Binary: PASS or FAIL, no "looks good"
- Specific: expected text/value stated, not a loose regex over noise
- Independent: one probe per assertion

## State Isolation

- **HOME + HAND_HOME redirect:** every TUI case runs with BOTH `HOME=$(mktemp -d)`
  and `HAND_HOME` pointing into it. Rationale: auth storage (`~/.hand/agent/auth.json`)
  and the global settings file resolve via `$HOME`, not `HAND_HOME` — redirecting only
  `HAND_HOME` would make probes write into the developer's real `~/.hand`. Never touch
  the real home. Copy fixtures into the temp home before launch.
- **Fresh tmux session per case:** named after the case id; killed in teardown even
  on failure (`trap`).
- **No cross-case state:** cases run in any order, including alone.
- **No real model calls:** interactive probes exercise UI chrome (editor, overlays,
  slash commands, replay). Flows that need model output use example binaries with
  scripted/fake streams, or `--features model/faux` test seams. Never require an
  API key in a probe.

## Surface Cost Tiers

| Tier | Cost | Isolation strategy | Surfaces (this project) |
|---|---|---|---|
| **cheap** | sub-second, hermetic | one case per probe | `cargo test` / `TestBackend`; raw byte-capture protocol probes; example binaries in `--headless`-style asserts |
| **medium** | seconds; PTY session | one tmux session per case; batch read-only asserts within one session when they don't mutate state | tmux-driven `hand` and demo/gallery examples |
| **expensive** | manual / real terminal | milestone boundaries only; scripted checklist for a human or a real kitty/iTerm2 run | visual image rendering, real kitty keyboard behaviour, scrollback feel |

Default when unsure: **medium**.

## Personas

Personas here are terminal-environment identities (single-user local CLI — identity
is the environment, not credentials):

- `plain_term_user` — TERM=xterm-256color, no kitty keyboard, no graphics; the
  lowest common denominator (also what tmux probing approximates).
- `kitty_term_user` — full kitty graphics + keyboard enhancement support; forced
  via capability env vars for emission probes; real-terminal for expensive tier.
- `tmux_user` — runs `hand` inside tmux; degraded capabilities must fall back
  gracefully (no garbage bytes on screen).
- `configured_user` — has `keybindings.yaml` and a custom theme JSON under
  `HAND_HOME`; expects overrides to keep working unchanged after migration.

## Fixtures and Test Data

**Location:** `tests/fixtures/tui/` (create on first use).

- `themes/<name>.json` — a valid custom theme in today's user format; plus a
  malformed variant for fallback cases
- `keybindings.yaml` — user keybinding overrides in today's format; plus a variant
  with unknown actions / bad chords / conflicts
- `settings-corrupt.yaml` — syntactically invalid settings for tolerance cases
- `images/` — sample PNG, large PNG, one each of jpeg/gif/webp, and a corrupt file
- `sessions/<scenario>.jsonl` — pre-recorded session files for resume/replay cases
- `mock-provider/` — a minimal local HTTP server script serving canned SSE streams in
  the provider wire format, plus a models config pointing `hand` at it; this is how
  streaming/loader/footer-usage probes run without an API key

**Rule:** fixtures are static data; they import no code. Copy into `HAND_HOME`
per case.

## Artifacts

**Location:** `tests/runs/<timestamp>/<case-id>/`

**Each FAIL must produce:**

- `report.md` — failed assertion + expected vs observed
- `repro.sh` — runnable script that reproduces the probe in isolation
- `capture.txt` — final `tmux capture-pane -p` output (and `capture.raw` for byte probes)

**Retention:** keep the last 10 runs.

## Anti-Patterns

### Asserting raw styling bytes

**Looks like:** expecting `\x1b[38;2;95;175;255m` in captured output.
**Why wrong:** themes and terminals change SGR encodings; the user-visible truth is
the text and coarse attributes.
**Do instead:** assert visible text via `capture-pane -p`; use `-e` only when the
case is explicitly about styling, and match attribute presence, not exact bytes.

### Fixed sleeps

**Looks like:** `sleep 5` then capture.
**Why wrong:** flaky on slow machines, wasteful on fast ones; hides missed-render bugs.
**Do instead:** poll `capture-pane` every 200ms up to 25 tries for the ready/expected
text; report the polling log on failure.

### Touching the real user home

**Looks like:** launching `hand` without `HAND_HOME`, reading/writing `~/.hand`.
**Why wrong:** destroys the developer's real sessions/auth; makes cases
order-dependent.
**Do instead:** always `HAND_HOME=$(mktemp -d)` + fixtures.

### Probing animation frames

**Looks like:** asserting the spinner shows `⠋` at t=300ms.
**Why wrong:** frame timing is scheduler-dependent; inherently flaky.
**Do instead:** assert the loader's static text ("Working…") appears, and that it
disappears after cancel/completion.

### Requiring a real model

**Looks like:** a probe that sends a chat message and waits for a real LLM reply.
**Why wrong:** needs API keys, is nondeterministic, and costs money.
**Do instead:** exercise UI chrome; use example binaries with scripted streams for
streaming-display cases.

### Tool-loop exhaustion

**Looks like:** retrying one flaky probe 50 times and reporting timeout.
**Why wrong:** no verdict is ever made.
**Do instead:** max 3 retries with explicit waits; then INCONCLUSIVE + attempt log.

## Knowledge Persistence

**Who writes here:** the runtime validator, after a run, recording facts that outlive
a single run. Authoring-time conventions stay in the sections above.

**Format:** one fact per entry.

```
- [YYYY-MM-DD] <surface / step>: <fact discovered>. <what to do next time>.
```

- [2026-07-23] tmux resize-reflow vs scrollback-leak: `tmux capture-pane -S -` (with history) captures tmux's OWN resize-reflow — on `resize-window`, tmux commits the pane's overflow/old-width rows to its own scrollback, for ANY pane content (proven with a pure-shell control; ratatui-core inline.rs also notes the quirk; cf. codex#11847 zellij/tmux). This is a terminal-multiplexer behavior the app cannot erase. To assert an inline-TUI app does NOT leak the live region into scrollback on resize (VAL-CORE-010/026 class), probe on a RAW PTY (`pty.fork` + `TIOCSWINSZ` + SIGWINCH, answer `\x1b[6n` with `\x1b[1;1R`) or via ratatui `TestBackend` — never `capture-pane -S -`. Under tmux, only assert the VISIBLE region is clean after resize (`capture-pane -p`, no `-S`). The rt runtime's resize handling is verified clean on raw PTY + TestBackend for both base-view and overlay.
- [2026-07-23] tmux 3.6a styled capture broken: `capture-pane -e` (SGR-preserving) returns 0 bytes for everything on this host, incl. a trivial colored `printf` control — a tmux/host defect, not a system defect. For styling assertions (VAL-CORE-034 class), capture raw bytes via `script -q` (run inside a tmux pane so the demo's genuine PTY output is logged, bypassing capture-pane) and inspect SGR sequences directly (e.g. assert a `\x1b[0m`/`39m`/`49m` reset precedes following content's cells = no colour bleed).
- [2026-07-23] rt_demo blocks on startup DSR: the demo emits `\x1b[6n` (cursor-position DSR) at startup and blocks until answered. A dumb PTY (`script -q`) never answers → only ~17 bytes emitted. For full-render probes use `pty.fork` and reply `\x1b[1;1R` to `\x1b[6n`. Protocol-prefix assertions (paste/kitty/mouse/sync) that fire before or at exit still work under `script`.
- [2026-07-23] disk pressure during probing: the shared Data volume can hit 100% under many concurrent tmux servers (each holds large scrollback). Set `TMPDIR=/tmp`, bound tmux `history-limit`, and `trap '... kill-server; rm -rf $HOME' EXIT` per case. `rm -rf target/debug/incremental` frees ~16G safely (does not touch the built binary/tests).
