# ExecPlan: Port pi-tui (TypeScript) to hand-tui (Rust) — Full API & Behavior Parity

**Status:** Draft
**Author:** Gump (planning assisted by Claude)
**Date:** 2026-05-06

This is a living document. The Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections must be kept up to date as work proceeds.

## Purpose

After this plan completes, `hand-coding-agent` (and any future Rust consumer) can build a fully interactive terminal UI on top of `hand-tui` with the same capabilities `pi-coding-agent` gets from `pi-tui` today: a multi-line editor with grapheme-aware word-wrap, slash-command and file-attachment autocomplete, paste markers, undo/redo, kill-ring, markdown rendering with tables and links, image preview via Kitty/iTerm2 protocols, overlay dialogs that don't leak styling, robust Kitty keyboard protocol parsing, and a render loop that survives terminal resize, paste storms, and SIGWINCH.

A user running the future `hand` CLI in a terminal will be able to:
- Type a multi-line prompt with proper CJK / emoji / ZWJ handling and have it word-wrap correctly inside the editor viewport.
- Trigger `/` slash-command autocomplete and `@` file autocomplete with debounced async lookups.
- Paste a 100-line block and see a single `[paste #1 +99 lines]` marker rather than a flood of literal newlines.
- Open an overlay (settings / model picker) on top of the agent transcript without the underlying ANSI styles bleeding through after dismiss.
- Resize the terminal mid-stream and see the UI re-flow without artifacts.
- Send `Ctrl+C` once to cancel, twice to quit, with consistent semantics across kitty/xterm/Windows Terminal/Termux.

The proof that this works is `cargo test -p hand-tui` with parity-level coverage (≥ 28 test files mirroring `pi-mono/packages/tui/test/`), plus a runnable example in `examples/` that exercises the full stack end-to-end.

## Progress

- [ ] M1.T1 — Port `utils.ts` (1140 lines) to `utils.rs` with grapheme + ANSI awareness
- [ ] M1.T2 — Port `keys.ts` (1400 lines) — Kitty protocol, modifyOtherKeys, KeyId, matchesKey, decodeKittyPrintable
- [ ] M1.T3 — Create `stdin_buffer.rs` from `stdin-buffer.ts` (411 lines)
- [ ] M1.T4 — Create `keybindings.rs` from `keybindings.ts` (244 lines)
- [ ] M2.T1 — Rebuild `Component` trait with hide/focus/setHidden/isFocused; add `InputEvent`
- [ ] M2.T2 — Rewrite `tui.rs`: async `run()`, input dispatch, debounced `request_render`, focus manager
- [ ] M2.T3 — Integrate `Overlay` into `Tui`: `show_overlay/hide_overlay/has_overlay`, anchors, margins, style isolation
- [ ] M2.T4 — Resize handling (`SIGWINCH` via tokio signals or crossterm event), `clearOnShrink`, viewport tracking
- [ ] M2.T5 — `ProcessTerminal` enters raw mode, reads stdin into `StdinBuffer`, restores on drop
- [ ] M3.T1 — Port `terminal-image.ts` to `terminal_image.rs`: Kitty + iTerm2 + fallback, capability detection, cell dimensions
- [ ] M3.T2 — Complete `editor.rs`: word-wrap with grapheme segmentation, paste markers, IME composition, autocomplete hooks
- [ ] M3.T3 — Complete `markdown.rs`: tables, links, theming, list nesting, blockquote
- [ ] M3.T4 — Complete `autocomplete.rs`: `AutocompleteProvider` trait, `CombinedAutocompleteProvider`, `SlashCommand`, debounce
- [ ] M3.T5 — Audit remaining components (`input`, `select_list`, `loader`, `box`, `text`, `truncated_text`, `image`) for parity gaps
- [ ] M4.T1 — Add `tests/` directory; port 28 TS test cases as Rust integration tests
- [ ] M4.T2 — Port named regression cases (overlay-style-leak, regional-indicator-width, isimageline-startswith, viewport-overwrite, truncate-to-width, wrap-ansi)
- [ ] M4.T3 — Add `examples/tui-demo` exercising editor + overlay + autocomplete + image
- [ ] M5.T1 — Wire `hand-coding-agent` to actually use `hand-tui` (smoke test that the API is usable)
- [ ] M5.T2 — `cargo clippy --workspace -- -D warnings` clean; document migration notes for existing 192 inline tests

