//! Lightweight ANSI syntax highlighter for fenced code blocks.
//!
//! Takes a code body plus an optional language tag and returns one
//! ANSI-colored line per source line. The goal is "code blocks look
//! like code", not a faithful semantic lexer — so this module is
//! intentionally regex-free and keyword-list driven. When the language
//! is unknown the body is returned with a flat color prefix.
//!
//! Supported languages: rust, ts/tsx/typescript, js/jsx/javascript,
//! python/py, json, bash/sh/shell, yaml/yml, toml. Aliases are mapped
//! to the canonical id in [`resolve_language`].
//!
//! ANSI palette mirrors common terminal "monokai-ish" mapping:
//!   keyword → bright cyan (96)
//!   string  → green       (32)
//!   number  → yellow      (33)
//!   comment → bright black (90)
//!   builtin → blue        (34)
//! Other characters are emitted unstyled.

use std::sync::Arc;

use hand_tui::components::markdown::{CodeHighlighter, MarkdownTheme};

// ---------------------------------------------------------------------------
// ANSI helpers
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const KEYWORD: &str = "\x1b[96m";
const STRING: &str = "\x1b[32m";
const NUMBER: &str = "\x1b[33m";
const COMMENT: &str = "\x1b[90m";
const BUILTIN: &str = "\x1b[34m";
const DEFAULT_BLOCK_FG: &str = "\x1b[37m";

fn paint(color: &str, text: &str) -> String {
    let mut s = String::with_capacity(text.len() + color.len() + RESET.len());
    s.push_str(color);
    s.push_str(text);
    s.push_str(RESET);
    s
}

// ---------------------------------------------------------------------------
// Public entry
// ---------------------------------------------------------------------------

/// Build the default `CodeHighlighter` closure for `MarkdownTheme`.
pub fn default_highlighter() -> CodeHighlighter {
    Arc::new(|code: &str, lang: Option<&str>| highlight(code, lang))
}

/// Return a `MarkdownTheme` with the default syntax highlighter wired in.
/// Call sites that build a `MarkdownComponent` should use this in place of
/// `MarkdownTheme::default()` to get fenced-block coloring.
pub fn default_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        highlight: Some(default_highlighter()),
        ..MarkdownTheme::default()
    }
}

