//! Inline gallery for the rt primitive widgets.
//!
//! A section-based tour of the `hand_tui::rt::components` primitives, built on the
//! rt stack the same way `rt_demo` is: a [`SessionGuard`] establishes the inline
//! viewport (never the alternate screen), the [`FrameScheduler`] is the single
//! painter (input only mutates state and requests a frame), and the input pump
//! feeds `RtInputEvent`s over a channel. Resize folds the new geometry into the
//! tracked size and requests one coalesced re-anchoring frame.
//!
//! # Sections
//!
//! The gallery is a **registry of sections** ([`Section`]): each names a topic and
//! carries a builder that lays out primitives into the section's content area.
//! This first milestone registers the six primitive widgets; later M2 features
//! (markdown, lists, loader, editor, image) register their own sections by
//! pushing onto the same registry — the navigation, layout, and draw path are
//! written once here and do not change as sections are added.
//!
//! # Navigation and interaction routing
//!
//! The gallery runs in one of two input regimes depending on the active section:
//!
//! - **Display-only sections** (Text/Box/Spacer/Truncated/StatusBar/Progress/
//!   Markdown/Loader/Toast/Image) are static painters. They keep the full gesture
//!   set below (`h`/`l`/`n`/`p`, arrows, digits, and the section-specific `d`/`x`/
//!   `i`/`k`/`o`).
//! - **Interactive sections** (Select/Settings/Editor/Autocomplete) hold a *live*
//!   component in gallery state. Every key that is not a reserved gallery gesture
//!   is routed straight into that component's `handle_key`, so a tmux-driven live
//!   walk can navigate a select list, edit a setting in place, type into the
//!   editor, or drive the autocomplete popup and watch it respond in real time.
//!   Only `Tab` / `BackTab` and the quit chords stay gallery-level here, so the
//!   tour can always advance out of an interactive section without the component
//!   swallowing the switch. (Digits route to the component too, so a number can be
//!   typed into the editor.)
//!
//! Reserved gallery gestures (always gallery-level, every section):
//!
//! - `Tab` / `Right` / `l` / `n` : next section (wraps) — on an interactive section
//!   only `Tab` switches; `Right`/`l`/`n` are handed to the component.
//! - `BackTab` / `Left` / `h` / `p` : previous section (wraps) — on an interactive
//!   section only `BackTab` switches.
//! - `1`..=`9` : jump directly to a section by number (display-only sections).
//! - `d` : (Toast section) dismiss the newest toast — reveals a hidden overflow
//! - `x` : (Toast section) advance toast TTLs — an expiring toast reveals the next
//! - `Ctrl+C` / `Ctrl+D` / `q` : quit cleanly (terminal fully restored)
//!
//! Run it:
//!   cargo run -p hand-tui --example rt_gallery
//!
//! On a non-TTY (piped) stdin/stdout it prints a diagnostic and exits non-zero
//! without ever touching the parent shell's terminal mode.

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use hand_tui::rt::components::{
    BorderTint, CancellableLoader, CombinedProvider, Editor, Loader, MarkdownView, PathEntry,
    PathProvider, ProgressBar, RawEmissionQueue, ResolvedProtocol, RtImage, ScrollbackImageChannel,
    SelectItem, SelectList, SelectListLayout, SettingEntry, SettingValue, SettingsList,
    SlashCommand, SlashProvider, Spacer, StatusBar, TextBlock, Toast, ToastLevel, TruncatedText,
    WidgetBox, default_markdown_theme, osc8_emission, write_cell_size_query,
};
use hand_tui::rt::events::{RtInputEvent, RtKey, spawn_event_pump};
use hand_tui::rt::history::HistorySink;
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionError, SessionGuard, SessionTerminal};
use hand_tui::rt::view::{RtComponent, TerminalSize};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};

/// Bound on the input event channel: interactive navigation is low-rate, so a
/// small buffer is plenty.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// A section body painter: given the section's content rect and the frame buffer,
/// it lays out that section's widgets. Boxed and `Send` so it can live in the
/// shared [`GalleryState`] the scheduler's draw task reads.
type SectionBuilder = Box<dyn Fn(Rect, &mut Buffer) + Send>;

/// Environment variable that forces the image section's graphics protocol,
/// bypassing capability detection so a `script -q` byte capture is deterministic
/// even inside tmux (whose own capabilities would otherwise apply). Values:
/// `kitty`, `iterm2`, `fallback`. Unset resolves from the live terminal.
///
/// This is the protocol-emission test seam the image probes drive (mirrors the
/// M1 demo's force-kitty-keyboard flag): a probe runs the gallery with this env
/// set, navigates to the Image section, captures the raw bytes, and greps for the
/// forced protocol's prefix (`\x1b_G` for Kitty, `\x1b]1337;File=` for iTerm2, or
/// zero graphics bytes for fallback / tmux).
const FORCE_IMAGE_PROTOCOL_ENV: &str = "HAND_TUI_FORCE_IMAGE_PROTOCOL";

/// Resolve the forced image protocol from the environment seam, if set.
fn forced_image_protocol() -> Option<ResolvedProtocol> {
    match std::env::var(FORCE_IMAGE_PROTOCOL_ENV).ok()?.as_str() {
        "kitty" => Some(ResolvedProtocol::Kitty),
        "iterm2" => Some(ResolvedProtocol::ITerm2),
        "fallback" => Some(ResolvedProtocol::Fallback),
        _ => None,
    }
}

/// The sample PNG the Image section displays, embedded at build time so the demo
/// needs no runtime fixture path.
const SAMPLE_IMAGE_PNG: &[u8] = include_bytes!("../../../tests/fixtures/tui/images/sample.png");

/// A large PNG whose base64 payload exceeds the 4096-char Kitty APC chunk limit,
/// so the Image section's oversized slot exercises multi-chunk transfer (and,
/// under the auto-flood env, chunking mid-flood — the flood seam a resize/APC
/// balance probe drives).
const HUGE_IMAGE_PNG: &[u8] = include_bytes!("../../../tests/fixtures/tui/images/huge.png");

/// A JPEG whose container magic sniffs but whose payload does not decode, so the
/// Image section's corrupt slot exercises decode-validation: on *every* persona it
/// must degrade to the placeholder box, emitting zero graphics bytes.
const CORRUPT_IMAGE_JPG: &[u8] = include_bytes!("../../../tests/fixtures/tui/images/corrupt.jpg");

/// Environment variable that auto-starts a frame flood at launch (mirrors the
/// rt_demo flood seam), so a probe can capture a large image's chunked transfer
/// *while frames are storming* — the flood seam under which the 4096-char APC
/// chunks must stay balanced (no half-escape at a frame seam). Set to `1`.
const FLOOD_ENV: &str = "HAND_TUI_GALLERY_FLOOD";

/// How often the flood task requests a frame: far above the scheduler's ~60fps
/// ceiling so coalescing/rate-limiting (and mid-flood emission) are exercised.
const FLOOD_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_micros(2_000);