## Surprises & Discoveries

(None yet — to be populated as M1 proceeds.)

## Decision Log

**D-001 — No `ratatui` dependency.** The user (Gump) explicitly required preserving pi-tui's self-rendered diff engine and Component model. `ratatui` would force a Buffer-based mental model and lose differential-line rendering. Confirmed in conversion-guidelines.md context.

**D-002 — `unicode-segmentation` for grapheme clusters.** TS uses `Intl.Segmenter` (host JS engine). The closest Rust equivalent for grapheme cluster iteration is the `unicode-segmentation` crate (used by ripgrep, helix). `unicode-width` (already a dep) handles display width but not segmentation, so we need both.

**D-003 — Async runtime: tokio (already declared).** `Tui::run` will be `async fn`. Stdin reading uses `tokio::io::AsyncReadExt` on a duplicated stdin file descriptor; raw-mode toggling stays on `crossterm`. SIGWINCH on Unix uses `tokio::signal::unix::signal(SignalKind::window_change())`; on Windows we poll `crossterm::event::poll` with a short timeout. This decision is testable independently — see Validation.

**D-004 — Existing 192 inline tests are kept.** They are not equivalent to the TS test suite, but they cover lower-level invariants (e.g. fuzzy match, kill-ring). M4.T1 adds a `tests/` integration directory rather than rewriting them.

**D-005 — `Component` trait will gain new methods with default impls.** This is a breaking change for any consumer that has implemented `Component` directly. `hand-coding-agent` does not yet `use hand_tui` (verified via grep), so we can break the trait freely. We'll document the new methods in the README's Migration section.

**D-006 — Atomic commit per task.** User's global rule (memory: `feedback_commit_after_change.md`) requires `/commit` after each logical change. Each `T` row in Progress is one commit unit.

## Outcomes & Retrospective

(To be filled at milestone completion.)

## Context and Orientation

Related documents:
- Conversion guidelines (binding rules for TS→Rust style): `docs/conversion-guidelines.md`
- Workspace-level conversion plan (covers all crates, this plan zooms into Stage 2): `docs/conversion-plan.md`
- Workspace manifest: `Cargo.toml`
- TUI crate manifest: `packages/tui/Cargo.toml`
- TUI README (current Rust API surface): `packages/tui/README.md`

Source-of-truth TypeScript implementation:
- `~/dev/opensource/pi-mono/packages/tui/src/` — production code
- `~/dev/opensource/pi-mono/packages/tui/test/` — 28 test files including named regression cases

Key source files (Rust side, current state):
- `packages/tui/src/lib.rs` — module declarations and re-exports
- `packages/tui/src/tui.rs` (242 lines) — minimal `Component`/`Container`/`Tui`. Missing: async `run()`, input dispatch, focus, overlay integration, resize handling.
- `packages/tui/src/terminal.rs` (270 lines) — `Terminal` trait + `ProcessTerminal`. Missing: raw-mode entry/exit, stdin reading, alternate-screen handling.
- `packages/tui/src/render.rs` (213 lines) — `DiffRenderer`. Mostly complete; needs viewport-aware diffing for resize.
- `packages/tui/src/keys.rs` (498 lines vs TS 1400) — basic CSI parsing. Missing: Kitty event types (release/repeat), modifyOtherKeys, `KeyId` system, `matchesKey`, base-layout-key, Windows Terminal quirks.
- `packages/tui/src/utils.rs` (266 lines vs TS 1140) — basic ANSI strip and width. Missing: grapheme segmentation, OSC 8 hyperlink tracking, `wrapTextWithAnsi`, `sliceByColumn/sliceWithWidth`, `applyBackgroundToLine`.
- `packages/tui/src/overlay.rs` — standalone `render_with_overlay` helper, NOT integrated into `Tui`.
- `packages/tui/src/components/editor.rs` (545 lines vs TS 2292) — basic editing. Missing: word-wrap with graphemes, paste markers, autocomplete integration, IME, viewport scrolling.
- `packages/tui/src/components/autocomplete.rs` (264 lines vs TS 783) — only the dropdown view component. Missing: provider system entirely.
- `packages/tui/src/components/markdown.rs` (280 lines vs TS 852) — basic. Missing: tables, links, theme.
- `packages/tui/src/components/image.rs` — image *component* (renders pre-encoded data). NOT the image-encoding layer (which TS has as `terminal-image.ts` and Rust lacks entirely).

