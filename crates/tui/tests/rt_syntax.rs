//! Behavioural tests for the rt keyword-driven syntax highlighter
//! (`hand_tui::rt::components::syntax_highlight`).
//!
//! These probe the signatures the external validator checks for
//! **VAL-WIDGET-004**: each token category (keyword / string / number / comment
//! / builtin-type) renders in a distinct color, language aliases resolve, a
//! `/* … */` block comment stays comment-colored continuously across lines, and
//! an unknown language degrades to a flat code color without dropping any line.
//!
//! Two layers:
//! - the [`highlight`] API directly, inspecting the returned styled [`Line`]s;
//! - the end-to-end path, painting a `MarkdownView` wired with
//!   [`default_markdown_theme`] into a ratatui `Buffer` and confirming the
//!   highlighter's colors reach real painted cells through the renderer's
//!   `CodeHighlighter` hook — the surface the validator probes.

use hand_tui::rt::components::syntax_highlight::{
    BUILTIN, COMMENT, DEFAULT_CODE, KEYWORD, NUMBER, STRING, highlight,
};
use hand_tui::rt::components::{MarkdownView, default_markdown_theme};
use hand_tui::rt::view::RtComponent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;

// --- helpers ----------------------------------------------------------------

/// The plain concatenated text of a styled line.
fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Whether any span on `line` carries exactly `content` in `color`.
fn has_token(line: &Line<'_>, color: Color, content: &str) -> bool {
    line.spans
        .iter()
        .any(|s| s.style.fg == Some(color) && s.content.as_ref() == content)
}

/// The set of foreground colors present on painted (non-blank) cells of a
/// `MarkdownView` rendered over `source`.
fn painted_colors(source: &str, width: u16, height: u16) -> Vec<Color> {
    let view = MarkdownView::new(source).theme(default_markdown_theme());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    let mut colors = Vec::new();
    for cell in buf.content() {
        if cell.symbol() != " " && !cell.symbol().is_empty() {
            colors.push(cell.fg);
        }
    }
    colors
}

// --- VAL-WIDGET-004: token classification -----------------------------------

#[test]
fn rust_categories_get_distinct_colors() {
    let lines = highlight("fn main() { let x: usize = 1; }", Some("rust"));
    let l = &lines[0];
    assert!(has_token(l, KEYWORD, "fn"), "keyword color: {l:?}");
    assert!(has_token(l, KEYWORD, "let"), "keyword color: {l:?}");
    assert!(has_token(l, BUILTIN, "usize"), "builtin-type color: {l:?}");
    assert!(has_token(l, NUMBER, "1"), "number color: {l:?}");
}

#[test]
fn rust_string_and_comment_colors() {
    let lines = highlight("let s = \"hi\"; // note", Some("rust"));
    assert!(has_token(&lines[0], STRING, "\"hi\""));
    assert!(has_token(&lines[0], COMMENT, "// note"));
}

#[test]
fn typescript_and_javascript_share_the_clike_family() {
    let ts = highlight("const greeting: string = \"hi\";", Some("ts"));
    assert!(has_token(&ts[0], KEYWORD, "const"));
    assert!(has_token(&ts[0], STRING, "\"hi\""));
    let js = highlight("function f() { return 1; }", Some("js"));
    assert!(has_token(&js[0], KEYWORD, "function"));
    assert!(has_token(&js[0], NUMBER, "1"));
}

#[test]
fn python_categories() {
    let lines = highlight("def greet(name):  # doc\n    print(name)", Some("python"));
    assert!(has_token(&lines[0], KEYWORD, "def"));
    assert!(has_token(&lines[0], COMMENT, "# doc"));
    assert!(has_token(&lines[1], BUILTIN, "print"));
}

