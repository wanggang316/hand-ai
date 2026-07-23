//! Gallery tour + interactive-routing smoke tests (VAL-WIDGET-016).
//!
//! The `rt_gallery` example is the M2 milestone's live-walk scaffold: a tour of
//! every rt widget, with the four interactive sections (Select/Settings/Editor/
//! Autocomplete) routing keys into a *live* component so a tmux user-test can
//! navigate, filter, and edit for real. The example's own `draw`/registry are
//! private to the binary, so these tests pin the two behaviours the feature must
//! make testable, at the library level the example is assembled from:
//!
//! 1. **Tour completeness** — every section renders its signature text into a
//!    gallery-sized buffer (the tour visits each section and each is visible).
//! 2. **Interactive routing** — a live component, seeded the way the gallery seeds
//!    it, mutates in response to a routed key (the live-walk enabler): a select
//!    list navigates, a settings list moves + edits, the editor types, and the
//!    autocomplete popup filters and navigates.
//!
//! Rendering correctness is asserted over `TestBackend` (never `tmux
//! capture-pane`), per `docs/architecture.md`'s multiplexer note.

use std::sync::Arc;

use hand_tui::rt::components::{
    CombinedProvider, Editor, MarkdownView, PathEntry, PathProvider, ProgressBar, SelectItem,
    SelectList, SelectListLayout, SettingEntry, SettingValue, SettingsList, SlashCommand,
    SlashProvider, StatusBar, TextBlock, default_markdown_theme,
};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::{HandleOutcome, RtComponent};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::{TerminalOptions, Viewport};

// --- key helpers (mirror the gallery's dispatcher ids) ----------------------

fn key(id: &str, code: KeyCode, mods: KeyModifiers) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: KeyEvent::new(code, mods),
    }
}

fn ch(c: char) -> RtKey {
    key(&c.to_string(), KeyCode::Char(c), KeyModifiers::NONE)
}

fn down() -> RtKey {
    key("down", KeyCode::Down, KeyModifiers::NONE)
}

fn enter() -> RtKey {
    key("enter", KeyCode::Enter, KeyModifiers::NONE)
}

/// Type each char of `s` as its own key press, routing spaces through the `space`
/// id exactly as the real dispatcher (and the gallery) does.
fn type_str(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        let k = if c == ' ' {
            key("space", KeyCode::Char(' '), KeyModifiers::NONE)
        } else {
            ch(c)
        };
        ed.handle_key(&k);
    }
}

// --- render helper ----------------------------------------------------------

/// Paint `f` into a fixed inline `TestBackend` of `cols`×`rows` and return the
/// painted rows as trimmed strings — the same scrollback-free surface the gallery
/// draws into, and the only surface `docs/architecture.md` allows resize/render
/// assertions on.
fn render_rows(cols: u16, rows: u16, f: impl FnOnce(&mut Buffer)) -> Vec<String> {
    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)),
        },
    )
    .expect("build inline test terminal");
    terminal
        .draw(|frame| f(frame.buffer_mut()))
        .expect("draw gallery section");
    let buffer = terminal.backend().buffer().clone();
    (0..rows)
        .map(|y| {
            let mut s = String::new();
            for x in 0..cols {
                s.push_str(buffer[(x, y)].symbol());
            }
            s.trim_end().to_string()
        })
        .collect()
}

/// Whether any painted row contains `needle`.
fn any_row_contains(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|r| r.contains(needle))
}

// --- gallery-parallel constructors ------------------------------------------
//
// These mirror the example's `build_interactive` seeding so the tests exercise
// the same starting state the live walk begins from. Kept minimal — the fine
// grain of each widget is pinned by its own suite (rt_lists / rt_editor /
// rt_autocomplete); here they stand in for "the section the gallery shows".