**Terms of art used in this plan:**
- *Differential rendering* — comparing the previous frame's lines against the new ones and emitting only the changed lines as ANSI cursor-move + line-rewrite sequences. Lives in `render.rs`.
- *Paste marker* — when a paste contains many bytes/lines, the editor stores the content out-of-band and inserts a placeholder like `[paste #1 +99 lines]`. The TUI substitutes back when sending to the agent. Implemented in `editor.ts` lines covering `PASTE_MARKER_REGEX`.
- *Kitty keyboard protocol* — extended escape sequences (`CSI u`) advertising key release events, repeat, and full modifier disambiguation. Spec: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
- *modifyOtherKeys* — xterm legacy protocol predating Kitty for similar disambiguation. Format `CSI 27;<modifier>;<keycode>~`.
- *Grapheme cluster* — user-perceived character; e.g. 👨‍👩‍👧 is one grapheme but several codepoints. Required for cursor movement and width calculation in CJK/emoji-heavy text.
- *OSC 8 hyperlink* — `OSC 8 ; params ; URI ST` ... `OSC 8 ; ; ST`. State machine must track these across line boundaries when wrapping/truncating.
- *Cell dimensions* — pixel size of a terminal cell, queried via `CSI 16 t` reply or fallback heuristics. Required to compute how many rows an image occupies.
- *Stop-replay* — when stdin delivers half an escape sequence (terminal split a `CSI` across reads), `StdinBuffer` holds the partial bytes until the rest arrives, then emits one complete sequence.

**How the parts fit:** The render path is `Component::render(width) -> Vec<String>` → `DiffRenderer::diff(prev, new) -> Commands` → `Terminal::write(&commands)`. The input path is stdin bytes → `StdinBuffer` reassembles complete sequences → `keys::parse_key` produces a `Key` → `Tui` dispatches to focused component or input listeners. Keybindings sit between `parse_key` and components: `KeybindingsManager::resolve(key) -> Option<Keybinding>` lets components handle semantic actions (`tui.editor.cursorUp`) instead of raw key strings.

## Plan of Work

### Milestone 1: Foundation utilities (no runtime change yet)

After this milestone, the lower-level building blocks for everything else exist with parity to TS. None of these touch the TUI loop, so they can be developed and tested in isolation. The crate keeps building and existing tests keep passing throughout.

**M1.T1 — Port `utils.ts` to `utils.rs`.** Add `unicode-segmentation` to `Cargo.toml`. Replace the simple ANSI helpers with a complete port: `visible_width` (grapheme-aware, using `unicode-segmentation::UnicodeSegmentation::graphemes` × `unicode-width::UnicodeWidthStr::width`), `wrap_text_with_ansi` (preserves SGR + OSC 8 across wrapped lines via an `AnsiCodeTracker` struct), `slice_by_column`, `slice_with_width`, `truncate_to_width` (multi-mode: end/middle/start), `apply_background_to_line`, `normalize_terminal_output`, `extract_ansi_code`, `extract_segments`. Translate the Intl.Segmenter machinery for paste markers using a custom regex iterator that yields `Segment::Text | Segment::PasteMarker { id, summary }`. Acceptance: `cargo test -p hand-tui utils::` passes; specifically port the `wrap-ansi.test.ts` and `truncate-to-width.test.ts` cases.

**M1.T2 — Complete `keys.rs` to match `keys.ts`.** Add `KeyId` (string-like identifier `"ctrl+shift+up"`), `KeyEventType` enum (`Press | Repeat | Release`), `matches_key(data, key_id) -> bool`, `decode_kitty_printable`, `is_key_release`, `is_key_repeat`, `is_kitty_protocol_active` (atomic bool), full Kitty CSI-u parser including the `_;event_type` suffix, modifyOtherKeys parser for legacy xterm, Windows Terminal raw-backspace handling (`isWindowsTerminalSession` checked via `WT_SESSION` env var), shifted-letter normalization. Re-use the existing `Key` struct but expand `KeyName` to cover all functional keys in the TS table. Acceptance: port `keys.test.ts` and `key-tester.ts` cases as integration tests in `tests/keys.rs`.

