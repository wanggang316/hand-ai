//! Integration tests for the tokenized substring matcher (the module
//! keeps its historical `fuzzy` name).

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
fn tokens_must_be_contiguous_substrings() {
    // Subsequence-era behaviour: "abc" used to match "aXbXc" via
    // scattered chars. Tokens are contiguous substrings now.
    assert!(fuzzy_match("abc", "aXbXc").is_none());
    assert!(fuzzy_match("abc", "xxabc").is_some());
}

#[test]
fn token_order_in_target_is_irrelevant() {
    assert!(fuzzy_match("world hello", "hello world").is_some());
    assert!(fuzzy_match("foo bar", "foobar").is_some());
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
