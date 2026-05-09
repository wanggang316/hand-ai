//! Integration tests for `MarkdownComponent`.

mod common;

use hand_tui::{Component, MarkdownComponent, utils};

fn rendered(md: &str, width: u16) -> Vec<String> {
    MarkdownComponent::new(md).render(width)
}

fn visible(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| utils::strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn empty_source_yields_no_lines() {
    assert!(rendered("", 80).is_empty());
    assert!(rendered("   ", 80).is_empty());
}

#[test]
fn plain_paragraph_renders_text() {
    let lines = rendered("hello world", 80);
    assert!(visible(&lines).contains("hello world"));
}

#[test]
fn heading_text_appears_in_output() {
    let lines = rendered("# Title\n\nbody", 80);
    let v = visible(&lines);
    assert!(v.contains("Title"));
    assert!(v.contains("body"));
}

#[test]
fn list_items_each_render() {
    let lines = rendered("- one\n- two\n- three", 80);
    let v = visible(&lines);
    assert!(v.contains("one"));
    assert!(v.contains("two"));
    assert!(v.contains("three"));
}

#[test]
fn code_block_text_preserved() {
    let lines = rendered("```\nlet x = 1;\n```", 80);
    assert!(visible(&lines).contains("let x = 1;"));
}

#[test]
fn set_source_invalidates_cache() {
    let mut md = MarkdownComponent::new("first");
    let _ = md.render(80);
    md.set_source("second");
    assert_eq!(md.source(), "second");
    assert!(visible(&md.render(80)).contains("second"));
}
