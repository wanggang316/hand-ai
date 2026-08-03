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
- `events.rs` — bounded-poll input pump (blocking `poll(50ms)`/`read` loop on a
  `spawn_blocking` thread, `AtomicBool` shutdown) → unified `RtInputEvent`
  (Key/Paste/Resize); `KeyEventKind::Press` filtering; `key_event_to_key_id`
  (byte-equal to legacy `keys::parse_key_id` canonical strings — modifier order
  shift, ctrl, alt, super). Never crossterm's async `EventStream`: its reader
  parks in the global event lock without a timeout, stranding the resize path's
  cursor-position replies until the next keypress (multi-second stalls, stuck
  width).
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

## Upstream inline shrink wipe (known residual, ratatui 0.30.2)

On a horizontal **shrink**, ratatui 0.30.2's inline-viewport resize recompute resets the
viewport target to the screen top (`next_area.y = 0`) and issues a full-screen clear
(`clear_region(ClearType::All)`) before re-anchoring — blanking the visible region. This is
upstream ratatui behaviour, not an rt defect, and it is deliberately not worked around. With
the bounded-poll pump keeping resize cursor queries answerable, a **widen** re-lays-out
cleanly at the new width; a **narrow** still blanks the visible screen. The transcript
already committed to native scrollback above survives (only the visible region is wiped;
Terminal.app's ED 2 erase does not push the wiped rows into scrollback).

## Testing note

The rt layer's rendering correctness must be pinned with `TestBackend` scrollback +
viewport-non-drift assertions, not just pure-function unit tests. Pure geometry/wrap/clock
tests pass while real terminal-integration defects (viewport de-anchor, scrollback leak,
exit ghost) slip through — that is exactly how the M1 stage-3 defects escaped unit review
until runtime probing caught them.

## Raw graphics-emission channel (added at M2; the mechanism M3 image display reuses)

Graphics protocols put a *picture* on screen with escape sequences the terminal interprets
out of band (Kitty `\x1b_G…\x1b\\` APC, iTerm2 `\x1b]1337;…` OSC), not cell content — they
cannot be stored in a ratatui `Buffer` cell or diffed like text. M2 introduced a channel
that lets graphics bypass the `Buffer` **without any widget touching the terminal**, keeping
invariant #1 (the scheduler owns the terminal). See `crates/tui/src/rt/components/image.rs`.

- **`RawEmissionQueue`** (`Arc<Mutex<Vec<PendingEmission>>>`, cloned to every image widget
  and the draw task). A graphics-mode `RtImage::render` reserves N blank rows in the buffer
  (so the frame diff paints/clears the footprint) **and** pushes a `PendingEmission { escape,
  row (viewport-local), rows }`. After `terminal.draw` and **inside** the BSU/ESU sync block,
  the draw-owning task calls `flush_to(out, viewport_origin_y)`: sort by row, save cursor
  (`\x1b7`), CUP to `viewport_origin_y + row` (`viewport_origin_y = frame.area().y`, per
  invariant #4) and write each escape, then restore cursor (`\x1b8`). The save/restore keeps
  the raw write from disturbing the caret ratatui positioned.
- **`ScrollbackImageChannel`** (`Arc<Mutex<{ids: HashMap<content_key,u32>, committed:
  HashSet<u32>}>>`). `image_id(bytes)` hashes content and returns a **stable** Kitty id
  (allocated once, reused for identical bytes) so transmission stays **bounded and
  frame-independent** — a repaint of an already-committed scrollback image transmits nothing
  (id reuse is legal; the contract is bounded, not exactly-once). `mark_committed(id)`
  protects a scrolled-into-history image. **The type has no method that can mint a wide
  `d=A`/`d=a` delete** — the only delete it produces is a single-id `d=I` via
  `delete_viewport_image(id)`, and only for an *uncommitted* id (a committed id yields
  `None`). That structural absence is the safety pin: a wide delete would wipe every image
  including the scrollback ones. `terminal_image::delete_all_kitty_images` (the `d=A` form)
  exists in the legacy C-class module but the rt channel never calls it.
- **Decode-validation gates every graphics persona.** `RtImage::encode` runs
  `decodes(&data)` (`image::load_from_memory`) first and degrades to the bordered placeholder
  box — on Kitty **and** iTerm2 — for any source the decoder rejects, so an undecodable/
  truncated blob never reaches the wire as a half graphics escape (the migration fix: iTerm2
  is no longer exempt). Kitty transmits PNG (`f=100`), so a non-PNG source is transcoded via
  `transcode_to_png`; iTerm2 carries the source bytes native.
- **OSC 8 hyperlinks ride the same channel.** A markdown link's URL cannot travel through a
  `Buffer` cell, so the renderer paints the visible styled span and, on a capable terminal,
  the host flushes `osc8_emission(text, url, row)` through the raw channel; on tmux/incapable
  terminals `links` is empty and the pinned `text (url)` in-cell fallback is painted.

`image` 0.25 (`default-features = false`, `features = [jpeg, gif, webp, png]`) was added to
`crates/tui` solely for the Kitty non-PNG→PNG transcode. It was already in the workspace lock
via `arboard`, so `Cargo.lock` did not grow.

Known deferred (M3 follow-on, tracked in the M2 handoffs, not defects): the scrollback commit
writes its escape at the viewport region rather than the exact reserved history row
(`Terminal::viewport_area` is private — no public accessor for the post-`insert_before`
absolute row), and a live `CSI 16 t` cell-size reply is not yet routed back through the typed
event pump (the query is proven non-blocking; the reply→row-scaling path is unit-tested).

## Interactive driver concurrency (added at M3; `crates/coding-agent/.../rt_driver`)

The `hand` interactive driver runs on the rt stack as of M3 (the strangler cutover — the
5071-line legacy `driver.rs` is deleted). Its run loop is split into **independent tokio
tasks** communicating through channels and one `Arc<Mutex<DriverState>>`:

1. **`turn_runner`** drains the submit channel and runs each `send_message` (or `/`-command,
   `!`-bash, selector) **to completion** before pulling the next — this is the FIFO queue the
   "messages submitted during a turn process in order" requirement rides on.
2. **`event_applier`** drains `AgentSessionEvent`s and commits scrollback lines. It is a
   **synchronous** apply over an async `recv()` loop.
3. The **scheduler** owns the terminal (invariant #1). The other tasks only mutate shared
   state and `request_frame()`.

Load-bearing rules for this layer:

- **Never multiplex the turn future and its own event stream in one `select!`.** `send_message`
  emits events through the same channel it is driven from; a shared `select!` cancels the
  in-flight turn the instant its first event arrives (the turn never completes, no HTTP call
  lands). The two-task split exists specifically to avoid this — it was a real bug found and
  fixed live during the skeleton.
- **A `std::sync::Mutex` guard is never held across `.await`.** Every `lock_state`/`lock_editor`/
  `lock_footer` guard is scoped in an explicit `{ … }` block (or a single expression) that drops
  **before** any `.await`. `apply_event` is deliberately `fn`, not `async fn`, so no event-apply
  guard can straddle a suspension point.
- **Lock poisoning is fatal-by-design for render state.** `lock_state`/`lock_editor`/`lock_footer`
  `.expect(...)` on poison: a poisoned lock means a panic already tore through the driver, and the
  terminal-restoring panic hook has already fired — continuing would paint garbage. The shared
  keybindings handle and the per-turn `CancellationToken` are the exceptions: they use
  `unwrap_or_else(|e| e.into_inner())` / `if let Ok(token)` because a poisoned turn there just
  means the turn already tore down, so recovering the inner value is harmless.
- **Single teardown funnel — no `process::exit`, no raw-pointer `StopHandle`.** Every exit route
  (Ctrl+D incl. over a non-empty buffer, `/quit`·`/exit`·`/q`, or a mid-stream quit) returns from
  the input loop to one teardown block: drop the submit channel, `abort()` the turn/applier/
  version tasks (a stalled mid-stream turn is abandoned, not awaited), drop the requester so the
  scheduler drains its final frame + closes the sync block, `abort()` the input pump, then
  `guard.restore()` (idempotent with `Drop`). This is what makes a mid-stream quit exit in
  bounded time with the terminal cooked.
- **OSC 133 / OSC 9;4 prompt+progress marks ride the raw-escape channel.** `DriverState.pending_raw`
  (queue via `queue_raw`) is drained by `flush_raw` **after** `terminal.draw` but **inside** the
  BSU/ESU sync block, bracketed by `ESC7`/`ESC8` cursor save/restore — the same discipline as the
  M2 image channel, except these are terminal-global escapes (no row address). This keeps
  invariant #1 intact. `/clear` reuses the channel for its `ESC[3J ESC[2J ESC[H` scrollback wipe
  (the rt stack has no native scrollback-clear API yet).
- **Overlays cross the task boundary as a `Send` `SelectorController`, not the M1 stack.** The M1
  `OverlayStack` is `?Send`; a mounted selector lives behind its own `Arc<Mutex<dyn
  SelectorController + Send>>` and the scheduler snapshots its `render_lines(width)` `Vec<Line>`
  each frame, painting the dialog via the public `anchor_rect` geometry. The M1 `?Send` contract
  is untouched.
- **Two crates/tui seams only (strangler invariant).** M3 added exactly two things to
  `crates/tui/src/rt/session.rs`: a single-session guard (`claim_session` `compare_exchange` +
  panic-hook-once `Once`, so repeated enter/drop cycles don't stack panic hooks) and
  `SessionGuard::suspend()`/`resume()` for the Ctrl+G external-editor handoff (they pop/re-push
  the interactive escapes only, deliberately not touching `SESSION_ACTIVE` or the panic hook).

## Theme application (M3)

A custom `~/.hand/themes/*.json` recolours the rt UI via a per-frame `ThemePalette` snapshot
(`DriverState::palette()`), threaded as `&ThemePalette` into the pure render functions across
~24 modules. `ThemePalette::from_theme` keys off `Theme::source_path()`: a **built-in**
`dark`/`light` (no source path) yields the historical hard-coded palette **byte-for-byte**, so
the default look is unchanged; only a file-loaded (`source_path().is_some()`) theme recolours,
and a custom slot resolving to `Color::Reset` falls back to the historical constant so an empty
slot stays readable. Keybindings are unified on the durable app-layer `core::keybindings`
(Decision Log 2026-07-24, option A); the legacy `hand_tui::keybindings` registry has no
production consumer and is an M4 retirement target.