fn gallery_select_list() -> SelectList {
    let mut list = SelectList::new(vec![
        SelectItem::new("opus", "Claude Opus").with_description("most capable, slowest"),
        SelectItem::new("sonnet", "Claude Sonnet").with_description("balanced quality and speed"),
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
    // Seed: focus the second item, as the gallery does.
    list.handle_key(&down());
    list
}

fn gallery_settings_list() -> SettingsList {
    SettingsList::new(vec![
        SettingEntry::new(
            "theme",
            SettingValue::Enum {
                choices: vec!["dark".into(), "light".into(), "auto".into()],
                selected: 0,
            },
            "Color theme — Enter cycles through the choices",
        ),
        SettingEntry::new("auto_save", SettingValue::Bool(true), "flips instantly"),
        SettingEntry::new("max_tokens", SettingValue::Number(4096.0), "edited inline"),
        SettingEntry::new(
            "model",
            SettingValue::String("claude-sonnet".into()),
            "free-text string",
        ),
    ])
    .show_description(true)
    .show_hint(true)
}

fn gallery_autocomplete_editor() -> Editor {
    let mut ed = Editor::new();
    ed.set_autocomplete_provider(Arc::new(CombinedProvider::new(vec![
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
    ed
}

// --- 1. Tour completeness: every section renders signature text -------------

#[test]
fn tour_visits_display_sections_and_each_renders_signature_text() {
    // A gallery-sized body band. Display-only widgets paint their signature text.
    let area = Rect::new(0, 0, 80, 6);

    let text_rows = render_rows(80, 6, |buf| {
        TextBlock::new("TextBlock word-wraps its content to the inner width").render(area, buf);
    });
    assert!(
        any_row_contains(&text_rows, "word-wraps"),
        "Text section renders: {text_rows:?}"
    );

    let status_rows = render_rows(80, 1, |buf| {
        StatusBar::new()
            .left("Model: gpt")
            .center("rt gallery")
            .right("Session: 42")
            .render(Rect::new(0, 0, 80, 1), buf);
    });
    assert!(
        any_row_contains(&status_rows, "rt gallery"),
        "StatusBar section renders: {status_rows:?}"
    );

    let progress_rows = render_rows(80, 1, |buf| {
        ProgressBar::new()
            .ratio(0.25)
            .label("25%")
            .render(Rect::new(0, 0, 80, 1), buf);
    });
    assert!(
        any_row_contains(&progress_rows, "25%"),
        "Progress section renders: {progress_rows:?}"
    );

    let md_rows = render_rows(80, 12, |buf| {
        MarkdownView::new("# Markdown renderer\n\nBody text with **bold**.")
            .theme(default_markdown_theme())
            .render(Rect::new(0, 0, 80, 12), buf);
    });
    assert!(
        any_row_contains(&md_rows, "Markdown renderer"),
        "Markdown section renders: {md_rows:?}"
    );
}

#[test]
fn tour_visits_interactive_sections_and_each_renders_signature_text() {
    // The four interactive sections render their live component's signature shape
    // even before any key is routed (the seed is the tour's visible content).
    let select_rows = render_rows(80, 8, |buf| {
        gallery_select_list().render(Rect::new(0, 0, 80, 8), buf);
    });
    assert!(
        any_row_contains(&select_rows, "Claude Opus"),
        "Select section renders items: {select_rows:?}"
    );

    let settings_rows = render_rows(80, 10, |buf| {
        gallery_settings_list().render(Rect::new(0, 0, 80, 10), buf);
    });
    assert!(
        any_row_contains(&settings_rows, "theme"),
        "Settings section renders entries: {settings_rows:?}"
    );

    let mut editor = Editor::new();
    type_str(&mut editor, "review the draft");
    let editor_rows = render_rows(80, 8, |buf| {
        editor.render(Rect::new(0, 0, 80, 8), buf);
    });
    assert!(
        any_row_contains(&editor_rows, "review the draft"),
        "Editor section renders its draft: {editor_rows:?}"
    );

    let mut auto = gallery_autocomplete_editor();
    type_str(&mut auto, "/h");
    let auto_rows = render_rows(80, 8, |buf| {
        auto.render(Rect::new(0, 0, 80, 8), buf);
    });
    assert!(
        any_row_contains(&auto_rows, "/help") || any_row_contains(&auto_rows, "help"),
        "Autocomplete section renders the popup: {auto_rows:?}"
    );
}

// --- 2. Interactive routing: a routed key mutates the live component --------

#[test]
fn routing_a_key_navigates_the_select_list() {
    let mut list = gallery_select_list();
    let before = list.selected_index();
    // The gallery routes a bare `down` into the active component (not navigation).
    let outcome = list.handle_key(&down());
    assert_eq!(outcome, HandleOutcome::Consumed, "list consumes down");
    assert_eq!(
        list.selected_index(),
        before + 1,
        "routed down advances the selection (live walk)"
    );
}

#[test]
fn routing_a_key_moves_and_edits_the_settings_list() {
    let mut list = gallery_settings_list();
    let before = list.selected_index();
    assert_eq!(
        list.handle_key(&down()),
        HandleOutcome::Consumed,
        "settings consumes down"
    );
    assert_eq!(
        list.selected_index(),
        before + 1,
        "routed down moves the settings selection"
    );
    // Enter on the bool entry (index 1) flips it in place — a live edit.
    assert_eq!(
        list.handle_key(&enter()),
        HandleOutcome::Consumed,
        "Enter edits the focused setting in place"
    );
}

#[test]
fn routing_keys_types_into_the_editor_live() {
    let mut editor = Editor::new();
    assert!(editor.text().is_empty(), "editor starts empty");
    for kc in "hi".chars() {
        assert_eq!(
            editor.handle_key(&ch(kc)),
            HandleOutcome::Consumed,
            "editor consumes a printable"
        );
    }
    assert_eq!(editor.text(), "hi", "routed chars land in the buffer live");
    // A soft break grows the editor without submitting (auto-grow live).
    editor.handle_key(&key("alt+enter", KeyCode::Enter, KeyModifiers::ALT));
    assert_eq!(editor.line_count(), 2, "alt+enter inserts a soft break");
}

#[test]
fn routing_keys_filters_and_navigates_the_autocomplete_popup() {
    let mut ed = gallery_autocomplete_editor();
    assert!(!ed.autocomplete_visible(), "closed before any trigger");

    // Typing `/h` opens and filters the popup to the `h*` slash commands — the
    // live filter the tmux walk drives.
    type_str(&mut ed, "/h");
    assert!(ed.autocomplete_visible(), "slash at line start opens popup");
    let labels: Vec<String> = ed
        .autocomplete()
        .items()
        .iter()
        .map(|i| i.label.clone())
        .collect();
    assert_eq!(
        labels.len(),
        2,
        "popup filtered to two `/h*` commands: {labels:?}"
    );
    assert!(
        labels[0].starts_with("/help"),
        "first candidate is /help: {labels:?}"
    );
    assert!(
        labels[1].starts_with("/history"),
        "second candidate is /history: {labels:?}"
    );

    // Down navigates the popup (consumed by the popup, never the buffer).
    assert_eq!(
        ed.handle_key(&down()),
        HandleOutcome::Consumed,
        "popup consumes down while visible"
    );
    assert_eq!(
        ed.text(),
        "/h",
        "popup navigation does not mutate the buffer"
    );
}
