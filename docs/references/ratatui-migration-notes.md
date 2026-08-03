# ratatui ecosystem notes (researched 2026-07)

Durable findings from the TUI → ratatui migration research. Sources verified against
official docs/repos at research time; re-verify version numbers before acting on them.

## Version matrix

| Crate | Version | Notes |
|---|---|---|
| ratatui | 0.30.2 (2026-06) | MSRV 1.88, Rust 2024 edition. 0.30 split into `ratatui-core`/`ratatui-widgets`/`ratatui-crossterm` etc.; `Alignment` → `HorizontalAlignment`; custom `Backend` needs associated `Error` + `clear_region`. Enable `scrolling-regions` feature for flicker-free `insert_before`. |
| crossterm | 0.29.0 (2025-04) | ratatui 0.30 default; selectable via `crossterm_0_28`/`crossterm_0_29` features. Never mix 0.27/0.28 in one dep tree. |
| ansi-to-tui | 8.x | Active; aligned with ratatui 0.30. Bridge for ANSI-emitting highlighters. |
| ratatui-image | 11.x | Active, in ratatui org. Kitty/iTerm2/Sixel/halfblocks; viewport-only — does NOT manage images in scrollback. |
| tui-textarea | 0.7.0 (2024-10) | Effectively unmaintained; pinned to ratatui 0.29/crossterm 0.28. Do not adopt. |
| tui-markdown | 0.3.x | Active but narrow; community norm for agent CLIs is a custom markdown → `Vec<Line>` renderer (needed anyway to control wrap width for history insertion). |

## Hard constraints (open upstream issues)

- **Inline viewport height cannot change at runtime** — ratatui#984 open, PR#1964 unmerged.
  Workarounds: recreate `Terminal` on height change, fix viewport at max height, or a
  codex-style custom terminal.
- **`insert_before` requires pre-wrapped content** — height is a fixed `u16` argument;
  no built-in wrapping (ratatui#1365). Caller must wrap to width first.
- Without `scrolling-regions`, `insert_before` clears the viewport and forces a full
  redraw next frame (flicker; ratatui#584). With the feature it uses terminal scroll
  regions (`scroll_region_up/down`).
- **Synchronized output (BSU/ESU) is not automatic** — wrap `terminal.draw()` in
  crossterm `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` yourself.
- ratatui does not touch input: push/pop crossterm `KeyboardEnhancementFlags` yourself
  (probe with `supports_keyboard_enhancement()`, pop on exit AND panic paths, filter
  `KeyEventKind::Release/Repeat` or keys fire twice on kitty-protocol terminals).
- ratatui `Buffer` is a cell grid — arbitrary escape sequences (terminal images,
  OSC 133) cannot pass through `draw()`/`insert_before`; they need a raw write channel
  to the backend.

## Production precedent: OpenAI codex-rs

The closest production-proven "Claude Code-style inline chat TUI" on ratatui
(ratatui 0.29 + crossterm 0.28, both forked). Patterns worth mirroring:

- `insert_history.rs` — finalized chat history is written to native scrollback via raw
  ANSI (scroll region + reverse index), not via stock `insert_before`.
- `FrameRequester`/`FrameScheduler` — tasks request redraws; scheduler coalesces
  requests and rate-limits; token streams never draw directly.
- `BottomPane` view stack — overlays/modals are layered views inside the viewport,
  not terminal-level floats.
- Their bug history to learn from: first-frame blank-cell diff skips leaving stale
  content (codex#21450), scrollback truncation under zellij/tmux (codex#11847).

## Streaming refresh consensus

Immediate mode, one `draw()` per event-loop turn; coalesce high-frequency updates and
cap around 60fps; use synchronized output to kill flicker. Do not draw per token.

## M1 empirical resolutions (verified 2026-07 against the rt stack)

Two of the "hard constraints" above were resolved with a concrete choice during the
M1 core-runtime build; recording the outcome so later milestones do not re-litigate them.

- **Inline viewport height (ratatui#984) → fixed-max-height viewport (strategy B).**
  The inline viewport is reserved once at its tallest (`MAX_VIEWPORT_ROWS` = border 2 +
  loader 1 + input 8 = 11 rows) and the bottom area is laid out *inside* it, bottom-anchored;
  a grow never enlarges the viewport (history is never eaten) and a shrink repaints the
  freed rows blank (no ghost). Rebuild-on-resize (strategy A) was rejected because rebuilding
  does not clear the old viewport rows (scrollback leak) and entangles with the scheduler's
  `insert_before` ordering. Pure geometry core: `crates/tui/src/rt/view.rs`
  (`bottom_area_geometry` / `clamp_input_rows`).
- **`insert_before` sufficiency → stock ratatui `insert_before` + `scrolling-regions` is
  enough for M1;** no codex-style raw scroll-region/reverse-index insertion was needed.
  It works because the caller (`HistorySink`) does the width-aware pre-wrap ratatui#1365
  requires and passes the exact wrapped row count as the fixed `height`, so committed height
  and painted rows stay in lock-step. Implementation: `crates/tui/src/rt/history.rs`
  (stateless `HistorySink`, pure `wrap_lines`). The codex-style raw path remains a documented
  fallback if M2 image-in-scrollback or M3 real load exposes a gap.
- **Post-resize commit width correctness.** `HistorySink::commit_lines` calls
  `terminal.autoresize()` *before* reading the wrap width, because an inline viewport resizes
  lazily (only on draw/autoresize); without this a block committed between a resize event and
  the next draw would pre-wrap to the stale width and land clipped. `autoresize()` is a no-op
  at steady state. Any future history-commit path must preserve this autoresize-before-wrap ordering.
- **Scheduler owns the terminal.** `insert_before` (history commit) and `terminal.draw`
  (viewport repaint) must both run on the single terminal-owning task, with commits drained
  *before* the draw. See the rt_demo scheduler closure for the reference wiring
  (`crates/tui/examples/rt_demo.rs`, `spawn_scheduler`).