/// Whether the auto-flood is requested by the environment seam.
fn flood_requested_by_env() -> bool {
    std::env::var(FLOOD_ENV).map(|v| v == "1").unwrap_or(false)
}

/// The Markdown section's source. Hoisted to a const so both the section builder
/// (which paints the styled text) and `draw`'s Markdown special-case (which
/// computes the OSC 8 link rows for the raw channel) render from the *same* text.
const MARKDOWN_SOURCE: &str = "\
# Markdown renderer

Body text with **bold, _nested italic_, back** to bold, `inline code`, \
a [link](https://example.com) and an ![image alt](diagram.png).

3. third item
4. fourth item

- outer bullet
  - nested bullet
- [ ] pending task
- [x] done task

> a blockquote line, dimmed and italic

---

```rust
fn main() {
    /* a block comment
       spanning lines */
    let count: usize = 42; // trailing
    println!(\"hello\");
}
```

| name | 值 |
|:-----|---:|
| 你好世界 | 1 |
| ascii | 22 |

~~struck through~~ text.";

/// A single gallery section: a title plus a builder that lays its primitives out
/// into the section's content rect.
///
/// The builder is a boxed closure so a later feature can register a section
/// without this file knowing anything about that section's widgets — it just
/// pushes a `Section` whose `build` paints whatever it likes into the area it is
/// handed. Every builder receives the content `Rect` (already inset from the
/// bordered frame) and the shared `Buffer`, exactly the surface an `RtComponent`
/// renders into.
struct Section {
    /// Short title shown in the section tab strip and the content header.
    title: &'static str,
    /// Paints this section's body into the given content rect.
    build: SectionBuilder,
}

impl Section {
    fn new(title: &'static str, build: impl Fn(Rect, &mut Buffer) + Send + 'static) -> Self {
        Self {
            title,
            build: Box::new(build),
        }
    }
}

/// The live, key-consuming widget behind an interactive section.
///
/// Where the display-only sections are per-frame [`SectionBuilder`] painters,
/// these four hold a *persistent* [`RtComponent`] in gallery state so a typed key
/// mutates it across frames — the whole point of interactive routing. Each is
/// seeded once (in [`build_interactive`]) with the same canned demo gestures the
/// stateless builders used to drive, so a section that is never touched still
/// renders its signature shape; from there a live walk drives it further.
///
/// The variant names match the section titles routing keys to them keys off
/// ([`InteractiveKind::from_title`]).
enum Interactive {
    /// The `Select` section's scrollable select list (↑/↓ navigate, Enter picks).
    Select(SelectList),
    /// The `Settings` section's settings list (↑/↓ move, Enter/Space edits).
    Settings(SettingsList),
    /// The `Editor` section's multi-line editor (grapheme-aware typing, kill-ring).
    Editor(Editor),
    /// The `Autocomplete` section's editor with slash/path providers installed.
    Autocomplete(Editor),
}

impl Interactive {
    /// Borrow the underlying component as a mutable `RtComponent`, so a routed key
    /// reaches its `handle_key` regardless of which widget backs the section.
    fn as_component_mut(&mut self) -> &mut dyn RtComponent {
        match self {
            Interactive::Select(c) => c,
            Interactive::Settings(c) => c,
            Interactive::Editor(c) | Interactive::Autocomplete(c) => c,
        }
    }

    /// Borrow the underlying component immutably, for rendering and cursor.
    fn as_component(&self) -> &dyn RtComponent {
        match self {
            Interactive::Select(c) => c,
            Interactive::Settings(c) => c,
            Interactive::Editor(c) | Interactive::Autocomplete(c) => c,
        }
    }
}

/// Which interactive widget a section title maps to, or `None` for a display-only
/// section. This is the single place the interactive-vs-static split is decided,
/// so navigation routing and body rendering agree on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveKind {
    Select,
    Settings,
    Editor,
    Autocomplete,
}

impl InteractiveKind {
    fn from_title(title: &str) -> Option<Self> {
        match title {
            "Select" => Some(Self::Select),
            "Settings" => Some(Self::Settings),
            "Editor" => Some(Self::Editor),
            "Autocomplete" => Some(Self::Autocomplete),
            _ => None,
        }
    }
}

/// The gallery's mutable state, shared between the input loop (which mutates the
/// active index and tracked size) and the scheduler's draw closure (which reads
/// them). A plain `std::sync::Mutex`: every critical section is a tiny field
/// access, and the scheduler's draw closure is synchronous.
struct GalleryState {
    /// The registered sections, in navigation order.
    sections: Vec<Section>,
    /// The live interactive widget for each section, parallel to `sections`.
    /// `None` for a display-only section; `Some` for the four interactive ones,
    /// seeded once so an untouched section still shows its signature shape and a
    /// routed key mutates it across frames.
    interactive: Vec<Option<Interactive>>,
    /// Index of the currently shown section.
    active: usize,
    /// Tracked terminal geometry, overwritten whole on each resize event.
    size: TerminalSize,
    /// Persistent toast stack for the Toast section. Held in state (not rebuilt
    /// per frame) so the dismiss-newest / TTL-tick gestures mutate it across
    /// frames, making the overflow-hidden-then-reappears behaviour observable in a
    /// capture (VAL-WIDGET-012). See [`toast_seam`].
    toast: Toast,
    /// The raw graphics-emission channel shared with the Image section: the
    /// section's builder enqueues the encoded escape into it during `draw`, and
    /// the scheduler drains it to stdout right after `terminal.draw`, positioned
    /// at the viewport origin (see [`spawn_scheduler`]). This is the in-viewport
    /// half of the buffer-bypass mechanism `m2-image-scrollback` extends.
    image_queue: RawEmissionQueue,
    /// The scrollback image channel: the stable content→id map + committed-id
    /// registry that keeps a scrolled-into-history image transmitted a bounded
    /// number of times and protected from any delete (see
    /// [`ScrollbackImageChannel`]). Shared with the scheduler, which commits and
    /// deletes on the terminal-owning task.
    scrollback: ScrollbackImageChannel,
    /// Images queued to be scrolled into native scrollback on the next frame
    /// (the `i` gesture). Drained by the scheduler *between* draws — the only
    /// place `insert_before` is legal — so each is committed once.
    pending_scrollback: Vec<PendingScrollbackImage>,
    /// Viewport-only image ids queued for a Kitty delete-by-id on the next frame
    /// (the `D` drop gesture). A committed (scrollback) id is refused by the
    /// channel, so this can only ever delete a pure-viewport image and never a
    /// wide `d=A`/`d=a` form.
    pending_deletes: Vec<u32>,
    /// Whether the demonstration overlay is open (the `o` gesture). Toggling it
    /// must not perturb any image already scrolled into scrollback (no
    /// re-transmit, no delete) — the VAL-CROSS-004 safety check.
    overlay_open: bool,
}