#[test]
fn json_key_value_number_and_literal() {
    let lines = highlight(
        "{\n  \"name\": \"hand\",\n  \"n\": 1,\n  \"ok\": true\n}",
        Some("json"),
    );
    assert!(
        has_token(&lines[1], BUILTIN, "\"name\""),
        "key: {:?}",
        lines[1]
    );
    assert!(
        has_token(&lines[1], STRING, "\"hand\""),
        "value: {:?}",
        lines[1]
    );
    assert!(has_token(&lines[2], NUMBER, "1"), "number: {:?}", lines[2]);
    assert!(
        has_token(&lines[3], KEYWORD, "true"),
        "literal: {:?}",
        lines[3]
    );
}

#[test]
fn bash_keyword_variable_and_comment() {
    let lines = highlight("if [ -n $HOME ]; then echo hi; fi  # go", Some("bash"));
    assert!(has_token(&lines[0], KEYWORD, "if"));
    assert!(has_token(&lines[0], KEYWORD, "then"));
    assert!(has_token(&lines[0], BUILTIN, "$HOME"));
    assert!(has_token(&lines[0], BUILTIN, "echo"));
    assert!(has_token(&lines[0], COMMENT, "# go"));
}

#[test]
fn yaml_key_value_number_and_comment() {
    let lines = highlight("# top\nname: hand-ai\nversion: 1", Some("yaml"));
    assert!(has_token(&lines[0], COMMENT, "# top"));
    assert!(has_token(&lines[1], BUILTIN, "name"));
    assert!(has_token(&lines[2], BUILTIN, "version"));
    assert!(has_token(&lines[2], NUMBER, "1"));
}

#[test]
fn toml_section_pair_string_number_and_comment() {
    let lines = highlight("# a\n[package]\nname = \"hand\"\nver = 2", Some("toml"));
    assert!(has_token(&lines[0], COMMENT, "# a"));
    assert!(has_token(&lines[1], KEYWORD, "[package]"));
    assert!(has_token(&lines[2], BUILTIN, "name"));
    assert!(has_token(&lines[2], STRING, "\"hand\""));
    assert!(has_token(&lines[3], NUMBER, "2"));
}

// --- VAL-WIDGET-004: non-ASCII round-trip -----------------------------------

#[test]
fn non_ascii_identifier_round_trips_without_mojibake() {
    // An unquoted non-ASCII identifier is unclassified, so it lands in the flat
    // fallback run. The tokenizer must advance one whole char at a time — never
    // reinterpret a multi-byte UTF-8 sequence byte-by-byte as Latin-1 — or the
    // CJK name renders as mojibake.
    let src = "let 名前 = 1;";
    let lines = highlight(src, Some("rust"));
    assert_eq!(lines.len(), 1);
    assert_eq!(
        text(&lines[0]),
        src,
        "the non-ASCII identifier must round-trip losslessly"
    );
    assert!(
        has_token(&lines[0], KEYWORD, "let"),
        "keyword still classified"
    );
    assert!(has_token(&lines[0], NUMBER, "1"), "number still classified");
}

#[test]
fn non_ascii_round_trips_across_byte_tokenized_languages() {
    // Python, JSON, and Bash share the same byte-cursor unclassified tail as
    // Rust; each must carry a multi-byte identifier through intact.
    for (src, lang) in [
        ("値 = 1", "python"),
        ("{ 鍵: 1 }", "json"),
        ("echo 変数", "bash"),
    ] {
        let lines = highlight(src, Some(lang));
        assert_eq!(
            text(&lines[0]),
            src,
            "{lang}: non-ASCII must round-trip losslessly"
        );
    }
}

// --- VAL-WIDGET-004: aliases ------------------------------------------------

