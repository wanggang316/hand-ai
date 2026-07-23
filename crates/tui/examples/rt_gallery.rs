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
    MarkdownView, ProgressBar, Spacer, StatusBar, TextBlock, TruncatedText, WidgetBox,
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
}

impl GalleryState {
    fn new(size: TerminalSize) -> Self {
        Self {
            sections: register_sections(),
            active: 0,
            size,
        }
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
    println!(\"hello\");
}
```

| name | 值 |
|:-----|---:|
| 你好世界 | 1 |
| ascii | 22 |

~~struck through~~ text.";
            MarkdownView::new(source).render(area, buf);
        }),
    ]
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

    if let Some(section) = guard.sections.get(active) {
        (section.build)(rows[1], buf);
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
