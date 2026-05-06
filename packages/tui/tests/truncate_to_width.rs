//! Integration tests for `truncate_to_width` and `visible_width`.

mod common;

use hand_tui::utils;
use hand_tui::{truncate_to_width, visible_width};

#[test]
fn fits_unchanged_when_within_width() {
    let s = "hello";
    assert_eq!(truncate_to_width(s, 10), "hello");
    assert_eq!(visible_width(s), 5);
}

#[test]
fn truncates_long_input_with_ellipsis() {
    let s = "hello world this is long";
    let out = truncate_to_width(s, 10);
    assert!(visible_width(&out) <= 10, "got width {}", visible_width(&out));
    assert!(out.ends_with('…') || out.ends_with("…\x1b[0m"));
}

#[test]
fn handles_huge_unicode_input_safely() {
    let text: String = "🙂界".repeat(10_000);
    let out = utils::truncate_to_width_with(&text, 40, "…", false);
    assert!(visible_width(&out) <= 40);
    assert!(out.ends_with("…\x1b[0m"));
}

#[test]
fn pads_to_width_when_requested() {
    let out = utils::truncate_to_width_with("🙂界🙂界🙂界", 8, "…", true);
    assert_eq!(visible_width(&out), 8);
}

#[test]
fn preserves_ansi_styling_around_truncation() {
    let text = format!("\x1b[31m{}\x1b[0m", "hello ".repeat(50));
    let out = utils::truncate_to_width_with(&text, 20, "…", false);
    assert!(visible_width(&out) <= 20);
    assert!(out.contains("\x1b[31m"));
}

#[test]
fn visible_width_skips_ansi_and_counts_wide() {
    assert_eq!(visible_width("\t\x1b[31m界\x1b[0m"), 5);
    assert_eq!(visible_width("界"), 2);
    assert_eq!(visible_width(""), 0);
}

#[test]
fn normalize_terminal_output_keeps_visible_width_stable() {
    let s = "ำabc";
    let n = utils::normalize_terminal_output(s);
    assert_eq!(visible_width(&n), visible_width(s));
}