/// An image queued to be committed into native scrollback, carried as its raw
/// bytes + label + protocol so the terminal-owning scheduler can build the
/// [`RtImage`] and commit it at the current width between draws.
struct PendingScrollbackImage {
    data: &'static [u8],
    label: String,
    protocol: ResolvedProtocol,
}

impl GalleryState {
    fn new(size: TerminalSize) -> Self {
        let image_queue = RawEmissionQueue::new();
        let sections = register_sections(image_queue.clone());
        let interactive = sections
            .iter()
            .map(|s| InteractiveKind::from_title(s.title).map(build_interactive))
            .collect();
        Self {
            sections,
            interactive,
            active: 0,
            size,
            toast: toast_seam(),
            image_queue,
            scrollback: ScrollbackImageChannel::new(),
            pending_scrollback: Vec::new(),
            pending_deletes: Vec::new(),
            overlay_open: false,
        }
    }

    /// Queue the sample image to be scrolled into native scrollback (the `i`
    /// gesture). Only meaningful in the Image section; a no-op elsewhere.
    fn scroll_image_into_history(&mut self) {
        if self.section_title() != Some("Image") {
            return;
        }
        let protocol = forced_image_protocol().unwrap_or(ResolvedProtocol::Kitty);
        self.pending_scrollback.push(PendingScrollbackImage {
            data: SAMPLE_IMAGE_PNG,
            label: "scrollback sample".to_string(),
            protocol,
        });
    }

    /// Drop a pure-viewport image (the `k` gesture): queue a viewport-only delete.
    ///
    /// Targets the large image, which the `i` gesture never commits (that commits
    /// the *sample*), so this is always a genuine viewport-only image and the
    /// delete fires — a single-id Kitty delete, never a wide `d=A`/`d=a`. The
    /// committed sample sitting in scrollback is a *different* id and stays
    /// untouched: the safety invariant a probe checks by dropping after a commit
    /// and seeing the committed image survive.
    fn drop_viewport_image(&mut self) {
        if self.section_title() != Some("Image") {
            return;
        }
        let id = self.scrollback.image_id(HUGE_IMAGE_PNG);
        self.pending_deletes.push(id);
    }

    /// Toggle the demonstration overlay (the `o` gesture). Opening/closing it must
    /// leave any scrollback image untouched.
    fn toggle_overlay(&mut self) {
        self.overlay_open = !self.overlay_open;
    }

    /// The active section's title, or `None` when there are no sections.
    fn section_title(&self) -> Option<&'static str> {
        self.sections.get(self.active).map(|s| s.title)
    }

    /// Dismiss the newest toast: the host gesture that reveals a hidden overflow
    /// toast (the observable half of retain-don't-discard).
    fn dismiss_toast(&mut self) {
        self.toast.dismiss_newest();
    }

    /// Advance the toast TTLs by one tick: an expiring toast dropping out reveals
    /// the next hidden one, the same way dismiss-newest does.
    fn tick_toast(&mut self) {
        self.toast.tick_ttl();
    }

    /// Move to the next section, wrapping past the last back to the first.
    fn next(&mut self) {
        if !self.sections.is_empty() {
            self.active = (self.active + 1) % self.sections.len();
        }
    }

    /// Move to the previous section, wrapping past the first to the last.
    fn prev(&mut self) {
        if !self.sections.is_empty() {
            self.active = (self.active + self.sections.len() - 1) % self.sections.len();
        }
    }

    /// Jump directly to section `index` if it is in range.
    fn jump(&mut self, index: usize) {
        if index < self.sections.len() {
            self.active = index;
        }
    }

    /// Whether the active section is one of the interactive four (so non-reserved
    /// keys should route into its live component rather than drive navigation).
    fn active_is_interactive(&self) -> bool {
        self.interactive
            .get(self.active)
            .is_some_and(Option::is_some)
    }

    /// The active section's live interactive component, mutably, if it has one.
    fn active_interactive_mut(&mut self) -> Option<&mut Interactive> {
        self.interactive.get_mut(self.active)?.as_mut()
    }

    /// The active section's live interactive component, immutably, if it has one.
    fn active_interactive(&self) -> Option<&Interactive> {
        self.interactive.get(self.active)?.as_ref()
    }

    /// Route a key into the active section's live interactive component.
    ///
    /// Only called when the active section is interactive; the key was already
    /// filtered of the reserved gallery gestures by the caller, so whatever
    /// arrives here is genuinely the component's to handle.
    fn route_to_interactive(&mut self, key: &RtKey) {
        if let Some(interactive) = self.active_interactive_mut() {
            let _ = interactive.as_component_mut().handle_key(key);
        }
    }
}