/// Highlight `code` in `lang`. Returns one ANSI-styled string per source
/// line (no trailing newline).
pub fn highlight(code: &str, lang: Option<&str>) -> Vec<String> {
    match resolve_language(lang) {
        Some(Language::Rust) => highlight_clike(code, RUST_KEYWORDS, RUST_TYPES, CLikeFlavor::Rust),
        Some(Language::TypeScript) | Some(Language::JavaScript) => {
            highlight_clike(code, JS_KEYWORDS, JS_BUILTINS, CLikeFlavor::JsLike)
        }
        Some(Language::Python) => highlight_python(code),
        Some(Language::Json) => highlight_json(code),
        Some(Language::Bash) => highlight_bash(code),
        Some(Language::Yaml) => highlight_yaml(code),
        Some(Language::Toml) => highlight_toml(code),
        None => code
            .lines()
            .map(|l| {
                let mut s = String::with_capacity(l.len() + 8);
                s.push_str(DEFAULT_BLOCK_FG);
                s.push_str(l);
                s.push_str(RESET);
                s
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Json,
    Bash,
    Yaml,
    Toml,
}

fn resolve_language(lang: Option<&str>) -> Option<Language> {
    match lang?.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => Some(Language::Rust),
        "ts" | "tsx" | "typescript" => Some(Language::TypeScript),
        "js" | "jsx" | "javascript" | "mjs" | "cjs" => Some(Language::JavaScript),
        "py" | "python" => Some(Language::Python),
        "json" => Some(Language::Json),
        "bash" | "sh" | "shell" | "zsh" => Some(Language::Bash),
        "yaml" | "yml" => Some(Language::Yaml),
        "toml" => Some(Language::Toml),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// C-like family (Rust, TypeScript, JavaScript)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum CLikeFlavor {
    Rust,
    JsLike,
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "yield", "box",
];

const RUST_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
    "u64", "u128", "usize", "str", "String", "Vec", "Box", "Option", "Result", "Arc", "Rc",
    "RefCell", "Mutex", "HashMap", "HashSet", "BTreeMap", "BTreeSet",
];

const JS_KEYWORDS: &[&str] = &[
    "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch", "class",
    "const", "constructor", "continue", "debugger", "declare", "default", "delete", "do",
    "else", "enum", "export", "extends", "false", "finally", "for", "from", "function", "get",
    "if", "implements", "import", "in", "instanceof", "interface", "let", "namespace", "new",
    "null", "of", "package", "private", "protected", "public", "readonly", "return", "set",
    "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined",
    "var", "void", "while", "with", "yield",
];

const JS_BUILTINS: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "Function", "JSON", "Map", "Math", "Number", "Object",
    "Promise", "RegExp", "Set", "String", "Symbol", "console", "document", "window", "globalThis",
    "process",
];

fn highlight_clike(
    code: &str,
    keywords: &[&str],
    types: &[&str],
    flavor: CLikeFlavor,
) -> Vec<String> {
    let mut in_block_comment = false;
    code.lines()
        .map(|line| highlight_clike_line(line, keywords, types, &mut in_block_comment, flavor))
        .collect()
}

fn highlight_clike_line(
    line: &str,
    keywords: &[&str],
    types: &[&str],
    in_block_comment: &mut bool,
    flavor: CLikeFlavor,
) -> String {
    let mut out = String::with_capacity(line.len() + 16);
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if *in_block_comment {
            let start = i;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    *in_block_comment = false;
                    break;
                }
                i += 1;
                if i >= bytes.len() {
                    break;
                }
            }
            if i > start {
                out.push_str(&paint(COMMENT, &line[start..i.min(bytes.len())]));
            } else if i == start {
                // Reached end without close — paint the rest as comment.
                out.push_str(&paint(COMMENT, &line[start..]));
                i = bytes.len();
            }
            if !*in_block_comment {
                continue;
            }
            // still in comment but EOL — already painted.
            break;
        }
        // Line comment
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            out.push_str(&paint(COMMENT, &line[i..]));
            return out;
        }
        // Block comment open
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            *in_block_comment = true;
            let start = i;
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    *in_block_comment = false;
                    break;
                }
                i += 1;
            }
            if *in_block_comment {
                // Ran off end — paint rest.
                out.push_str(&paint(COMMENT, &line[start..]));
                return out;
            } else {
                out.push_str(&paint(COMMENT, &line[start..i]));
                continue;
            }
        }
        // String
        if c == '"' || c == '\'' || (matches!(flavor, CLikeFlavor::JsLike) && c == '`') {
            let quote = c as u8;
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&paint(STRING, &line[start..i]));
            continue;
        }
        // Number
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'_'
                    || bytes[i] == b'x'
                    || bytes[i] == b'b'
                    || bytes[i] == b'o'
                    || (bytes[i] >= b'a' && bytes[i] <= b'f')
                    || (bytes[i] >= b'A' && bytes[i] <= b'F'))
            {
                i += 1;
            }
            out.push_str(&paint(NUMBER, &line[start..i]));
            continue;
        }
        // Identifier / keyword
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if keywords.contains(&word) {
                out.push_str(&paint(KEYWORD, word));
            } else if types.contains(&word) {
                out.push_str(&paint(BUILTIN, word));
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try",
    "while", "with", "yield", "match", "case",
];

