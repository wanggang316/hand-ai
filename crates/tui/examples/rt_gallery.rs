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
//! # Navigation
//!
//! - `Tab` / `Right` / `l` / `n` : next section (wraps)
//! - `BackTab` / `Left` / `h` / `p` : previous section (wraps)
//! - `1`..=`9` : jump directly to a section by number
//! - `d` : (Toast section) dismiss the newest toast — reveals a hidden overflow
//! - `x` : (Toast section) advance toast TTLs — an expiring toast reveals the next
//! - `Ctrl+C` / `Ctrl+D` / `q` : quit cleanly (terminal fully restored)
//!
//! Run it:
//!   cargo run -p hand-tui --example rt_gallery
//!
//! On a non-TTY (piped) stdin/stdout it prints a diagnostic and exits non-zero
//! without ever touching the parent shell's terminal mode.

use std::io;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use hand_tui::rt::components::{
    BorderTint, CancellableLoader, CombinedProvider, Editor, Loader, MarkdownView, PathEntry,
    PathProvider, ProgressBar, SelectItem, SelectList, SelectListLayout, SettingEntry,
    SettingValue, SettingsList, SlashCommand, SlashProvider, Spacer, StatusBar, TextBlock, Toast,
    ToastLevel, TruncatedText, WidgetBox, default_markdown_theme,
};
use hand_tui::rt::events::{RtInputEvent, RtKey, spawn_event_pump};
use hand_tui::rt::scheduler::{FrameRequester, FrameScheduler, draw_synchronized};
use hand_tui::rt::session::{EraseOnDrop, SessionError, SessionGuard, SessionTerminal};
use hand_tui::rt::view::{RtComponent, TerminalSize};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};

/// Bound on the input event channel: interactive navigation is low-rate, so a
/// small buffer is plenty.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// A section body painter: given the section's content rect and the frame buffer,
/// it lays out that section's widgets. Boxed and `Send` so it can live in the
/// shared [`GalleryState`] the scheduler's draw task reads.
type SectionBuilder = Box<dyn Fn(Rect, &mut Buffer) + Send>;

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

/// The gallery's mutable state, shared between the input loop (which mutates the
/// active index and tracked size) and the scheduler's draw closure (which reads
/// them). A plain `std::sync::Mutex`: every critical section is a tiny field
/// access, and the scheduler's draw closure is synchronous.
struct GalleryState {
    /// The registered sections, in navigation order.
    sections: Vec<Section>,
    /// Index of the currently shown section.
    active: usize,
    /// Tracked terminal geometry, overwritten whole on each resize event.
    size: TerminalSize,
    /// Persistent toast stack for the Toast section. Held in state (not rebuilt
    /// per frame) so the dismiss-newest / TTL-tick gestures mutate it across
    /// frames, making the overflow-hidden-then-reappears behaviour observable in a
    /// capture (VAL-WIDGET-012). See [`toast_seam`].
    toast: Toast,
}