/// Register the sections for this milestone: the six primitive widgets. Later M2
/// features append their own sections here (or via a follow-on registration
/// function) without touching navigation or draw.
///
/// `image_queue` is threaded into the Image section so its builder can enqueue a
/// graphics-protocol emission during `draw`; the scheduler flushes the queue to
/// the terminal right after the frame draws.
fn register_sections(image_queue: RawEmissionQueue) -> Vec<Section> {
    vec![
        Section::new("Text", |area, buf| {
            let text = TextBlock::new(
                "TextBlock word-wraps its content to the inner width and honours \
                 padding. Resize the terminal to watch the wrap re-flow. \
                 Wide glyphs (你好世界🎉) count as their display width.",
            )
            .padding(1, 0);
            text.render(area, buf);
        }),
        Section::new("Box", |area, buf| {
            let inner = Box::new(TextBlock::new(
                "WidgetBox fills its whole area with a background style (padding \
                 included) and paints its child inset by the padding.",
            ));
            let bx = WidgetBox::new()
                .background(Style::default().bg(Color::Blue))
                .padding(2, 1)
                .child(inner);
            bx.render(area, buf);
        }),
        Section::new("Spacer", |area, buf| {
            // A vertical stack: label, a 3-row spacer, then a second label, so the
            // precise gap between them is visible.
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            TruncatedText::new("above the spacer").render(rows[0], buf);
            Spacer::new(3).render(rows[1], buf);
            TruncatedText::new("exactly 3 blank rows above this line").render(rows[2], buf);
        }),
        Section::new("Truncated", |area, buf| {
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            TruncatedText::new(
                "This single line is deliberately far too long to fit and will be \
                 clipped with an ellipsis when the terminal is narrow.",
            )
            .render(rows[0], buf);
            TruncatedText::new("你好世界你好世界你好世界你好世界 — wide glyphs truncate cleanly")
                .render(rows[1], buf);
        }),
        Section::new("StatusBar", |area, buf| {
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            StatusBar::new()
                .left("Model: gpt")
                .center("rt gallery")
                .right("Session: 42")
                .style(Style::default().add_modifier(Modifier::REVERSED))
                .render(rows[0], buf);
        }),
        Section::new("Progress", |area, buf| {
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            ProgressBar::new()
                .ratio(0.25)
                .label("25%")
                .style(Style::default().fg(Color::Green))
                .render(rows[0], buf);
            ProgressBar::new()
                .ratio(0.75)
                .label("75%")
                .style(Style::default().fg(Color::Cyan))
                .render(rows[1], buf);
            // An over-range ratio clamps to 100% rather than overflowing.
            ProgressBar::new()
                .ratio(1.5)
                .label("clamped")
                .style(Style::default().fg(Color::Magenta))
                .render(rows[2], buf);
        }),
        Section::new("Markdown", |area, buf| {
            // One source touching every block/inline signature: headings, an
            // ordered list starting past 1, a nested bullet list, a blockquote,
            // a rule, a fenced code block, a CJK table, nested inline styles, a
            // link, an image (degraded to alt), strikethrough and task markers.
            // Resize between 100 and 40 columns to watch it reflow with the
            // code-block frame staying intact.
            //
            // The link renders as a real OSC 8 hyperlink out of band on a capable
            // terminal (see the Markdown special-case in `draw`, which computes the
            // link rows and hands them to the scheduler's raw flush); on tmux /
            // fallback the renderer paints the `text (url)` form in the cells.
            // Wire the real keyword-driven highlighter into the fenced code
            // block so keyword/string/number/comment/type render in distinct
            // colors (VAL-WIDGET-004).
            MarkdownView::new(MARKDOWN_SOURCE)
                .theme(default_markdown_theme())
                .render(area, buf);
        }),
        Section::new("Select", |area, buf| {
            // A two-column select list: labels in a padded primary column, dimmed
            // descriptions in the second, the selected row a reversed highlight
            // bar with a `▸` indicator. A window of 4 over 6 items shows the
            // `(n/total)` counter; the description on "sonnet" is deliberately
            // long so it truncates with an ellipsis on a narrow terminal.
            //
            // Interactive: the live list lives in gallery state (see
            // `build_interactive`); this builder paints only the header, and `draw`
            // renders the persistent list into the body so ↑/↓ navigate it live.
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            TruncatedText::new(
                "SelectList — ↑/↓ navigate (live), ▸ selected row, dimmed description column",
            )
            .render(rows[0], buf);
        }),
        Section::new("Settings", |area, buf| {
            // A settings list exercising every value type plus the full chrome:
            // an enum, a bool, a number, and a string, with the focused entry's
            // description below the list and the footer hint.
            //
            // Interactive: the live list lives in gallery state; this builder paints
            // only the header and `draw` renders the list from state, so ↑/↓ move
            // the selection and Enter/Space edits in place, live.
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            TruncatedText::new(
                "SettingsList — ↑/↓ move · Enter/Space edits in place (live); selection clamps",
            )
            .render(rows[0], buf);
        }),
        Section::new("Loader", |area, buf| {
            // The loader family: a basic Loader (spinner + static message) and a
            // CancellableLoader (spinner + message + elapsed suffix + a block-glyph
            // progress bar with a percentage + an Escape-to-cancel hint). The
            // spinner *glyph* is host-timed and not asserted; the static message
            // text is what the capture confirms.
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Length(1), // basic loader
                Constraint::Length(1), // spacer
                Constraint::Length(3), // cancellable loader (3 rows)
                Constraint::Min(0),
            ])
            .split(area);
            TruncatedText::new(
                "Loader — animated spinner + static message; cancellable variant below",
            )
            .render(rows[0], buf);
            Loader::new("Working on your request…").render(rows[1], buf);
            let mut cancellable = CancellableLoader::new("Compiling project");
            cancellable.set_elapsed(Some("3.2s".to_string()));
            cancellable.set_progress(Some(0.42));
            cancellable.render(rows[3], buf);
        }),
        Section::new("Toast", |area, buf| {
            // The header lives in the stateless builder; the toast stack itself is
            // painted from persistent gallery state by `draw` (see the special-case
            // there) so the dismiss-newest (`d`) and TTL-tick (`x`) gestures reveal
            // the hidden overflow toast across frames.
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            TruncatedText::new(
                "Toast — newest first, cap hides (not drops) overflow · d: dismiss-newest · x: tick TTL",
            )
            .render(rows[0], buf);
        }),
        Section::new("Editor", |area, buf| {
            // The multi-line editor core in its box style: grapheme-aware editing,
            // auto-grow, a `line:col` indicator woven into the bottom rail, and the
            // focus/thinking border tint.
            //
            // Interactive: the editor lives in gallery state, seeded once with the
            // draft below (see `build_interactive`). This builder paints only the
            // header; `draw` renders the live editor into the body so typed keys
            // edit it in real time (a tmux live walk: type, C-w/C-y, undo). The
            // TestBackend paths in `tests/rt_editor.rs` still pin the fine grain.
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Min(0),    // editor box
            ])
            .split(area);
            TruncatedText::new(
                "Editor — type to edit (live) · grapheme-aware · kill-ring (C-w/C-y) · undo",
            )
            .render(rows[0], buf);
        }),
        Section::new("Autocomplete", |area, buf| {
            // The editor with an autocomplete provider installed: a `/` at the
            // start of the line opens the slash-command popup, an `@` opens the
            // path popup.
            //
            // Interactive: the editor lives in gallery state, seeded with a `/h`
            // that opens the popup. This builder paints only the header; `draw`
            // renders the live editor so typing filters the popup, ↑/↓ navigate,
            // Tab accepts, and Esc closes — all live.
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Min(0),    // editor box + popup
            ])
            .split(area);
            TruncatedText::new(
                "Autocomplete — type `/` or `@` (live) · ↑↓ navigate · Tab accept · Esc close",
            )
            .render(rows[0], buf);
        }),
        Section::new("Image", move |area, buf| {
            // The image widget + graphics emission. On a graphics terminal the
            // widget reserves N rows (blank cells) here and *enqueues* the encoded
            // escape into the shared queue; the scheduler emits it out of band
            // right after this draw, positioned onto those rows. On a plain
            // terminal / inside tmux it paints a bordered placeholder box with the
            // filename and the sniffed `[<mime> WxH]` tag, and enqueues nothing —
            // zero graphics bytes reach the wire.
            //
            // The protocol is forced by the `HAND_TUI_FORCE_IMAGE_PROTOCOL` env
            // seam so a `script -q` capture is deterministic regardless of the
            // host terminal (an unset env resolves from the live capabilities).
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Min(0),    // image body
            ])
            .split(area);
            let mode = forced_image_protocol();
            let header = match mode {
                Some(ResolvedProtocol::Kitty) => {
                    "Image — Kitty APC (forced) · reserves rows, emits \\x1b_G out of band"
                }
                Some(ResolvedProtocol::ITerm2) => {
                    "Image — iTerm2 OSC 1337 (forced) · native passthrough"
                }
                Some(ResolvedProtocol::Fallback) => {
                    "Image — fallback (forced) · bordered box, zero graphics bytes"
                }
                None => "Image — protocol resolved from the live terminal capabilities",
            };
            TruncatedText::new(header).render(rows[0], buf);

            // The body is a stack of safety-scenario slots so a `script -q` probe
            // can drive each layout/decode/label case from one section:
            //   1. the plain sample (baseline emission),
            //   2. a large image whose base64 exceeds the 4096-char APC limit
            //      (multi-chunk transfer — under the flood seam this chunks
            //      mid-flood),
            //   3. a corrupt source (decode-validation → box on every persona),
            //   4. a CJK-labelled image (display-width label clipping).
            let slots = Layout::vertical([
                Constraint::Length(4), // sample
                Constraint::Length(6), // large / chunked
                Constraint::Length(4), // corrupt → box
                Constraint::Length(4), // CJK label box
                Constraint::Min(0),
            ])
            .split(rows[1]);

            let build = |data: &'static [u8], label: &str, area: Rect, buf: &mut Buffer| {
                let mut image = RtImage::new(data)
                    .label(label.to_string())
                    .emission_queue(image_queue.clone());
                if let Some(protocol) = mode {
                    image = image.protocol(protocol);
                }
                image.render(area, buf);
            };

            build(SAMPLE_IMAGE_PNG, "sample.png", slots[0], buf);
            build(HUGE_IMAGE_PNG, "huge.png (chunked)", slots[1], buf);
            build(CORRUPT_IMAGE_JPG, "broken.jpg", slots[2], buf);
            // A CJK/emoji label long enough to clip in a narrow box: the label row
            // must stay inside the frame by display width (border stays aligned).
            build(
                SAMPLE_IMAGE_PNG,
                "你好世界你好世界你好世界🎉 sample",
                slots[3],
                buf,
            );
        }),
    ]
}

