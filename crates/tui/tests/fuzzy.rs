//! Integration tests for the fuzzy matcher.

mod common;

use hand_tui::{FuzzyMatch, fuzzy_filter, fuzzy_match};

#[test]
fn empty_query_matches_with_zero_score() {
    let m = fuzzy_match("", "anything").unwrap();
    assert_eq!(m.score, 0);
    assert!(m.indices.is_empty());
}

#[test]
fn returns_none_when_no_match() {
    assert!(fuzzy_match("xyz", "hello").is_none());
}

#[test]
fn case_insensitive_matching() {
    let m = fuzzy_match("ABC", "abcdef").unwrap();
    assert_eq!(m.indices, vec![0, 1, 2]);
}

#[test]
fn order_is_required() {
    assert!(fuzzy_match("abc", "aXbXc").is_some());
    assert!(fuzzy_match("abc", "cba").is_none());
}

#[test]
fn consecutive_outscores_scattered() {
    let consecutive = fuzzy_match("foo", "foobar").unwrap();
    let scattered = fuzzy_match("foo", "f_o_o_bar").unwrap();
    assert!(consecutive.score > scattered.score);
}

#[test]
fn fuzzy_filter_excludes_non_matches_and_sorts() {
    let items = vec!["apply", "app", "append", "banana"];
    let results: Vec<(usize, FuzzyMatch)> = fuzzy_filter("app", &items);
    assert!(!results.iter().any(|(i, _)| items[*i] == "banana"));
    // Sorted descending by score.
    for w in results.windows(2) {
        assert!(w[0].1.score >= w[1].1.score);
    }
}

#[test]
fn fuzzy_filter_empty_input() {
    let items: Vec<&str> = vec![];
    assert!(fuzzy_filter("anything", &items).is_empty());
}
