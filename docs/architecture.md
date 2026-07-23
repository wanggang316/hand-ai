# Architecture — TUI (ratatui migration)

Durable architectural invariants of the new terminal-UI stack (`crates/tui/src/rt/`).
Written at the M1 milestone. This is the Library reference for anyone building on the rt
stack — read it before touching rendering, the event loop, or the viewport. Code is the
source of truth for *what exists*; this records the *why* and the invariants that are
easy to violate and expensive to rediscover.

## Model

The rt stack is an **inline** TUI on ratatui 0.30 + crossterm 0.29: finished content
scrolls into the terminal's native scrollback, a fixed-height viewport at the bottom holds
the live UI (input, loader, overlays). It is NOT a full-screen alternate-screen app — the
shell transcript above the viewport stays visible, and native scrollback/selection work.

Module map (`crates/tui/src/rt/`):
- `session.rs` — terminal lifecycle: raw mode, inline `Terminal` init, kitty keyboard
  flags, bracketed paste, `SessionGuard` (RAII + panic hook, idempotent restore),
  `FallbackSizeBackend` (0×0 PTY → 80×24), SIGHUP listener, viewport erase primitive.
- `events.rs` — crossterm `EventStream` → unified `RtInputEvent` (Key/Paste/Resize);
  `KeyEventKind::Press` filtering; `key_event_to_key_id` (byte-equal to legacy
  `keys::parse_key_id` canonical strings — modifier order shift, ctrl, alt, super).
- `scheduler.rs` — codex FrameRequester pattern: coalesced redraw requests, ≤70/s frame
  cap, BSU/ESU (`?2026h`/`?2026l`) synchronized-output wrapping, idle silence.
- `history.rs` — `HistorySink` over `Terminal::insert_before` + `scrolling-regions`;
  pure width-aware pre-wrap (`wrap_lines`, grapheme/CJK/ZWJ/regional-indicator correct).
- `view.rs` — geometry (`bottom_area_geometry`, `clamp_input_rows`, `TerminalSize`,
  `BottomGeometry::offset_y`); the `RtComponent` trait + `FocusView` (exclusive key
  routing, cursor-follows-focus).
- `overlay.rs` — `OverlayStack`: 9-anchor clamped placement, LIFO modal capture,
  non-capturing passthrough, DIM background, cross-task mpsc mount channel.

## Load-bearing invariants (violate these and rendering breaks)

1. **The scheduler owns the terminal.** `insert_before` (history commit) and
   `terminal.draw` (viewport repaint) both run on the single terminal-owning task, with
   commits drained *before* the draw. Never `insert_before` from another task.

2. **Pre-wrap before committing, and autoresize before reading the wrap width.**
   `insert_before` takes a fixed `u16` height and does not wrap (ratatui#1365) — the caller
   must wrap to the current width and pass the exact wrapped row count. `HistorySink`
   calls `terminal.autoresize()` *before* reading the width, because an inline viewport
   resizes lazily (only on draw/autoresize); a commit landing between a resize event and
   the next draw would otherwise wrap to the stale width and clip in scrollback.

3. **Fixed-max-height inline viewport (ratatui#984 strategy B).** The viewport is
   reserved once at its tallest (`MAX_VIEWPORT_ROWS` = 11) and the bottom area is laid out
   *inside* it, bottom-anchored. A grow never enlarges the viewport (history is never
   eaten); a shrink repaints freed rows blank (no ghost). Do NOT rebuild the `Terminal`
   on resize (strategy A) — rebuild does not clear old viewport rows (scrollback leak) and
   entangles with `insert_before` ordering.

4. **Draw bottom geometry at the viewport's ACTUAL origin, not row 0.** `insert_before`
   slides the inline viewport origin *down* from y=0 as scrollback fills. Apply
   `BottomGeometry::offset_y(frame.area().y)` so the bottom UI follows the viewport;
   painting viewport-local geometry at absolute row 0 makes the bottom UI vanish
   (was VAL-CORE-033).

5. **Erase the viewport region on exit and before resize.** ratatui's `Terminal::Drop`
   only shows the cursor; the restore sequence must additionally erase the viewport rows
   or the bottom-UI box is left as a ghost (was VAL-CORE-016/036). On resize, erase the
   old-width viewport before ratatui's `compute_inline_size`/`append_lines` runs, or old
   content leaks into scrollback (was VAL-CORE-010). Both use the same
   `clear_viewport_region` primitive in `session.rs`.

6. **Restore the terminal on every exit path.** Normal quit, Ctrl+D EOF (event pump drops
   its sender → run loop exits → `SessionGuard::Drop`), panic (chained hook), and SIGHUP
   (tokio signal listener → same clean-exit path as Ctrl+D — a bare SIGHUP would kill the
   process before Drop runs). Restore is idempotent (`restore_once` arbiter). Never leave
   the shell in raw mode, with pushed kitty flags, or with bracketed paste armed.

7. **Never enable mouse capture.** It destroys native selection and wheel scrollback —
   the core inline-mode UX. No `?1000h`/`?1002h`/`?1003h`/`?1006h`, ever.

8. **Modal capture is LIFO and blocks even on ignore.** The topmost capturing overlay
   owns input; a capturing overlay that *ignores* an event still blocks lower layers
   (listeners, focused component). Non-capturing overlays render above but pass keys
   through to the focused component.

## Terminal-multiplexer limitation (not a bug we can fix)

Under tmux (and zellij), `resize-window` reflows the pane's overflow/old-width rows into
tmux's OWN scrollback — for any pane content, regardless of the app (cf. codex#11847;
ratatui-core's `inline.rs` notes the quirk). The rt runtime cannot erase what the
multiplexer already committed. Real terminals (kitty/iTerm2/Terminal.app) do not reflow
the primary screen this way. Consequence for testing: assert resize/scrollback cleanliness
on a **raw PTY** or **`TestBackend`**, never `tmux capture-pane -S -` — see
`docs/user-test-patterns.md` Knowledge Persistence.

## Testing note

The rt layer's rendering correctness must be pinned with `TestBackend` scrollback +
viewport-non-drift assertions, not just pure-function unit tests. Pure geometry/wrap/clock
tests pass while real terminal-integration defects (viewport de-anchor, scrollback leak,
exit ghost) slip through — that is exactly how the M1 stage-3 defects escaped unit review
until runtime probing caught them.