/// Build the live component behind an interactive section, seeded once with the
/// same canned demo gestures the stateless builders used to drive every frame.
///
/// Seeding here (rather than per frame) is what makes the section *interactive*:
/// the seeded state is the starting point, and routed keys mutate it from there.
/// An untouched section still renders its signature shape (the seed), so the tour
/// smoke test sees the same text it did when the builders were stateless.
fn build_interactive(kind: InteractiveKind) -> Interactive {
    match kind {
        InteractiveKind::Select => {
            let mut list = SelectList::new(vec![
                SelectItem::new("opus", "Claude Opus").with_description("most capable, slowest"),
                SelectItem::new("sonnet", "Claude Sonnet")
                    .with_description("balanced quality and speed for most day-to-day coding work"),
                SelectItem::new("haiku", "Claude Haiku").with_description("fastest, lightest"),
                SelectItem::new("gpt", "GPT-4o").with_description("multimodal"),
                SelectItem::new("gemini", "Gemini").with_description("long context"),
                SelectItem::new("llama", "Llama").with_description("open weights"),
            ])
            .visible_count(4)
            .layout(SelectListLayout {
                min_primary_column_width: Some(18),
                max_primary_column_width: Some(18),
            });
            // Focus the second item so the highlight bar is not on the first row.
            list.handle_key(&named_key_none("down", crossterm::event::KeyCode::Down));
            Interactive::Select(list)
        }
        InteractiveKind::Settings => {
            let mut list = SettingsList::new(vec![
                SettingEntry::new(
                    "theme",
                    SettingValue::Enum {
                        choices: vec!["dark".into(), "light".into(), "auto".into()],
                        selected: 0,
                    },
                    "Color theme — Enter cycles through the choices",
                ),
                SettingEntry::new(
                    "auto_save",
                    SettingValue::Bool(true),
                    "Enter/Space flips this boolean instantly",
                ),
                SettingEntry::new(
                    "max_tokens",
                    SettingValue::Number(4096.0),
                    "A number, edited inline; a non-numeric edit is rejected",
                ),
                SettingEntry::new(
                    "model",
                    SettingValue::String("claude-sonnet".into()),
                    "A free-text string, edited inline with a visible caret",
                ),
            ])
            .show_description(true)
            .show_hint(true);
            // Focus the number entry and open its inline editor so the caret and
            // edit-in-place value render out of the box.
            list.handle_key(&named_key_none("down", crossterm::event::KeyCode::Down));
            list.handle_key(&named_key_none("down", crossterm::event::KeyCode::Down));
            list.handle_key(&named_key_none("enter", crossterm::event::KeyCode::Enter));
            Interactive::Settings(list)
        }
        InteractiveKind::Editor => {
            let mut editor = Editor::new();
            // A multi-line draft with CJK content to show cluster-aware editing and
            // the auto-grow shape. Each printable key is one press; `alt+enter`
            // inserts a soft break without submitting.
            for c in "review the ".chars() {
                editor.handle_key(&char_key(c));
            }
            for c in "文档".chars() {
                editor.handle_key(&char_key(c));
            }
            editor.handle_key(&named_key("alt+enter", crossterm::event::KeyCode::Enter));
            for c in "then ship the draft".chars() {
                editor.handle_key(&char_key(c));
            }
            // Kill-ring + coalescing undo demo, all canned:
            //   Ctrl-W kills the trailing word ("draft") onto the ring,
            //   Ctrl-Y yanks it back — a full cut/paste round trip,
            //   Undo peels the yank as one atomic unit, leaving "then ship the ".
            editor.handle_key(&ctrl_key('w'));
            editor.handle_key(&ctrl_key('y'));
            editor.undo();
            // Paste pipeline demo: a big bracketed paste folds to a compact
            // `[paste #1 …]` marker (the full payload lives out-of-band and is
            // spliced back on submit); a pasted escape sequence lands defused as
            // inert text rather than re-colouring the box.
            editor.handle_key(&char_key(' '));
            let big_paste = (0..40)
                .map(|i| format!("payload line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            editor.insert_paste(&big_paste);
            editor.insert_paste(" \x1b[31mESC-defused\x1b[0m");
            // Show the focused border tint (the thinking tint is the host's to drive
            // during streaming in a later milestone).
            editor.set_tint(BorderTint::Focused);
            Interactive::Editor(editor)
        }
        InteractiveKind::Autocomplete => {
            let mut editor = Editor::new();
            editor.set_autocomplete_provider(Arc::new(CombinedProvider::new(vec![
                Box::new(SlashProvider::new(vec![
                    SlashCommand::new("help").with_description("show help"),
                    SlashCommand::new("history").with_description("recall prompts"),
                    SlashCommand::new("model").with_description("switch model"),
                ])),
                Box::new(PathProvider::new(vec![
                    PathEntry::dir("src"),
                    PathEntry::file("src/main.rs"),
                    PathEntry::file("README.md"),
                ])),
            ])));
            // Type `/h` at the line start: the popup opens filtered to `/help`,
            // `/history`. The first candidate is selected (`▸`).
            for c in "/h".chars() {
                editor.handle_key(&char_key(c));
            }
            editor.set_tint(BorderTint::Focused);
            Interactive::Autocomplete(editor)
        }
    }
}

/// Build a bare printable-character `RtKey` for the gallery's canned editor demo.
fn char_key(c: char) -> RtKey {
    RtKey {
        key_id: Some(c.to_string()),
        raw: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ),
    }
}

/// Build a named-key `RtKey` (e.g. `alt+enter`) with the given crossterm code and
/// ALT modifier, for the gallery's canned editor demo.
fn named_key(id: &str, code: crossterm::event::KeyCode) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::ALT),
    }
}