const PY_BUILTINS: &[&str] = &[
    "abs", "all", "any", "bool", "bytes", "callable", "chr", "dict", "dir", "enumerate", "filter",
    "float", "frozenset", "getattr", "hasattr", "hash", "id", "input", "int", "isinstance",
    "issubclass", "iter", "len", "list", "map", "max", "min", "next", "object", "open", "ord",
    "print", "range", "repr", "reversed", "round", "set", "setattr", "slice", "sorted", "str",
    "sum", "tuple", "type", "vars", "zip",
];

fn highlight_python(code: &str) -> Vec<String> {
    code.lines()
        .map(highlight_python_line)
        .collect()
}

fn highlight_python_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            out.push_str(&paint(COMMENT, &line[i..]));
            return out;
        }
        if c == '"' || c == '\'' {
            let quote = c as u8;
            let start = i;
            // Triple-quote handling: if next two bytes match, scan until matching triple.
            let triple = i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote;
            if triple {
                i += 3;
                while i + 2 < bytes.len()
                    && !(bytes[i] == quote && bytes[i + 1] == quote && bytes[i + 2] == quote)
                {
                    i += 1;
                }
                if i + 2 < bytes.len() {
                    i += 3;
                } else {
                    i = bytes.len();
                }
            } else {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            out.push_str(&paint(STRING, &line[start..i]));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'_') {
                i += 1;
            }
            out.push_str(&paint(NUMBER, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if PY_KEYWORDS.contains(&word) {
                out.push_str(&paint(KEYWORD, word));
            } else if PY_BUILTINS.contains(&word) {
                out.push_str(&paint(BUILTIN, word));
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn highlight_json(code: &str) -> Vec<String> {
    code.lines().map(highlight_json_line).collect()
}

fn highlight_json_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            // Detect "key": pattern — if the next non-ws byte is ':', color
            // it as a builtin so keys stand out from values.
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let is_key = j < bytes.len() && bytes[j] == b':';
            let color = if is_key { BUILTIN } else { STRING };
            out.push_str(&paint(color, &line[start..i]));
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '-' {
                i += 1;
            }
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'e' || bytes[i] == b'E' || bytes[i] == b'+' || bytes[i] == b'-')
            {
                i += 1;
            }
            out.push_str(&paint(NUMBER, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &line[start..i];
            if matches!(word, "true" | "false" | "null") {
                out.push_str(&paint(KEYWORD, word));
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

const BASH_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "return", "exit", "break", "continue", "local", "export", "readonly",
    "declare", "set", "unset", "shift", "source", "alias", "true", "false",
];

const BASH_BUILTINS: &[&str] = &[
    "echo", "printf", "read", "cd", "pwd", "ls", "cat", "grep", "awk", "sed", "find", "test",
    "trap", "wait", "kill", "jobs", "fg", "bg",
];

fn highlight_bash(code: &str) -> Vec<String> {
    code.lines().map(highlight_bash_line).collect()
}

fn highlight_bash_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            out.push_str(&paint(COMMENT, &line[i..]));
            return out;
        }
        if c == '"' || c == '\'' {
            let quote = c as u8;
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() && quote == b'"' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&paint(STRING, &line[start..i]));
            continue;
        }
        if c == '$' {
            let start = i;
            i += 1;
            if i < bytes.len() && bytes[i] == b'{' {
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            out.push_str(&paint(BUILTIN, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
                i += 1;
            }
            let word = &line[start..i];
            if BASH_KEYWORDS.contains(&word) {
                out.push_str(&paint(KEYWORD, word));
            } else if BASH_BUILTINS.contains(&word) {
                out.push_str(&paint(BUILTIN, word));
            } else {
                out.push_str(word);
            }
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            out.push_str(&paint(NUMBER, &line[start..i]));
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

fn highlight_yaml(code: &str) -> Vec<String> {
    code.lines().map(highlight_yaml_line).collect()
}

fn highlight_yaml_line(line: &str) -> String {
    // Strip leading whitespace for analysis but preserve it in output.
    let trimmed_start = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(trimmed_start);
    let mut out = String::with_capacity(line.len() + 16);
    out.push_str(indent);
    // Comment line
    if rest.starts_with('#') {
        out.push_str(&paint(COMMENT, rest));
        return out;
    }
    // List item marker
    let rest = if let Some(after) = rest.strip_prefix("- ") {
        out.push_str(&paint(KEYWORD, "- "));
        after
    } else {
        rest
    };
    // Look for `key:`
    if let Some(colon_pos) = rest.find(':') {
        let key = &rest[..colon_pos];
        let after = &rest[colon_pos..];
        // Only treat as a mapping key when the char after ':' is space, tab,
        // newline, or end-of-line.
        let mapping_break = after.len() == 1
            || after.as_bytes().get(1).map(|b| matches!(b, b' ' | b'\t')).unwrap_or(true);
        if mapping_break && !key.is_empty() && !key.contains(' ') {
            out.push_str(&paint(BUILTIN, key));
            out.push(':');
            let value = &after[1..];
            highlight_yaml_value(value.trim_start(), value.len() - value.trim_start().len(), &mut out);
            return out;
        }
    }
    // Otherwise treat the rest as a value
    highlight_yaml_value(rest, 0, &mut out);
    out
}

fn highlight_yaml_value(value: &str, leading_ws: usize, out: &mut String) {
    out.push_str(&" ".repeat(leading_ws));
    if value.is_empty() {
        return;
    }
    let trimmed = value;
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        out.push_str(&paint(STRING, trimmed));
        return;
    }
    if matches!(trimmed.trim(), "true" | "false" | "null" | "~" | "yes" | "no") {
        out.push_str(&paint(KEYWORD, trimmed));
        return;
    }
    if trimmed.chars().next().map(|c| c.is_ascii_digit() || c == '-').unwrap_or(false)
        && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
    {
        out.push_str(&paint(NUMBER, trimmed));
        return;
    }
    out.push_str(trimmed);
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

fn highlight_toml(code: &str) -> Vec<String> {
    code.lines().map(highlight_toml_line).collect()
}

fn highlight_toml_line(line: &str) -> String {
    let trimmed_start = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(trimmed_start);
    let mut out = String::with_capacity(line.len() + 16);
    out.push_str(indent);
    if rest.starts_with('#') {
        out.push_str(&paint(COMMENT, rest));
        return out;
    }
    if rest.starts_with('[')
        && let Some(end) = rest.find(']') {
            out.push_str(&paint(KEYWORD, &rest[..=end]));
            out.push_str(&rest[end + 1..]);
            return out;
        }
    if let Some(eq_pos) = rest.find('=') {
        let key = rest[..eq_pos].trim_end();
        let after = &rest[eq_pos..];
        out.push_str(&paint(BUILTIN, key));
        out.push_str(&" ".repeat(rest[..eq_pos].len() - key.len()));
        out.push('=');
        let value = &after[1..];
        let value_trimmed = value.trim_start();
        out.push_str(&" ".repeat(value.len() - value_trimmed.len()));
        if value_trimmed.starts_with('"') || value_trimmed.starts_with('\'') {
            out.push_str(&paint(STRING, value_trimmed));
        } else if matches!(value_trimmed.trim(), "true" | "false") {
            out.push_str(&paint(KEYWORD, value_trimmed));
        } else if value_trimmed.chars().next().map(|c| c.is_ascii_digit() || c == '-').unwrap_or(false) {
            out.push_str(&paint(NUMBER, value_trimmed));
        } else {
            out.push_str(value_trimmed);
        }
        return out;
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        // crude SGR-stripper for assertions: drop \x1b[...m sequences.
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                while let Some(c) = chars.next() {
                    if c == 'm' {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn unknown_language_returns_default_color() {
        let lines = highlight("hello world", Some("klingon"));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(DEFAULT_BLOCK_FG));
        assert_eq!(strip(&lines[0]), "hello world");
    }

    #[test]
    fn none_language_returns_default_color() {
        let lines = highlight("plain", None);
        assert_eq!(strip(&lines[0]), "plain");
    }

    #[test]
    fn rust_keywords_get_keyword_color() {
        let lines = highlight("fn main() { let x = 1; }", Some("rust"));
        let l = &lines[0];
        assert!(l.contains(&paint(KEYWORD, "fn")));
        assert!(l.contains(&paint(KEYWORD, "let")));
        assert!(l.contains(&paint(NUMBER, "1")));
    }

    #[test]
    fn rust_line_comment_paints_rest() {
        let lines = highlight("let x = 1; // trailing comment", Some("rs"));
        assert!(lines[0].contains(&paint(COMMENT, "// trailing comment")));
    }

    #[test]
    fn rust_block_comment_spans_lines() {
        let lines = highlight("/* start\nmiddle\nend */ let x = 1;", Some("rust"));
        assert_eq!(lines.len(), 3);
        // First line: starts the block.
        assert!(lines[0].contains(COMMENT));
        // Middle line is entirely comment.
        assert!(lines[1].starts_with(COMMENT));
        // Closing line: comment-then-code; `let` keyword reappears.
        assert!(lines[2].contains(&paint(KEYWORD, "let")));
    }

    #[test]
    fn ts_keywords_and_strings() {
        let lines = highlight("const greeting: string = \"hi\";", Some("ts"));
        let l = &lines[0];
        assert!(l.contains(&paint(KEYWORD, "const")));
        assert!(l.contains(&paint(STRING, "\"hi\"")));
    }

    #[test]
    fn python_def_and_string() {
        let lines = highlight("def greet(name):\n    return f'hi {name}'", Some("python"));
        assert!(lines[0].contains(&paint(KEYWORD, "def")));
        assert!(lines[1].contains(&paint(KEYWORD, "return")));
    }

    #[test]
    fn json_key_and_value_distinct_colors() {
        let lines = highlight("{\n  \"name\": \"hand\",\n  \"age\": 1\n}", Some("json"));
        // Find a key (BUILTIN-colored) and a value (STRING-colored).
        let joined = lines.join("\n");
        assert!(joined.contains(&paint(BUILTIN, "\"name\"")));
        assert!(joined.contains(&paint(STRING, "\"hand\"")));
        assert!(joined.contains(&paint(NUMBER, "1")));
    }

    #[test]
    fn bash_keyword_and_variable() {
        // $VAR outside quotes is the variable, inside quotes it's part of
        // the string (no shell-style interpolation parsing in this
        // lightweight tokenizer).
        let lines = highlight("if [ -n $HOME ]; then echo hi; fi", Some("bash"));
        let l = &lines[0];
        assert!(l.contains(&paint(KEYWORD, "if")));
        assert!(l.contains(&paint(KEYWORD, "then")));
        assert!(l.contains(&paint(KEYWORD, "fi")));
        assert!(l.contains(&paint(BUILTIN, "$HOME")));
    }

    #[test]
    fn yaml_key_value_split() {
        let lines = highlight("name: hand-ai\nversion: 1", Some("yaml"));
        assert!(lines[0].contains(&paint(BUILTIN, "name")));
        assert!(lines[1].contains(&paint(BUILTIN, "version")));
        assert!(lines[1].contains(&paint(NUMBER, "1")));
    }

    #[test]
    fn toml_section_and_pair() {
        let lines = highlight("[package]\nname = \"hand\"", Some("toml"));
        assert!(lines[0].contains(&paint(KEYWORD, "[package]")));
        assert!(lines[1].contains(&paint(BUILTIN, "name")));
        assert!(lines[1].contains(&paint(STRING, "\"hand\"")));
    }

    #[test]
    fn default_highlighter_round_trips_through_arc() {
        let h = default_highlighter();
        let out = h("let x = 1;", Some("rust"));
        assert!(out[0].contains(&paint(KEYWORD, "let")));
    }
}