**M1.T3 — Create `stdin_buffer.rs` from `stdin-buffer.ts`.** New module. Public surface: `pub struct StdinBuffer`, `StdinBufferOptions`, an event channel (`tokio::sync::mpsc::UnboundedReceiver<StdinBufferEvent>`) replacing the JS `EventEmitter`. Implementation: `is_complete_sequence`, `is_complete_csi_sequence`, `is_complete_osc_sequence`, `is_complete_dcs_sequence`, `is_complete_apc_sequence`, `extract_complete_sequences(buffer: &str) -> (Vec<String>, String /* remainder */)`, `parse_unmodified_kitty_printable_codepoint`. Buffer accepts arbitrary `&[u8]` chunks, decodes UTF-8 with replacement (incomplete UTF-8 boundaries treated like incomplete escapes), and emits `StdinBufferEvent::Data(String)` once a complete grapheme/sequence is ready. Acceptance: port `stdin-buffer.test.ts` as `tests/stdin_buffer.rs`.

**M1.T4 — Create `keybindings.rs` from `keybindings.ts`.** Public surface: enum `Keybinding` (one variant per TS string key, e.g. `EditorCursorUp`, `InputSubmit`), `KeybindingDefinition { default_keys: Vec<KeyId>, description: Option<String> }`, `TUI_KEYBINDINGS: phf::Map<&'static str, KeybindingDefinition>` (or a `LazyLock<HashMap>` if `phf` is judged overkill), `KeybindingsManager { config: HashMap<Keybinding, Vec<KeyId>> }` with `set`, `get`, `matches(key_data: &str, binding: Keybinding) -> bool`, `conflicts() -> Vec<KeybindingConflict>`. The conflict-detection algorithm walks the configured map and reports any `KeyId` mapped to more than one `Keybinding`. Acceptance: port `keybindings.test.ts`.

### Milestone 2: TUI runtime (the missing event loop)

After this milestone, you can write `let mut tui = Tui::new(ProcessTerminal::new()?); tui.add_child(component); tui.run().await?;` and it actually loops, reads stdin, dispatches input, handles resize, and renders. This is where the crate transitions from "library of pieces" to "runnable framework". Slice vertically: build the loop end-to-end with a single Text component first, then layer focus, overlays, and resize.

**M2.T1 — Expand `Component` trait.** In `tui.rs`, add to `Component`: `fn hide(&mut self) {}`, `fn set_hidden(&mut self, hidden: bool) {}`, `fn is_hidden(&self) -> bool { false }`. Add to `Focusable`: `fn focus(&mut self)`, `fn unfocus(&mut self)`, `fn is_focused(&self) -> bool`. Update existing Rust components (12 of them) to implement the new methods where meaningful (Input, Editor, SelectList, SettingsList focus; all components hide). Define `pub enum InputEvent { Key(Key), Paste(String), Resize { cols: u16, rows: u16 }, Tick }` so handlers receive structured events instead of raw `&str`.

**M2.T2 — Rewrite `Tui` with an async run loop.** Replace the current `Tui::render()` shell with:
```rust
impl Tui {
    pub async fn run(&mut self) -> Result<(), TuiError>;
    pub fn request_render(&mut self);          // debounced
    pub fn add_input_listener(&mut self, listener: InputListener) -> ListenerId;
    pub fn remove_input_listener(&mut self, id: ListenerId);
    pub fn set_focus(&mut self, target: ComponentId);
    pub fn focus(&self) -> Option<ComponentId>;
    pub fn stop(&mut self);
}
```
Implementation: spawn three concurrent tasks via `tokio::select!` — (1) stdin reader feeding `StdinBuffer`, (2) SIGWINCH/resize watcher, (3) render-tick when `request_render` was called. Render is debounced with a 4 ms timer (TS uses 4 ms via `setTimeout`); a `force=true` request bypasses the debounce. Components are addressed by stable `ComponentId` issued by `Container::add_child` so focus survives child reordering.