/// Build a named-key `RtKey` (e.g. `down`, `enter`) with no modifier, for seeding
/// the list widgets' starting selection.
fn named_key_none(id: &str, code: crossterm::event::KeyCode) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    }
}

/// Build a Ctrl-chord `RtKey` (e.g. `ctrl+w`) for the gallery's canned editor demo.
fn ctrl_key(c: char) -> RtKey {
    RtKey {
        key_id: Some(format!("ctrl+{c}")),
        raw: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    }
}

/// Build the persistent toast stack for the Toast section: four toasts under a
/// two-visible cap, so two are shown and two are hidden overflow. The dismiss
/// (`d`) and TTL-tick (`x`) gestures reveal the hidden ones one at a time — the
/// capturable seam for the overflow-hidden-then-reappears behaviour.
fn toast_seam() -> Toast {
    let mut toast = Toast::new();
    toast.set_max_visible(2);
    toast.info("Session started");
    toast.success("Model connected");
    toast.warning("Rate limit near");
    // The newest toast expires on its own after a few TTL ticks, so `x` alone
    // (no `d`) also drives the overflow re-appearance.
    toast.push_with_ttl(ToastLevel::Error, "Request failed — retrying", 3);
    toast
}

/// The index of the Toast section in the registry, so `draw` can special-case it
/// to paint the persistent stack. Kept in sync with `register_sections`.
fn toast_section_index(sections: &[Section]) -> Option<usize> {
    sections.iter().position(|s| s.title == "Toast")
}

fn main() -> ExitCode {
    if wants_help() {
        print_help();
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("rt_gallery: failed to start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(SessionError::NotATty) => {
            eprintln!(
                "rt_gallery: standard input/output is not a terminal (TTY).\n\
                 Run this from an interactive terminal, e.g. \
                 `cargo run -p hand-tui --example rt_gallery`."
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("rt_gallery: {err}");
            ExitCode::FAILURE
        }
    }
}

fn wants_help() -> bool {
    std::env::args()
        .skip(1)
        .any(|arg| arg == "--help" || arg == "-h")
}

fn print_help() {
    println!(
        "rt_gallery — inline tour of the rt widgets\n\
         \n\
         Inline viewport (no alternate screen); prior shell content stays visible.\n\
         Navigate the sections; each shows one widget's behaviour. The four\n\
         interactive sections (Select/Settings/Editor/Autocomplete) hold a live\n\
         component: any non-reserved key is routed into it, so you can navigate,\n\
         filter, and edit for real.\n\
         \n\
         Always (every section):\n\
         \x20 Tab : next section     BackTab : previous section\n\
         \x20 Ctrl+C / Ctrl+D / q : quit\n\
         \n\
         Display-only sections also accept:\n\
         \x20 Right / l / n : next     Left / h / p : previous\n\
         \x20 1..9 : jump to a section by number\n\
         \x20 d / x : (Toast) dismiss-newest / tick TTL\n\
         \x20 i / k : (Image) scroll image into scrollback / drop viewport image\n\
         \x20 o : toggle the demonstration overlay\n\
         \n\
         Interactive sections route every other key to the live component:\n\
         \x20 Select/Settings : ↑/↓ navigate · Enter/Space select or edit-in-place\n\
         \x20 Editor : type to edit · C-w/C-y kill/yank · alt+enter soft break\n\
         \x20 Autocomplete : type `/` or `@` · ↑/↓ navigate · Tab accept · Esc close\n\
         \n\
         Env seams (for byte-capture probes):\n\
         \x20 HAND_TUI_FORCE_IMAGE_PROTOCOL=kitty|iterm2|fallback : force the image protocol\n\
         \x20 HAND_TUI_GALLERY_FLOOD=1 : jump to Image and storm frames (chunk-mid-flood)\n\
         \x20 HAND_TUI_QUERY_CELL_SIZE=1 : issue CSI 16 t once (fire-and-forget, never blocks)"
    );
}

async fn run() -> Result<(), SessionError> {
    // Establish the guard first so a non-interactive launch leaves the shell
    // untouched (it verifies stdin/stdout are TTYs before toggling raw mode).
    let mut guard = SessionGuard::enter()?;
    let terminal = guard.terminal()?;

    let (init_cols, init_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let state = Arc::new(Mutex::new(GalleryState::new(TerminalSize::new(
        init_cols, init_rows,
    ))));

    let (requester, scheduler) = spawn_scheduler(terminal, state.clone());
    let (mut events, pump) = spawn_event_pump(EVENT_CHANNEL_CAPACITY);

    // Optional auto-flood: jump straight to the Image section and storm frames, so
    // a probe can capture the large image's chunked APC transfer *while frames are
    // flooding* (the flood seam) without synthesizing keypresses. The scheduler
    // coalesces/rate-limits; the point is that a chunked emission at a frame seam
    // never leaves a half-escape.
    let flood_handle = if flood_requested_by_env() {
        // Read the Image index and release the lock *before* jumping — a
        // reentrant `lock(&state)` while the scrutinee's guard is still live would
        // deadlock (std::sync::Mutex is not reentrant).
        let image_index = image_section_index(&lock(&state).sections);
        if let Some(index) = image_index {
            lock(&state).jump(index);
        }
        let flood_requester = requester.clone();
        Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(FLOOD_REQUEST_INTERVAL);
            loop {
                ticker.tick().await;
                flood_requester.request_frame();
            }
        }))
    } else {
        None
    };

    // Initial paint.
    requester.request_frame();

    while let Some(event) = events.recv().await {
        let mut quit = false;
        match event {
            RtInputEvent::Key(key) => {
                if handle_nav_key(&state, &key) {
                    quit = true;
                }
            }
            RtInputEvent::Resize { cols, rows } => {
                // Fold the whole new geometry into the tracked size; the coalesced
                // frame below re-anchors and re-lays-out against it.
                let _ = lock(&state).size.apply_resize(cols, rows);
            }
            RtInputEvent::Paste(_) | RtInputEvent::FocusGained | RtInputEvent::FocusLost => {}
        }

        if quit {
            break;
        }
        requester.request_frame();
    }

    // Stop the flood task, if running, before tearing anything down.
    if let Some(handle) = flood_handle {
        handle.abort();
    }

    // Drop the requester so the scheduler drains its final frame and stops, then
    // wait for it to release the terminal before restoring.
    drop(requester);
    let _ = scheduler.await;
    pump.abort();
    guard.restore();
    Ok(())
}