#[test]
fn language_aliases_resolve_to_the_same_family() {
    // rust
    assert!(has_token(
        &highlight("let x = 1;", Some("rs"))[0],
        KEYWORD,
        "let"
    ));
    // typescript
    for a in ["ts", "tsx", "typescript"] {
        assert!(
            has_token(&highlight("const x = 1;", Some(a))[0], KEYWORD, "const"),
            "alias {a}"
        );
    }
    // javascript
    for a in ["js", "jsx", "javascript", "mjs", "cjs"] {
        assert!(
            has_token(&highlight("return 1;", Some(a))[0], KEYWORD, "return"),
            "alias {a}"
        );
    }
    // python
    assert!(has_token(
        &highlight("return x", Some("py"))[0],
        KEYWORD,
        "return"
    ));
    // bash
    for a in ["sh", "shell", "zsh"] {
        assert!(
            has_token(&highlight("echo hi", Some(a))[0], BUILTIN, "echo"),
            "alias {a}"
        );
    }
    // yaml
    assert!(has_token(&highlight("k: 1", Some("yml"))[0], BUILTIN, "k"));
}

// --- VAL-WIDGET-004: cross-line block comment -------------------------------

#[test]
fn block_comment_is_continuously_colored_across_lines() {
    let lines = highlight("/* start\nmiddle\nend */ let x = 1;", Some("rust"));
    assert_eq!(lines.len(), 3);
    // Opening line: entirely comment.
    assert!(lines[0].spans.iter().all(|s| s.style.fg == Some(COMMENT)));
    // Middle line: one continuous comment span, no code color leaks in.
    assert!(lines[1].spans.iter().all(|s| s.style.fg == Some(COMMENT)));
    assert_eq!(text(&lines[1]), "middle");
    // Closing line: comment closes, then real code (the `let` keyword) resumes.
    assert!(has_token(&lines[2], COMMENT, "end */"));
    assert!(has_token(&lines[2], KEYWORD, "let"));
}

// --- VAL-WIDGET-004: unknown-language degrade -------------------------------

#[test]
fn unknown_language_degrades_flat_without_dropping_lines() {
    let lines = highlight("alpha\nbeta\ngamma", Some("klingon"));
    assert_eq!(lines.len(), 3);
    for l in &lines {
        assert!(
            l.spans.iter().all(|s| s.style.fg == Some(DEFAULT_CODE)),
            "unknown language must be flat: {l:?}"
        );
    }
    assert_eq!(text(&lines[2]), "gamma");
}

#[test]
fn absent_language_degrades_flat() {
    let lines = highlight("plain text", None);
    assert!(
        lines[0]
            .spans
            .iter()
            .all(|s| s.style.fg == Some(DEFAULT_CODE))
    );
}

// --- VAL-WIDGET-004: end-to-end through the markdown renderer ----------------

#[test]
fn highlighter_colors_reach_painted_cells_via_markdown_hook() {
    // A rust code block inside markdown, rendered through the real highlighter
    // theme, must paint keyword/string/number/comment/builtin-type cells in
    // their distinct colors — proving the hook is wired end to end.
    let source = "\
```rust
let count: usize = 42; // note
let s = \"hi\";
```";
    let colors = painted_colors(source, 60, 12);
    for (color, name) in [
        (KEYWORD, "keyword"),
        (STRING, "string"),
        (NUMBER, "number"),
        (COMMENT, "comment"),
        (BUILTIN, "builtin"),
    ] {
        assert!(
            colors.contains(&color),
            "{name} color missing from painted cells: {colors:?}"
        );
    }
}

#[test]
fn unknown_language_block_keeps_border_and_body_intact() {
    // The unknown-language fallback still paints the border frame and every body
    // line; the flat code color reaches painted cells.
    let source = "```klingon\nqapla body line\n```";
    let view = MarkdownView::new(source).theme(default_markdown_theme());
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    let mut rows = Vec::new();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        rows.push(row.trim_end().to_string());
    }
    assert!(
        rows.iter().any(|r| r.starts_with('┌')),
        "top border: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with('└')),
        "bottom border: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("qapla body line")),
        "body must survive: {rows:?}"
    );
    // The flat code color reaches the body cells.
    let colors = painted_colors(source, 40, 8);
    assert!(
        colors.contains(&DEFAULT_CODE),
        "flat code color: {colors:?}"
    );
}