**M2.T3 — Integrate Overlay into `Tui`.** Replace the standalone `render_with_overlay` helper. Add to `Tui`: `show_overlay(component, OverlayOptions) -> OverlayHandle`, `hide_overlay()`, `has_overlay() -> bool`. Implement `OverlayOptions { anchor: OverlayAnchor, margin: OverlayMargin, capture_input: bool, dim_background: bool }` with `OverlayAnchor` covering all 9 positions (`TopLeft..BottomRight..Center`) plus relative-to-component anchoring. The render pipeline becomes: `base = root.render(width); overlay = overlay_stack.render(width, base.len()); composed = compose(base, overlay)`. Crucially, after `hide_overlay`, the next render must clear any SGR state the overlay introduced — this is the `tui-overlay-style-leak` regression. Each composed line ends with explicit `\x1b[0m` if the overlay touched it.

**M2.T4 — Resize handling.** On Unix, register `tokio::signal::unix::signal(SignalKind::window_change())` and feed events into the run loop. On Windows, poll `crossterm::event::poll(Duration::from_millis(50))` and watch for `Event::Resize`. Behavior: when terminal width changes → full re-render (bypass diff). When height shrinks below `max_lines_rendered` and `clear_on_shrink` is true → clear and full re-render. Track `previous_width`, `previous_height`, `max_lines_rendered` on `Tui`. Acceptance: `tests/tui_render.rs` exercises a `MockTerminal` with simulated resize.

**M2.T5 — `ProcessTerminal` raw mode + stdin.** Implement `enter_raw_mode()` / `leave_raw_mode()` using `crossterm::terminal::enable_raw_mode`. Add a `Drop` impl that restores the terminal even on panic. Add `pub async fn read_stdin(&mut self, buf: &mut [u8]) -> io::Result<usize>` that uses `tokio::io::stdin()`. Add `enter_alternate_screen` / `leave_alternate_screen` (gated by an option since pi-tui does not always use alt-screen — confirm by reading `tui.ts:start()`).

### Milestone 3: Component depth (parity with the rich features users actually see)

After this milestone, the editor and markdown components produce output indistinguishable from pi-tui for the same inputs, and the autocomplete machinery is consumable by `hand-coding-agent`. This is the largest milestone.

**M3.T1 — Port `terminal-image.ts` to `terminal_image.rs`.** New module. Public surface: `encode_kitty(data: &[u8], opts: &ImageRenderOptions) -> String`, `encode_iterm2(...)`, `image_fallback(...)`, `detect_capabilities() -> TerminalCapabilities`, `get_cell_dimensions() -> Result<CellDimensions, _>`, `calculate_image_rows(image: &ImageDimensions, cell: &CellDimensions, max_rows: u16) -> u16`, `allocate_image_id() -> u32`, format-specific dimension probes (`get_png_dimensions`, `get_jpeg_dimensions`, `get_gif_dimensions`, `get_webp_dimensions` — read magic bytes and IHDR/SOF chunks; no full decode). Use `base64` crate for Kitty/iTerm2 payload encoding. Cell dimension query: write `CSI 14 t`, read reply, fall back to `(8, 16)` on timeout. Wire `components/image.rs` to consume this module rather than expecting pre-encoded data.

**M3.T2 — Complete `editor.rs`.** This is the biggest single task — break into sub-commits if it grows past 5 files. Add: `word_wrap_line(line, max_width, segments)` using grapheme iteration from `unicode-segmentation`, paste-marker storage (`HashMap<u32, PasteContent>` on the editor with regex match/replace on render and submission), IME composition state (`composing: Option<String>` set by Kitty `CSI ? 6 c` markers), viewport scrolling (`viewport_top`, `viewport_height`, `cursor_visible_in_viewport`), kill-ring integration (already exists in `kill_ring.rs`, needs wiring), undo-stack with grouped operations (`UndoEntry { op: Insert | Delete | Replace, position, text, cursor_before, cursor_after }`), slash-command and attachment autocomplete callbacks (`on_autocomplete_request: Option<Box<dyn Fn(&AutocompleteContext) -> BoxFuture<...>>>`). The autocomplete callback fires after a 20 ms debounce (TS `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS`).

**M3.T3 — Complete `markdown.rs`.** Use `pulldown-cmark` (already a dep) for parsing; render to ANSI with: `MarkdownTheme { heading_fg, code_bg, link_fg, ... }`, table rendering with cell-aware alignment (read `Alignment::Left|Center|Right`), link rendering as OSC 8 hyperlinks (with fallback to `[text](url)` when terminal lacks support), inline code with `code_bg` highlighting, list nesting (track depth on stack), blockquote with left-bar prefix, default text style fallback. Code blocks already work — extend with optional language label header. Acceptance: port `markdown.test.ts`.