impl GalleryState {
    fn new(size: TerminalSize) -> Self {
        Self {
            sections: register_sections(),
            active: 0,
            size,
            toast: toast_seam(),
        }
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
}

/// Register the sections for this milestone: the six primitive widgets. Later M2
/// features append their own sections here (or via a follow-on registration
/// function) without touching navigation or draw.
fn register_sections() -> Vec<Section> {
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
            let source = "\
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
            // Wire the real keyword-driven highlighter into the fenced code
            // block so keyword/string/number/comment/type render in distinct
            // colors (VAL-WIDGET-004).
            MarkdownView::new(source)
                .theme(default_markdown_theme())
                .render(area, buf);
        }),
        Section::new("Select", |area, buf| {
            // A two-column select list: labels in a padded primary column, dimmed
            // descriptions in the second, the selected row a reversed highlight
            // bar with a `▸` indicator. A window of 4 over 6 items shows the
            // `(n/total)` counter; the description on "sonnet" is deliberately
            // long so it truncates with an ellipsis on a narrow terminal.
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            TruncatedText::new(
                "SelectList — ▸ selected row, dimmed description column, wrap navigation",
            )
            .render(rows[0], buf);
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
            list.handle_key(&RtKey {
                key_id: Some("down".to_string()),
                raw: crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ),
            });
            list.render(rows[1], buf);
        }),
        Section::new("Settings", |area, buf| {
            // A settings list exercising every value type plus the full chrome:
            // an enum, a bool, a number, and a string, with the focused entry's
            // description below the list and the footer hint. The number entry is
            // shown mid-edit (inline caret) to demonstrate edit-in-place.
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
            TruncatedText::new(
                "SettingsList — Enter/Space edits in place; selection clamps (no wrap)",
            )
            .render(rows[0], buf);
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
            // edit-in-place value render.
            list.handle_key(&RtKey {
                key_id: Some("down".to_string()),
                raw: crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ),
            });
            list.handle_key(&RtKey {
                key_id: Some("down".to_string()),
                raw: crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Down,
                    crossterm::event::KeyModifiers::NONE,
                ),
            });
            list.handle_key(&RtKey {
                key_id: Some("enter".to_string()),
                raw: crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::NONE,
                ),
            });
            list.render(rows[1], buf);
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
            // focus/thinking border tint. Sections are stateless painters, so this
            // drives a canned key sequence into a fresh editor each frame to *show*
            // the core — real interactivity is covered by `tests/rt_editor.rs` and
            // the lib unit tests. (The chat-input's borderless Horizontal style with
            // no indicator is a later-milestone concern; the gallery demonstrates the
            // box variant.)
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Min(0),    // editor box
            ])
            .split(area);
            TruncatedText::new(
                "Editor — grapheme-aware · auto-grow · kill-ring (C-w/C-y) · undo · paste markers/defuse",
            )
            .render(rows[0], buf);

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
            editor.render(rows[1], buf);
        }),
        Section::new("Autocomplete", |area, buf| {
            // The editor with an autocomplete provider installed: a `/` at the
            // start of the line opens the slash-command popup, an `@` opens the
            // path popup. Sections are stateless painters, so this drives a canned
            // `/h` into a fresh editor each frame to *show* the popup — Up/Down
            // navigate, Tab accepts (the only accept gesture), Esc closes. Real
            // interactivity is covered by `tests/rt_autocomplete.rs`.
            let rows = Layout::vertical([
                Constraint::Length(1), // header
                Constraint::Min(0),    // editor box + popup
            ])
            .split(area);
            TruncatedText::new(
                "Autocomplete — `/` slash + `@` path providers · ↑↓ navigate · Tab accept · Esc close",
            )
            .render(rows[0], buf);

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
            editor.render(rows[1], buf);
        }),
    ]
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
        "rt_gallery — inline tour of the rt primitive widgets\n\
         \n\
         Inline viewport (no alternate screen); prior shell content stays visible.\n\
         Navigate the sections; each shows one primitive's behaviour.\n\
         \n\
         Keys:\n\
         \x20 Tab / Right / l / n : next section\n\
         \x20 BackTab / Left / h / p : previous section\n\
         \x20 1..9 : jump to a section by number\n\
         \x20 d / x : (Toast section) dismiss-newest / tick TTL\n\
         \x20 Ctrl+C / Ctrl+D / q : quit"
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

    // Drop the requester so the scheduler drains its final frame and stops, then
    // wait for it to release the terminal before restoring.
    drop(requester);
    let _ = scheduler.await;
    pump.abort();
    guard.restore();
    Ok(())
}

/// Apply a navigation key to the gallery, returning `true` when it is a quit key.
fn handle_nav_key(state: &Arc<Mutex<GalleryState>>, key: &RtKey) -> bool {
    match key.key_id.as_deref() {
        Some("ctrl+c" | "ctrl+d" | "q") => return true,
        Some("tab" | "right" | "l" | "n") => lock(state).next(),
        Some("shift+tab" | "left" | "h" | "p") => lock(state).prev(),
        // Toast gestures: dismiss the newest toast, or advance its TTLs — both
        // reveal a hidden overflow toast, the capturable seam for VAL-WIDGET-012.
        Some("d") => lock(state).dismiss_toast(),
        Some("x") => lock(state).tick_toast(),
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
    FrameScheduler::spawn(move || {
        // Wrap the whole paint in BSU/ESU so an interrupt mid-draw never leaves an
        // open synchronized block.
        let mut stdout = io::stdout();
        let state = &state;
        draw_synchronized(&mut stdout, |_w| {
            terminal.draw(|frame| draw(frame, state))?;
            Ok(())
        })
    })
}

/// Paint one gallery frame: a bordered box holding a tab strip of section titles
/// and the active section's body.
fn draw(frame: &mut Frame, state: &Arc<Mutex<GalleryState>>) {
    let guard = lock(state);
    let area = frame.area();
    let buf = frame.buffer_mut();
    if area.is_empty() {
        return;
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
        return;
    }

    // Split the interior into a one-row tab strip and the section body.
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    render_tab_strip(&guard, rows[0], buf);

    let body = rows[1];
    if let Some(section) = guard.sections.get(active) {
        (section.build)(body, buf);
    }

    // The Toast section paints its header via the stateless builder above; overlay
    // the persistent toast stack (from gallery state) just below that header so the
    // dismiss-newest / TTL-tick gestures show up across frames.
    if toast_section_index(&guard.sections) == Some(active) {
        let toast_rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(body);
        guard.toast.render(toast_rows[1], buf);
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