/// The index of the Image section in the registry, so the auto-flood can jump to
/// it at launch. Kept in sync with `register_sections`.
fn image_section_index(sections: &[Section]) -> Option<usize> {
    sections.iter().position(|s| s.title == "Image")
}

/// Apply a key to the gallery, returning `true` when it is a quit key.
///
/// Quit (`Ctrl+C`/`Ctrl+D`/`q`) and section-switch (`Tab`/`BackTab`) are always
/// gallery-level, on every section, so the tour can advance out of any section.
/// The rest of the routing depends on the active section:
///
/// - **Interactive section** (Select/Settings/Editor/Autocomplete): every other
///   key — arrows, Enter, chars, digits, Esc, Ctrl-chords — is routed straight
///   into the live component so a live walk drives it in real time. Digits route
///   to the component too, so a number can be typed into the editor.
/// - **Display-only section**: the full navigation + section-specific gesture set
///   (`h`/`l`/`n`/`p`, arrows, digit jump, and `d`/`x`/`i`/`k`/`o`).
fn handle_nav_key(state: &Arc<Mutex<GalleryState>>, key: &RtKey) -> bool {
    match key.key_id.as_deref() {
        // Reserved on every section: quit and section switch.
        Some("ctrl+c" | "ctrl+d" | "q") => return true,
        Some("tab") => {
            lock(state).next();
            return false;
        }
        Some("shift+tab") => {
            lock(state).prev();
            return false;
        }
        _ => {}
    }

    // On an interactive section, every non-reserved key belongs to the live
    // component (this is the interactive-routing core: navigation/filter/edit go
    // to the widget, not the tour). Route it and stop.
    if lock(state).active_is_interactive() {
        lock(state).route_to_interactive(key);
        return false;
    }

    // Display-only section: the classic gallery gesture set.
    match key.key_id.as_deref() {
        Some("right" | "l" | "n") => lock(state).next(),
        Some("left" | "h" | "p") => lock(state).prev(),
        // Toast gestures: dismiss the newest toast, or advance its TTLs — both
        // reveal a hidden overflow toast, the capturable seam for VAL-WIDGET-012.
        Some("d") => lock(state).dismiss_toast(),
        Some("x") => lock(state).tick_toast(),
        // Image scrollback gestures (Image section):
        //   i : scroll the sample image into native scrollback (commit once)
        //   k : drop a pure-viewport image (viewport-only delete; a committed
        //       image is a different id and stays protected)
        //   o : toggle the demonstration overlay (must not perturb scrollback)
        Some("i") => lock(state).scroll_image_into_history(),
        Some("k") => lock(state).drop_viewport_image(),
        Some("o") => lock(state).toggle_overlay(),
        Some(digit) if digit.len() == 1 => {
            if let Some(d) = digit.chars().next().and_then(|c| c.to_digit(10))
                && d >= 1
            {
                lock(state).jump((d - 1) as usize);
            }
        }
        _ => {}
    }
    false
}

/// Spawn the frame scheduler over the session terminal. The returned closure is
/// the one and only painter: it snapshots the active section and size under the
/// lock, then draws the bordered gallery frame wrapped in synchronized-output
/// markers via [`draw_synchronized`].
fn spawn_scheduler(
    terminal: SessionTerminal,
    state: Arc<Mutex<GalleryState>>,
) -> (FrameRequester, tokio::task::JoinHandle<io::Result<()>>) {
    // `EraseOnDrop` wipes the inline viewport region when the scheduler task ends
    // (quit or panic) before `guard.restore()`, so the shell prompt lands on a
    // fresh line with no ghost gallery box (VAL-CORE-016/036).
    let mut terminal = EraseOnDrop::new(terminal);
    // The history sink lives on the terminal-owning task (like rt_demo): the only
    // place `insert_before` — and therefore a scrollback image commit — is legal.
    let mut history = HistorySink::new();
    FrameScheduler::spawn(move || {
        // Wrap the whole paint in BSU/ESU so an interrupt mid-draw never leaves an
        // open synchronized block.
        let mut stdout = io::stdout();
        let state = &state;
        // The image queue and the scrollback channel captured once for this frame.
        // Cloning the handles is cheap; they are the same channels the Image
        // section enqueues into and the input gestures queue commits/deletes on.
        let (image_queue, scrollback, commits, deletes) = {
            let mut guard = lock(state);
            (
                guard.image_queue.clone(),
                guard.scrollback.clone(),
                std::mem::take(&mut guard.pending_scrollback),
                std::mem::take(&mut guard.pending_deletes),
            )
        };

        // 1. Commit any queued images into native scrollback *before* the draw —
        //    each becomes one `insert_before` reserving the image's footprint, and
        //    the returned escape is transmitted once under a stable id. A committed
        //    id is marked delete-protected by the channel. Repainting the viewport
        //    afterwards never re-transmits it (bounded transmission, VAL-IMG-005).
        for pending in commits {
            let image = RtImage::new(pending.data).label(pending.label);
            if let Some(emission) =
                history.commit_image(&mut *terminal, &scrollback, &image, pending.protocol)?
            {
                // Write the scrollback image escape once. `insert_before` already
                // reserved and scrolled the blank footprint above the viewport; the
                // graphics image is painted over it out of band. The channel has
                // already marked the id delete-protected.
                stdout.write_all(emission.escape.as_bytes())?;
                stdout.flush()?;
            }
        }

        // 2. Honour any queued viewport-only delete. The channel refuses a
        //    committed (scrollback) id, so this can only ever emit a single-id
        //    `d=I` delete for a pure-viewport image — NEVER a wide `d=A`/`d=a`
        //    that would wipe the scrollback copies (VAL-IMG-009 safety pin).
        for id in deletes {
            if let Some(escape) = scrollback.delete_viewport_image(id) {
                stdout.write_all(escape.as_bytes())?;
                stdout.flush()?;
            }
        }

        draw_synchronized(&mut stdout, |w| {
            // Issue the terminal cell-size query once, fire-and-forget: gated by
            // the `HAND_TUI_QUERY_CELL_SIZE` seam so it is a no-op by default, and
            // it never waits for a reply, so a silent PTY (which never answers)
            // cannot stall the render loop. A reply, if any, is read on the input
            // stream and folded in via `set_cell_dimensions` (see `run`).
            CELL_SIZE_QUERY_ONCE.call_once(|| {
                let _ = write_cell_size_query(w);
            });
            let mut viewport_origin_y = 0u16;
            let mut osc8: Vec<hand_tui::rt::components::PendingEmission> = Vec::new();
            terminal.draw(|frame| {
                viewport_origin_y = frame.area().y;
                osc8 = draw(frame, state);
            })?;
            // Emit any queued graphics escape onto its reserved rows. On a plain /
            // tmux frame the queue is empty (fallback enqueues nothing), so this
            // writes zero bytes — the zero-graphics-bytes guarantee.
            image_queue.flush_to(w, viewport_origin_y)?;
            // Emit real OSC 8 hyperlinks for the Markdown section's links through
            // the same raw channel (a capable terminal only; on tmux/fallback the
            // renderer painted `text (url)` and this list is empty). Save/restore
            // the cursor around the out-of-band writes so the draw's caret survives.
            if !osc8.is_empty() {
                w.write_all(b"\x1b7")?;
                for e in &osc8 {
                    let absolute_row = viewport_origin_y.saturating_add(e.row);
                    write!(w, "\x1b[{};1H", absolute_row.saturating_add(1))?;
                    w.write_all(e.escape.as_bytes())?;
                }
                w.write_all(b"\x1b8")?;
            }
            Ok(())
        })
    })
}