**M3.T4 — Complete `autocomplete.rs`.** Move provider abstraction from "non-existent" to first-class. New types: `pub trait AutocompleteProvider: Send + Sync { fn query(&self, ctx: &AutocompleteContext) -> BoxFuture<'static, Vec<AutocompleteItem>>; }`, `pub struct CombinedAutocompleteProvider { providers: Vec<Arc<dyn AutocompleteProvider>> }` that fans out queries and merges results, `pub struct SlashCommand { pub name: String, pub description: String, pub arguments: Option<String> }`, `pub struct AutocompleteContext { pub text: String, pub cursor: usize, pub trigger: AutocompleteTrigger }`. The existing dropdown view component stays as the *renderer* of suggestions — providers feed it. Debouncing happens in the editor (M3.T2), not here.

**M3.T5 — Audit remaining components.** Diff each Rust component against its TS counterpart for missing public methods / options. Likely small touchups: `SelectList` layout options (`SelectListLayoutOptions`), `SettingsList` themes, `Loader` indicator options, `Box` border styles, `Image` lifecycle (`hide`/`show`/`reset`). Document any deliberate omissions in a "non-goals" comment in each file.

### Milestone 4: Test parity & regression coverage

After this milestone, regressions caught by the TS suite cannot silently slip into the Rust port. All 28 TS test files have a Rust counterpart in `packages/tui/tests/`.

**M4.T1 — Create `tests/` integration directory and port the easy 22 files.** Files: `autocomplete.rs`, `editor.rs`, `fuzzy.rs`, `image.rs`, `input.rs`, `keybindings.rs`, `keys.rs`, `markdown.rs`, `select_list.rs`, `stdin_buffer.rs`, `terminal_image.rs`, `terminal.rs`, `truncate_to_width.rs`, `truncated_text.rs`, `tui_render.rs`, `wrap_ansi.rs`, plus shared `common/mod.rs` for fixtures (mock terminal, golden-line helpers).

**M4.T2 — Port the 6 named regression files specifically.** These are the bug-prevention nets:
- `regression_regional_indicator_width.rs` — flag emoji (🇯🇵 = 2 RIs) renders as width 2, not 4.
- `bug_regression_isimageline_startswith.rs` — image-line detection must not be fooled by user text starting with the marker prefix.
- `tui_overlay_style_leak.rs` — overlay dismiss must reset SGR state.
- `overlay_non_capturing.rs`, `overlay_options.rs`, `overlay_short_content.rs` — overlay positioning edge cases.
- `viewport_overwrite_repro.rs` — editor viewport repaint on terminal-height change.
- `tui_cell_size_input.rs` — cell-dimension probe doesn't deadlock on terminals that ignore `CSI 16 t`.

**M4.T3 — Add `examples/tui-demo`.** Standalone binary in `examples/src/tui_demo.rs` that wires editor + markdown + autocomplete + overlay + image preview into a small test harness. Lets a human eyeball regressions that automated tests miss. Run via `cargo run --example tui-demo`.

### Milestone 5: Integration & cleanup

**M5.T1 — Wire `hand-coding-agent` to use `hand-tui`.** This is consumer-side work but proves the API. Add a minimal `use hand_tui::*` somewhere in `coding-agent` (likely a placeholder render mode in `src/main.rs`) so the Cargo dependency is exercised. If API friction surfaces, file as a new task and adjust `hand-tui` accordingly.

**M5.T2 — `clippy --workspace -- -D warnings` clean-up + Migration notes.** Fix any lints that surfaced over the milestones. Update `packages/tui/README.md`'s "Core Traits" section to reflect the expanded `Component`/`Focusable`. Add a brief `MIGRATION.md` for anyone (future) who held a pre-port `hand-tui` reference — likely empty since no real consumer exists.

## Concrete Steps

All commands run from the repository root: `/Users/wanggang/.touch-code/repos/hand-ai/feat-tui`.

**Pre-flight (before M1.T1):**
```bash
cargo build -p hand-tui
cargo test -p hand-tui 2>&1 | tail -3
cargo clippy -p hand-tui -- -D warnings 2>&1 | tail -3
```
Expected: clean build, the existing 192 inline tests pass (`test result: ok. 192 passed`), no clippy warnings. Snapshot this output into the Surprises section if anything is already broken.

