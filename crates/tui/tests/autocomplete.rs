//! Integration tests for `AutocompleteComponent` and `Suggestion`.

mod common;

use hand_tui::{AutocompleteComponent, Component, Suggestion, utils};

fn suggestions() -> Vec<Suggestion> {
    vec![
        Suggestion::new("alpha"),
        Suggestion::new("bravo").with_label("Bravo"),
        Suggestion::new("charlie").with_description("third letter"),
    ]
}

#[test]
fn newly_constructed_is_hidden_with_no_count() {
    let ac = AutocompleteComponent::new();
    assert!(!ac.is_visible());
    assert_eq!(ac.count(), 0);
    assert!(ac.selected().is_none());
}

#[test]
fn set_suggestions_makes_visible_and_selects_first() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    assert!(ac.is_visible());
    assert_eq!(ac.count(), 3);
    assert_eq!(ac.selected().unwrap().value, "alpha");
}

#[test]
fn select_next_wraps_around() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    ac.select_next();
    ac.select_next();
    assert_eq!(ac.selected().unwrap().value, "charlie");
    ac.select_next();
    assert_eq!(ac.selected().unwrap().value, "alpha");
}

#[test]
fn select_prev_wraps_around() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    ac.select_prev();
    assert_eq!(ac.selected().unwrap().value, "charlie");
}

#[test]
fn hide_clears_visibility_until_show() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    ac.hide();
    assert!(!ac.is_visible());
    ac.show();
    assert!(ac.is_visible());
}

#[test]
fn empty_set_clears_selection_and_hides() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    ac.set_suggestions(vec![]);
    assert!(!ac.is_visible());
    assert_eq!(ac.count(), 0);
    assert!(ac.selected().is_none());
}

#[test]
fn render_includes_visible_labels() {
    let mut ac = AutocompleteComponent::new();
    ac.set_suggestions(suggestions());
    let lines = ac.render(40);
    let joined = lines
        .iter()
        .map(|l| utils::strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("alpha"));
    assert!(joined.contains("Bravo"));
    assert!(joined.contains("charlie"));
}
