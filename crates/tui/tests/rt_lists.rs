//! Rendering + interaction tests for the rt list widgets
//! (`hand_tui::rt::components::{SelectList, SettingsList}`).
//!
//! Both widgets paint styled [`Line`]s into a ratatui `Buffer` — the same model
//! the rt scheduler draws every frame — and consume structured [`RtKey`]s. These
//! tests drive the *behavioural signatures* the external validator probes, read
//! from the painted cell grid and from the public accessors, at a fixed geometry
//! plus a narrow (truncation) geometry.
//!
//! The load-bearing contrast pinned here (Decision Log):
//!
//! - **`SelectList` wraps** at the ends — Up from the first lands on the last,
//!   Down from the last lands on the first, and PageUp/PageDown wrap the same way
//!   (not clamp).
//! - **`SettingsList` clamps** at the ends — Up on the first and Down on the last
//!   are no-ops. The two lists disagree on end behaviour on purpose.
//!
//! Assertions traced to the plan's validation-contract:
//! - **VAL-WIDGET-007** — SelectList navigation: wrap + window counter + jump/page.
//! - **VAL-WIDGET-008** — SelectList filter: case-insensitive prefix / restore /
//!   zero-match hint.
//! - **VAL-WIDGET-009** — SelectList two-column layout with ellipsis truncation.
//! - **VAL-WIDGET-010** — SettingsList edit semantics (bool/enum/string/number).
//! - **VAL-WIDGET-015** — empty states for both lists.
//! - **VAL-WIDGET-021** — SettingsList chrome (counter/description/hint) and the
//!   first/last clamp (no wrap).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hand_tui::rt::components::{
    SelectItem, SelectList, SelectListLayout, SelectOutcome, SettingEntry, SettingValue,
    SettingsList,
};
use hand_tui::rt::events::RtKey;
use hand_tui::rt::view::RtComponent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

// --- key helpers -------------------------------------------------------------

/// A named key (e.g. `"up"`, `"pageDown"`) with no modifiers.
fn named(id: &str, code: KeyCode) -> RtKey {
    RtKey {
        key_id: Some(id.to_string()),
        raw: KeyEvent::new(code, KeyModifiers::NONE),
    }
}

/// A bare printable character key.
fn chr(c: char) -> RtKey {
    RtKey {
        key_id: Some(c.to_string()),
        raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
    }
}

/// A ctrl-chorded character key (e.g. `ctrl+j`).
fn ctrl_char(c: char) -> RtKey {
    RtKey {
        key_id: Some(format!("ctrl+{c}")),
        raw: KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
    }
}

// --- render helpers ----------------------------------------------------------

/// Render a component into a fresh buffer of the given size.
fn render<C: RtComponent>(comp: &C, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    comp.render(area, &mut buf);
    buf
}

/// The symbols of one buffer row concatenated, trailing blanks trimmed.
fn row_string(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    let mut row = String::new();
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, y)) {
            row.push_str(cell.symbol());
        }
    }
    row.trim_end().to_string()
}

/// Every row of the buffer as a trimmed string.
fn all_rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.y..area.y + area.height)
        .map(|y| row_string(buf, y))
        .collect()
}

/// Whether any cell on row `y` carries the reversed (highlight) modifier.
fn row_is_reversed(buf: &Buffer, y: u16) -> bool {
    let area = buf.area;
    (area.x..area.x + area.width).any(|x| {
        buf.cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::REVERSED))
    })
}

// --- SelectList fixtures -----------------------------------------------------

fn select_items() -> Vec<SelectItem> {
    vec![
        SelectItem::new("alpha", "Alpha").with_description("first letter"),
        SelectItem::new("beta", "Beta").with_description("second letter"),
        SelectItem::new("gamma", "Gamma"),
        SelectItem::new("delta", "Delta"),
        SelectItem::new("epsilon", "Epsilon"),
    ]
}

// =============================================================================
// VAL-WIDGET-007 — SelectList navigation: wrap, window counter, jump, page
// =============================================================================