**Per-task workflow (every `T` row):**
1. Read the corresponding TS file in full: `view ~/dev/opensource/pi-mono/packages/tui/src/<file>.ts`.
2. Read existing Rust counterpart if any.
3. Edit Rust source.
4. `cargo test -p hand-tui --no-run` — confirm it compiles.
5. `cargo test -p hand-tui <module>::` — confirm new tests pass.
6. `cargo clippy -p hand-tui -- -D warnings` — must be clean before commit.
7. `/commit` (per user's global rule: atomic commit per logical change).
8. Tick the Progress checkbox; add row to Decision Log if a non-trivial decision was made.

**Milestone exit gate (after each Mn final task):**
```bash
cargo test -p hand-tui
cargo clippy -p hand-tui -- -D warnings
cargo doc -p hand-tui --no-deps
```
All three must succeed. Update Outcomes & Retrospective at each milestone close.

## Validation and Acceptance

The plan is complete when:

1. **Test parity.** `ls packages/tui/tests/*.rs | wc -l` ≥ 22 plus 6 regression files = ≥ 28 files. `cargo test -p hand-tui` reports ≥ (192 inline + ~150 integration) passing tests.
2. **Behavioral parity.** `cargo run --example tui-demo` opens an interactive editor, accepts `Ctrl+C`-once-cancel-twice-quit, shows a slash-command popup on `/`, dismisses overlays cleanly, and re-flows on `tput cols 60` resize.
3. **Build hygiene.** `cargo clippy --workspace -- -D warnings` and `cargo fmt -- --check` exit zero.
4. **Consumer smoke.** `hand-coding-agent` has at least one `use hand_tui::Tui` import and `cargo build -p hand-coding-agent` succeeds.
5. **Regression coverage.** Each file under `tests/` named with a `regression_*` or named TS `*-regression-*` prefix exists and corresponds to a real TS test.

For each individual `T` row, acceptance is: the named TS test file's cases pass when ported, and the corresponding TS source file's public API is mirrored in Rust with snake_case names.

## Idempotence and Recovery

Every task is structured as additive (new file) or rewriting a single Rust file. Rerunning a task is safe: the Rust source is overwritten to match the TS file's current state. Git is the recovery substrate — if a milestone introduces a regression, `git revert <commit-range>` restores the prior milestone's state because of the per-task atomic-commit rule.

The `tests/` directory is created idempotently via `mkdir -p packages/tui/tests`. Cargo discovers integration tests by file presence, no manifest entry needed.

If `cargo clippy -- -D warnings` blocks a commit, the failure mode is to fix the warnings rather than `--no-verify` past them (per user's global rule and `CLAUDE.md`).

The work can stop and resume at any milestone boundary because every milestone leaves the crate green. Mid-milestone resume: read the Progress checklist, find the highest unchecked `T`, read its description, continue.

## Artifacts and Notes

**Sample TS test that motivates a regression case (from `pi-mono/packages/tui/test/regression-regional-indicator-width.test.ts`):** flag emojis like 🇯🇵 are made of two regional-indicator codepoints. A naïve `.length` reports 2 graphemes; correct `visible_width` reports 2 (display width). The Rust port using `unicode-segmentation::graphemes(true)` + `unicode-width` must yield 2. This is the canary for "did you actually use grapheme clusters or did you fall back to char iteration?"

**Decision example for word-wrap:** TS uses `Intl.Segmenter` lazily-instantiated. Rust analogue: `unicode_segmentation::UnicodeSegmentation::graphemes(s, true)` (the `true` enables extended grapheme clusters). Test against the same sample strings used in `wrap-ansi.test.ts`.

**Performance note:** `DiffRenderer` already implements line-level diff. Editor viewport scrolling does NOT require a per-cell diff; we only need to ensure full re-renders happen on resize. No performance work is in scope for this plan.

## Interfaces and Dependencies

**New crate dependencies** (added to `packages/tui/Cargo.toml`):
```toml
unicode-segmentation = "1"
base64 = "0.22"
phf = { version = "0.11", features = ["macros"] }   # optional; LazyLock+HashMap acceptable
```
`tokio` is already declared with `full` features (covers `signal` and `io`).

**Public surface added to `lib.rs`:**
```rust
pub mod stdin_buffer;
pub mod keybindings;
pub mod terminal_image;

pub use stdin_buffer::{StdinBuffer, StdinBufferEvent, StdinBufferOptions};
pub use keybindings::{Keybinding, KeybindingsManager, KeybindingDefinition, TUI_KEYBINDINGS};
pub use terminal_image::{
    encode_kitty, encode_iterm2, image_fallback, detect_capabilities,
    get_cell_dimensions, calculate_image_rows, allocate_image_id,
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions,
};
```

**Trait updates in `tui.rs`:**
```rust
pub trait Component: Send {
    fn render(&self, width: u16) -> Vec<String>;
    fn handle_input(&mut self, event: &InputEvent) -> HandleResult { HandleResult::Ignored }
    fn invalidate(&mut self) {}
    fn wants_key_release(&self) -> bool { false }
    fn hide(&mut self) {}                       // NEW
    fn set_hidden(&mut self, hidden: bool) {}   // NEW
    fn is_hidden(&self) -> bool { false }       // NEW
}

pub trait Focusable: Component {
    fn focused(&self) -> bool;
    fn set_focused(&mut self, focused: bool);
    fn cursor_position(&self) -> Option<(u16, u16)>;
    fn focus(&mut self) { self.set_focused(true); }     // NEW
    fn unfocus(&mut self) { self.set_focused(false); }  // NEW
    fn is_focused(&self) -> bool { self.focused() }     // NEW
}

pub enum InputEvent {                                   // NEW
    Key(Key),
    Paste(String),
    Resize { cols: u16, rows: u16 },
    Tick,
}

pub type InputListener = Box<dyn FnMut(&InputEvent) -> ListenerResult + Send>;
pub struct ListenerId(u64);
pub struct ListenerResult { pub consume: bool }

impl Tui {
    pub async fn run(&mut self) -> Result<(), TuiError>;
    pub fn request_render(&mut self);
    pub fn request_render_force(&mut self);
    pub fn add_input_listener(&mut self, listener: InputListener) -> ListenerId;
    pub fn remove_input_listener(&mut self, id: ListenerId);
    pub fn show_overlay(&mut self, c: Box<dyn Component>, opts: OverlayOptions) -> OverlayHandle;
    pub fn hide_overlay(&mut self);
    pub fn has_overlay(&self) -> bool;
    pub fn set_focus(&mut self, target: ComponentId);
    pub fn focus(&self) -> Option<ComponentId>;
    pub fn set_clear_on_shrink(&mut self, enabled: bool);
}
```

**Autocomplete provider interface** (in `components/autocomplete.rs`, M3.T4):
```rust
pub trait AutocompleteProvider: Send + Sync {
    fn query<'a>(&'a self, ctx: &'a AutocompleteContext)
        -> Pin<Box<dyn Future<Output = Vec<AutocompleteItem>> + Send + 'a>>;
}

pub struct CombinedAutocompleteProvider {
    providers: Vec<Arc<dyn AutocompleteProvider>>,
}

#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub kind: AutocompleteItemKind,
}

#[derive(Debug, Clone)]
pub enum AutocompleteTrigger { Slash, At, Manual }
```

**Error type** (new, in `error.rs` — currently the crate has no top-level error):
```rust
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("terminal raw mode unavailable: {0}")]
    RawMode(String),
    #[error("stdin reader exited unexpectedly")]
    StdinClosed,
}
```

**External crate version pins** (from `Cargo.toml`):
- `crossterm = "0.28"` — keep, expand usage to raw mode + alternate screen.
- `unicode-width = "0.2"` — keep.
- `pulldown-cmark = "0.12"` — keep, expand for tables/links.
- `tokio = "1"` (full features) — keep.
- `serde = "1"`, `serde_json = "1"` — keep for `KeybindingsConfig` deserialization.

---

**PLAN READY FOR REVIEW:**
- Title: Port pi-tui (TypeScript) to hand-tui (Rust) — Full API & Behavior Parity
- Plan structure: 5 milestones, 17 atomic tasks
- Open risks: 3 — (a) async stdin + raw mode interaction on Windows is the least-tested path; (b) `Intl.Segmenter` paste-marker logic may not map 1:1 to a regex iterator and might need a small custom segmenter; (c) overlay style-leak fix may force `DiffRenderer` to track SGR state, expanding M2.T3's scope.

→ Approve, ask for revisions, or tell me which milestone to start with.