/// Guards the one-shot cell-size query so it is issued exactly once, on the first
/// frame, rather than every draw.
static CELL_SIZE_QUERY_ONCE: std::sync::Once = std::sync::Once::new();

/// Paint one gallery frame: a bordered box holding a tab strip of section titles
/// and the active section's body.
///
/// Returns the OSC 8 hyperlink emissions the Markdown section produced this frame
/// (viewport-local rows), for the scheduler to flush through the raw channel after
/// the draw. Empty for every other section and on a non-OSC-8 terminal.
fn draw(
    frame: &mut Frame,
    state: &Arc<Mutex<GalleryState>>,
) -> Vec<hand_tui::rt::components::PendingEmission> {
    let guard = lock(state);
    let area = frame.area();
    let overlay_open = guard.overlay_open;
    let mut osc8 = Vec::new();
    // The focused interactive component's caret, in viewport coordinates, computed
    // during the buffer-painting scope and applied to the frame after the buffer
    // borrow ends so live typing/navigation shows a real hardware cursor.
    let mut cursor: Option<Position> = None;
    let buf = frame.buffer_mut();
    if area.is_empty() {
        return osc8;
    }

    // Outer bordered block titled with the gallery name and section counter.
    let active = guard.active;
    let count = guard.sections.len();
    let title = format!(" rt gallery — {}/{} ", active + 1, count.max(1));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.is_empty() {
        return osc8;
    }

    // Split the interior into a one-row tab strip and the section body.
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    render_tab_strip(&guard, rows[0], buf);

    let body = rows[1];
    if let Some(section) = guard.sections.get(active) {
        (section.build)(body, buf);
    }

    // Interactive sections: the builder painted only the header on `body`'s first
    // row; render the *live* component (from gallery state) into the band below it
    // so a routed key's effect shows this frame. Its caret is translated into
    // viewport coordinates for the hardware cursor.
    if let Some(interactive) = guard.active_interactive() {
        let body_rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(body);
        let component_area = body_rows[1];
        let component = interactive.as_component();
        component.render(component_area, buf);
        cursor = component
            .cursor()
            .map(|caret| clamp_cursor_into(component_area, caret));
    }

    // The Toast section paints its header via the stateless builder above; overlay
    // the persistent toast stack (from gallery state) just below that header so the
    // dismiss-newest / TTL-tick gestures show up across frames.
    if toast_section_index(&guard.sections) == Some(active) {
        let toast_rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(body);
        guard.toast.render(toast_rows[1], buf);
    }

    // Markdown special-case: compute the real OSC 8 hyperlinks for the section's
    // links. The renderer painted the visible link text into the cells; the URL
    // rides out of band through the raw channel so a capable terminal surfaces a
    // clickable hyperlink (VAL-WIDGET-006). On tmux / fallback the renderer keeps
    // the `text (url)` form in the cells and `links()` returns empty.
    if guard.section_title() == Some("Markdown") {
        let view = MarkdownView::new(MARKDOWN_SOURCE).theme(default_markdown_theme());
        for link in view.links(body.width) {
            // Position at the link's logical row within the body, clamped to the
            // body band so an over-long source never anchors past the viewport.
            let row = body
                .y
                .saturating_add(link.row.min(body.height.saturating_sub(1)));
            osc8.push(osc8_emission(&link.text, &link.url, row));
        }
    }

    // The demonstration overlay: a small bordered box drawn on top of the base
    // view. Toggling it (the `o` gesture) must not perturb any image already in
    // scrollback — it only repaints viewport cells, never re-transmits a committed
    // image nor emits any delete (VAL-CROSS-004).
    if overlay_open {
        render_demo_overlay(inner, buf);
        // The overlay owns the surface while open; hide the interactive caret so it
        // does not stray onto the overlay box.
        cursor = None;
    }

    // Release the state guard, then place the hardware cursor (the buffer borrow
    // ended with the block above). A `None` leaves the cursor hidden.
    drop(guard);
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }

    osc8
}

/// Translate a component-local caret into an absolute viewport position, clamped
/// inside `area` so the hardware cursor can never stray outside the component that
/// owns it. Mirrors `FocusView`'s private clamp: a component reports its caret in
/// its own render-area coordinates, and this offsets it by the area's origin.
fn clamp_cursor_into(area: Rect, caret: Position) -> Position {
    let max_x = area.x.saturating_add(area.width.saturating_sub(1));
    let max_y = area.y.saturating_add(area.height.saturating_sub(1));
    Position::new(
        area.x.saturating_add(caret.x).min(max_x),
        area.y.saturating_add(caret.y).min(max_y),
    )
}

/// Paint the demonstration overlay: a centered bordered box. Deliberately simple
/// (not the full `OverlayStack`) — its only job in the gallery is to be toggled
/// on and off so a probe can confirm an overlay open/close does not disturb a
/// scrollback image.
fn render_demo_overlay(area: Rect, buf: &mut Buffer) {
    let w = area.width.clamp(4, 30);
    let h = area.height.clamp(3, 5);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" overlay ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(rect);
    ratatui::widgets::Clear.render(rect, buf);
    block.render(rect, buf);
    if !inner.is_empty() {
        buf.set_stringn(
            inner.x,
            inner.y,
            "overlay open (o toggles)",
            inner.width as usize,
            Style::default().fg(Color::Yellow),
        );
    }
}

/// Paint the horizontal strip of section titles, highlighting the active one.
fn render_tab_strip(guard: &GalleryState, area: Rect, buf: &mut Buffer) {
    let mut x = area.x;
    let end = area.x + area.width;
    for (i, section) in guard.sections.iter().enumerate() {
        if x >= end {
            break;
        }
        let label = format!(" {}:{} ", i + 1, section.title);
        let style = if i == guard.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let remaining = (end - x) as usize;
        let (next_x, _) = buf.set_stringn(x, area.y, &label, remaining, style);
        x = next_x;
    }
}

/// Lock the gallery state, treating poisoning as fatal (a panic already tore
/// through the gallery).
fn lock(state: &Arc<Mutex<GalleryState>>) -> std::sync::MutexGuard<'_, GalleryState> {
    state.lock().expect("gallery state mutex poisoned")
}