#[test]
fn select_down_wraps_at_end() {
    let mut list = SelectList::new(select_items());
    for _ in 0..4 {
        list.handle_key(&named("down", KeyCode::Down));
    }
    assert_eq!(list.selected_index(), 4);
    // Down at the last item wraps to the first — the load-bearing contrast with
    // SettingsList's clamp.
    list.handle_key(&named("down", KeyCode::Down));
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn select_up_wraps_at_start() {
    let mut list = SelectList::new(select_items());
    list.handle_key(&named("up", KeyCode::Up));
    assert_eq!(list.selected_index(), 4, "up at first wraps to last");
}

#[test]
fn select_ctrl_jk_aliases_navigate() {
    let mut list = SelectList::new(select_items());
    list.handle_key(&ctrl_char('j'));
    list.handle_key(&ctrl_char('j'));
    assert_eq!(list.selected_index(), 2);
    list.handle_key(&ctrl_char('k'));
    assert_eq!(list.selected_index(), 1);
}

#[test]
fn select_home_end_jump_without_wrap() {
    let mut list = SelectList::new(select_items());
    list.handle_key(&named("end", KeyCode::End));
    assert_eq!(list.selected_index(), 4);
    list.handle_key(&named("home", KeyCode::Home));
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn select_pageup_pagedown_wrap_like_step() {
    // Window of 2 over 5 items: a page is 2 rows and wraps like Up/Down (not
    // clamp) — pin the deliberate wrap semantics for the page keys.
    let mut list = SelectList::new(select_items()).visible_count(2);
    list.handle_key(&named("pageDown", KeyCode::PageDown)); // 0 -> 2
    assert_eq!(list.selected_index(), 2);
    list.handle_key(&named("pageDown", KeyCode::PageDown)); // 2 -> 4
    assert_eq!(list.selected_index(), 4);
    list.handle_key(&named("pageDown", KeyCode::PageDown)); // 4 -> wrap to 1
    assert_eq!(list.selected_index(), 1);
    list.handle_key(&named("pageUp", KeyCode::PageUp)); // 1 -> wrap to 4
    assert_eq!(list.selected_index(), 4);
}

#[test]
fn select_window_scrolls_and_shows_counter() {
    // Window of 3 over 5 items: only 3 item rows show, plus a `(n/total)` counter.
    let mut list = SelectList::new(select_items()).visible_count(3);
    let buf = render(&list, 60, 8);
    let rows = all_rows(&buf);
    // First 3 items visible, gamma..epsilon not yet.
    assert!(rows.iter().any(|r| r.contains("Alpha")));
    assert!(rows.iter().any(|r| r.contains("Gamma")));
    assert!(!rows.iter().any(|r| r.contains("Epsilon")));
    // Counter line reflects the current selection over the total.
    assert!(
        rows.iter().any(|r| r.contains("(1/5)")),
        "counter missing: {rows:?}"
    );

    // Move the selection to the last item; the window scrolls to keep it visible.
    list.handle_key(&named("end", KeyCode::End));
    let rows = all_rows(&render(&list, 60, 8));
    assert!(rows.iter().any(|r| r.contains("Epsilon")));
    assert!(!rows.iter().any(|r| r.contains("Alpha")));
    assert!(rows.iter().any(|r| r.contains("(5/5)")), "{rows:?}");
}

#[test]
fn select_selected_row_is_highlighted() {
    let list = SelectList::new(select_items());
    let buf = render(&list, 60, 8);
    // Row 0 (the first, selected) is reversed; row 1 is not.
    assert!(row_is_reversed(&buf, 0), "selected row must be reversed");
    assert!(
        !row_is_reversed(&buf, 1),
        "unselected row must not be reversed"
    );
    // The indicator glyph marks the selected row.
    assert!(
        row_string(&buf, 0).starts_with("▸"),
        "{:?}",
        row_string(&buf, 0)
    );
}

#[test]
fn select_enter_and_escape_latch_outcome() {
    let mut list = SelectList::new(select_items());
    list.handle_key(&named("down", KeyCode::Down)); // select index 1 (beta)
    list.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(list.take_outcome(), Some(SelectOutcome::Selected(1)));
    // The latch is one-shot: a second poll is empty.
    assert_eq!(list.take_outcome(), None);

    list.handle_key(&named("escape", KeyCode::Esc));
    assert_eq!(list.take_outcome(), Some(SelectOutcome::Cancelled));
}

// =============================================================================
// VAL-WIDGET-008 — SelectList filter: prefix, case-insensitive, restore, zero
// =============================================================================

#[test]
fn select_filter_is_case_insensitive_prefix() {
    let mut list = SelectList::new(select_items());
    list.set_filter("AL");
    assert_eq!(list.filtered_len(), 1);
    assert_eq!(list.selected_item().unwrap().value, "alpha");
}

#[test]
fn select_filter_is_prefix_not_substring() {
    let mut list = SelectList::new(select_items());
    // "psilon" is a substring of "epsilon" but not a prefix → no match.
    list.set_filter("psilon");
    assert_eq!(list.filtered_len(), 0);
    // A real prefix of "epsilon" matches.
    list.set_filter("ep");
    assert_eq!(list.filtered_len(), 1);
    assert_eq!(list.selected_item().unwrap().value, "epsilon");
}

#[test]
fn select_filter_resets_selection_to_first_match() {
    let mut list = SelectList::new(select_items());
    list.handle_key(&named("end", KeyCode::End)); // move selection off the top
    assert_eq!(list.selected_index(), 4);
    list.set_filter("delta");
    // Selection resets to the first surviving match.
    assert_eq!(list.selected_index(), 0);
    assert_eq!(list.selected_item().unwrap().value, "delta");
}

#[test]
fn select_empty_filter_restores_full_list() {
    let mut list = SelectList::new(select_items());
    list.set_filter("a");
    assert_eq!(list.filtered_len(), 1);
    list.set_filter("");
    assert_eq!(list.filtered_len(), 5);
}

#[test]
fn select_zero_match_renders_hint() {
    let mut list = SelectList::new(select_items());
    list.set_filter("zzz");
    let rows = all_rows(&render(&list, 60, 8));
    assert!(
        rows.iter().any(|r| r.contains("No matching items")),
        "no-match hint missing: {rows:?}"
    );
}

// =============================================================================
// VAL-WIDGET-009 — SelectList two-column layout with ellipsis truncation
// =============================================================================

#[test]
fn select_description_column_dimmed_and_padded() {
    // A wide layout so the label column is padded and the description follows it.
    let list = SelectList::new(select_items()).layout(SelectListLayout {
        min_primary_column_width: Some(16),
        max_primary_column_width: Some(16),
    });
    let buf = render(&list, 80, 8);
    let row0 = row_string(&buf, 0);
    // The description appears in the same row as the label, after padding.
    assert!(row0.contains("Alpha"), "{row0}");
    assert!(row0.contains("first letter"), "{row0}");
    // Label sits in a fixed-width column, so the description starts well past it.
    let alpha_at = row0.find("Alpha").unwrap();
    let desc_at = row0.find("first letter").unwrap();
    assert!(
        desc_at > alpha_at + 8,
        "description column not padded: {row0}"
    );
}

#[test]
fn select_long_description_truncates_with_ellipsis() {
    let items =
        vec![SelectItem::new("k", "Key").with_description(
            "a description far too long to fit in a very narrow terminal window",
        )];
    let list = SelectList::new(items);
    // Narrow area forces the description column to clip.
    let row0 = row_string(&render(&list, 30, 4), 0);
    assert!(
        row0.contains('…'),
        "narrow description must ellipsize: {row0}"
    );
    // The single-line invariant holds: nothing spills to row 1.
    assert_eq!(row_string(&render(&list, 30, 4), 1), "");
}

#[test]
fn select_description_newlines_flattened() {
    let items = vec![SelectItem::new("k", "Key").with_description("line one\nline two")];
    let list = SelectList::new(items);
    let row0 = row_string(&render(&list, 80, 4), 0);
    // Newline collapses to a space; both fragments stay on one row.
    assert!(row0.contains("line one line two"), "{row0}");
}

// =============================================================================
// VAL-WIDGET-015 — SelectList empty state
// =============================================================================

#[test]
fn select_empty_list_renders_no_items() {
    let list = SelectList::new(vec![]);
    let rows = all_rows(&render(&list, 40, 4));
    assert!(
        rows.iter().any(|r| r.contains("(no items)")),
        "empty hint missing: {rows:?}"
    );
}

// --- SettingsList fixtures ---------------------------------------------------

fn setting_entries() -> Vec<SettingEntry> {
    vec![
        SettingEntry::new(
            "theme",
            SettingValue::Enum {
                choices: vec!["dark".into(), "light".into(), "auto".into()],
                selected: 0,
            },
            "Color theme used across the UI",
        ),
        SettingEntry::new(
            "auto_save",
            SettingValue::Bool(true),
            "Save on every change",
        ),
        SettingEntry::new("max_tokens", SettingValue::Number(4096.0), "Token budget"),
        SettingEntry::new(
            "model",
            SettingValue::String("gpt".into()),
            "Default model name",
        ),
    ]
}

// =============================================================================
// VAL-WIDGET-021 — SettingsList clamp (no wrap) + chrome lines
// =============================================================================

#[test]
fn settings_navigation_clamps_at_first() {
    let mut list = SettingsList::new(setting_entries());
    // Up on the first entry is a no-op (clamp, not wrap) — the deliberate
    // contrast with SelectList.
    list.handle_key(&named("up", KeyCode::Up));
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn settings_navigation_clamps_at_last() {
    let mut list = SettingsList::new(setting_entries());
    for _ in 0..10 {
        list.handle_key(&named("down", KeyCode::Down));
    }
    // Down past the last entry stops on the last (clamp, not wrap).
    assert_eq!(list.selected_index(), 3);
}

#[test]
fn settings_ctrl_jk_aliases_navigate() {
    let mut list = SettingsList::new(setting_entries());
    list.handle_key(&ctrl_char('j'));
    assert_eq!(list.selected_index(), 1);
    list.handle_key(&ctrl_char('k'));
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn settings_window_counter_shown_when_overflowing() {
    let list = SettingsList::new(setting_entries()).max_visible(2);
    let rows = all_rows(&render(&list, 60, 10));
    // Two entry rows plus the counter.
    assert!(rows.iter().any(|r| r.contains("theme")));
    assert!(rows.iter().any(|r| r.contains("auto_save")));
    assert!(!rows.iter().any(|r| r.contains("model")));
    assert!(rows.iter().any(|r| r.contains("(1/4)")), "{rows:?}");
}

#[test]
fn settings_description_follows_selection() {
    let mut list = SettingsList::new(setting_entries()).show_description(true);
    let rows = all_rows(&render(&list, 60, 12));
    assert!(
        rows.iter().any(|r| r.contains("Color theme used")),
        "focused description missing: {rows:?}"
    );
    // Move down; the description tracks the new selection.
    list.handle_key(&named("down", KeyCode::Down));
    let rows = all_rows(&render(&list, 60, 12));
    assert!(
        rows.iter().any(|r| r.contains("Save on every change")),
        "description did not follow selection: {rows:?}"
    );
}

#[test]
fn settings_hint_footer_shown_when_enabled() {
    let list = SettingsList::new(setting_entries()).show_hint(true);
    let rows = all_rows(&render(&list, 60, 12));
    assert!(
        rows.iter()
            .any(|r| r.contains("Enter/Space to change") && r.contains("Esc to cancel")),
        "hint footer missing: {rows:?}"
    );
}

// =============================================================================
// VAL-WIDGET-010 — SettingsList edit semantics per value type
// =============================================================================

#[test]
fn settings_enter_toggles_bool() {
    let mut list = SettingsList::new(setting_entries());
    list.handle_key(&named("down", KeyCode::Down)); // auto_save (bool, true)
    list.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(list.entries()[1].value, SettingValue::Bool(false));
    // Space toggles it back.
    list.handle_key(&named("space", KeyCode::Char(' ')));
    assert_eq!(list.entries()[1].value, SettingValue::Bool(true));
}

#[test]
fn settings_enter_cycles_enum() {
    let mut list = SettingsList::new(setting_entries());
    // theme: dark -> light -> auto -> dark (cycles).
    list.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(list.entries()[0].value.to_string(), "light");
    list.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(list.entries()[0].value.to_string(), "auto");
    list.handle_key(&named("enter", KeyCode::Enter));
    assert_eq!(list.entries()[0].value.to_string(), "dark");
}

#[test]
fn settings_string_edit_in_place_with_caret() {
    let mut list = SettingsList::new(setting_entries());
    for _ in 0..3 {
        list.handle_key(&named("down", KeyCode::Down)); // model (string "gpt")
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // open editor
    assert!(list.is_editing());
    // A visible caret is reported while editing.
    let buf = render(&list, 60, 8);
    assert!(
        list.cursor().is_some(),
        "caret must be reported while editing"
    );
    let rows = all_rows(&buf);
    assert!(
        rows.iter().any(|r| r.contains('▏')),
        "caret marker missing: {rows:?}"
    );
    // Type more, then commit.
    list.handle_key(&chr('-'));
    list.handle_key(&chr('5'));
    list.handle_key(&named("enter", KeyCode::Enter));
    assert!(!list.is_editing());
    assert_eq!(
        list.entries()[3].value,
        SettingValue::String("gpt-5".into())
    );
    // Caret is gone once the editor closes.
    let _ = render(&list, 60, 8);
    assert!(list.cursor().is_none());
}

#[test]
fn settings_number_edit_commits_valid() {
    let mut list = SettingsList::new(setting_entries());
    for _ in 0..2 {
        list.handle_key(&named("down", KeyCode::Down)); // max_tokens (number 4096)
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // open editor pre-filled
    // Clear "4096" and type "8000".
    for _ in 0..4 {
        list.handle_key(&named("backspace", KeyCode::Backspace));
    }
    for c in "8000".chars() {
        list.handle_key(&chr(c));
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // commit
    assert_eq!(list.entries()[2].value, SettingValue::Number(8000.0));
}

#[test]
fn settings_invalid_number_keeps_old_value() {
    let mut list = SettingsList::new(setting_entries());
    for _ in 0..2 {
        list.handle_key(&named("down", KeyCode::Down)); // max_tokens
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // open editor
    for _ in 0..4 {
        list.handle_key(&named("backspace", KeyCode::Backspace));
    }
    for c in "abc".chars() {
        list.handle_key(&chr(c));
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // commit rejected silently
    assert_eq!(list.entries()[2].value, SettingValue::Number(4096.0));
}

#[test]
fn settings_escape_discards_edit() {
    let mut list = SettingsList::new(setting_entries());
    for _ in 0..3 {
        list.handle_key(&named("down", KeyCode::Down)); // model (string)
    }
    list.handle_key(&named("enter", KeyCode::Enter)); // open editor
    list.handle_key(&chr('X'));
    list.handle_key(&named("escape", KeyCode::Esc)); // discard
    assert!(!list.is_editing());
    assert_eq!(list.entries()[3].value, SettingValue::String("gpt".into()));
}

// =============================================================================
// VAL-WIDGET-015 — SettingsList empty state
// =============================================================================

#[test]
fn settings_empty_list_renders_no_settings() {
    let list = SettingsList::new(vec![]);
    let rows = all_rows(&render(&list, 40, 4));
    assert!(
        rows.iter().any(|r| r.contains("(no settings)")),
        "empty hint missing: {rows:?}"
    );
    // An empty list never opens the editor.
    let mut list = SettingsList::new(vec![]);
    list.handle_key(&named("enter", KeyCode::Enter));
    assert!(!list.is_editing());
}
