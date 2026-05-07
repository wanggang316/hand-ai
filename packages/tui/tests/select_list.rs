//! Integration tests for `SelectListComponent`.

mod common;

use hand_tui::{Component, SelectItem, SelectListComponent, utils};

fn items() -> Vec<SelectItem> {
    vec![
        SelectItem::new("apple", "Apple"),
        SelectItem::new("banana", "Banana"),
        SelectItem::new("cherry", "Cherry"),
    ]
}

#[test]
fn defaults_to_first_item_selected() {
    let list = SelectListComponent::new(items());
    let sel = list.selected_item().unwrap();
    assert_eq!(sel.value, "apple");
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn set_selected_index_clamps_within_bounds() {
    let mut list = SelectListComponent::new(items());
    list.set_selected_index(2);
    assert_eq!(list.selected_index(), 2);
    list.set_selected_index(99);
    assert!(list.selected_index() < list.filtered_len());
}

#[test]
fn filter_narrows_visible_items() {
    let mut list = SelectListComponent::new(items());
    list.set_filter("ban");
    assert_eq!(list.filtered_len(), 1);
    assert_eq!(list.selected_item().unwrap().value, "banana");
}

#[test]
fn filter_clearing_restores_all_items() {
    let mut list = SelectListComponent::new(items());
    list.set_filter("a");
    list.set_filter("");
    assert_eq!(list.filtered_len(), 3);
}

#[test]
fn render_shows_all_labels_at_sufficient_width() {
    let list = SelectListComponent::new(items());
    let lines = list.render(40);
    let joined = lines
        .iter()
        .map(|l| utils::strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    for label in ["Apple", "Banana", "Cherry"] {
        assert!(joined.contains(label), "missing {} in {:?}", label, joined);
    }
}

#[test]
fn empty_list_renders_without_panic() {
    let list = SelectListComponent::new(vec![]);
    let lines = list.render(20);
    // Should produce at least one (possibly empty/placeholder) line, no panic.
    let _ = lines;
    assert_eq!(list.filtered_len(), 0);
    assert!(list.selected_item().is_none());
}
