//! Keyword-driven syntax highlighter for fenced code blocks — rt-native.
//!
//! The rt-native counterpart to the legacy ANSI highlighter: it takes a code
//! body plus an optional language tag and returns one styled [`Line<'static>`]
//! per source line, ready to fill the markdown renderer's [`CodeHighlighter`]
//! hook. The goal is "code blocks look like code", not a faithful semantic
//! lexer — so this is intentionally regex-free and keyword-list driven, the same
//! shape as the legacy tokenizer, only the output moves from embedded SGR escape
//! strings to ratatui [`Span`]s carrying a [`Style`].
//!
//! Supported languages: rust, ts/tsx/typescript, js/jsx/javascript,
//! python/py, json, bash/sh/shell/zsh, yaml/yml, toml. Aliases map to the
//! canonical id in [`resolve_language`]. When the language is unknown (or
//! absent) the body is emitted flat in a single code color — border and body
//! stay intact, nothing is dropped.
//!
//! Token colors (each category a distinct [`Color`], mirroring the legacy
//! "monokai-ish" terminal mapping):
//!
//! - keyword — [`KEYWORD`] (bright cyan)
//! - string — [`STRING`] (green)
//! - number — [`NUMBER`] (yellow)
//! - comment — [`COMMENT`] (bright black / gray)
//! - builtin type — [`BUILTIN`] (blue)
//!
//! Unclassified characters take the flat [`DEFAULT_CODE`] color.

use std::sync::Arc;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::markdown::{CodeHighlighter, MarkdownTheme};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Keyword color (legacy ANSI 96 — bright cyan).
pub const KEYWORD: Color = Color::LightCyan;
/// String-literal color (legacy ANSI 32 — green).
pub const STRING: Color = Color::Green;
/// Numeric-literal color (legacy ANSI 33 — yellow).
pub const NUMBER: Color = Color::Yellow;
/// Comment color (legacy ANSI 90 — bright black, i.e. gray).
pub const COMMENT: Color = Color::DarkGray;
/// Builtin-type / key color (legacy ANSI 34 — blue).
pub const BUILTIN: Color = Color::Blue;
/// Flat color for the unknown-language fallback and unclassified characters
/// (legacy ANSI 37 — white/gray).
pub const DEFAULT_CODE: Color = Color::Gray;

/// A styled span carrying `text` in `color`.
fn painted(color: Color, text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(color))
}

/// An unclassified span in the flat code color.
fn plain(text: &str) -> Span<'static> {
    painted(DEFAULT_CODE, text)
}

// ---------------------------------------------------------------------------
// Public entry
// ---------------------------------------------------------------------------

/// Build the default [`CodeHighlighter`] closure to wire into a
/// [`MarkdownTheme`].
#[must_use]
pub fn default_highlighter() -> CodeHighlighter {
    Arc::new(|code: &str, lang: Option<&str>| highlight(code, lang))
}

/// A [`MarkdownTheme`] with this syntax highlighter wired into its code-block
/// hook. Call sites that want fenced-block coloring use this in place of
/// [`MarkdownTheme::default`].
#[must_use]
pub fn default_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        highlight: Some(default_highlighter()),
        ..MarkdownTheme::default()
    }
}

/// Highlight `code` in `lang`, returning one styled [`Line<'static>`] per source
/// line (the fenced body's trailing newline is dropped by [`str::lines`], the
/// same convention the markdown renderer expects).
#[must_use]
pub fn highlight(code: &str, lang: Option<&str>) -> Vec<Line<'static>> {
    match resolve_language(lang) {
        Some(Language::Rust) => highlight_clike(code, RUST_KEYWORDS, RUST_TYPES, CLikeFlavor::Rust),
        Some(Language::TypeScript | Language::JavaScript) => {
            highlight_clike(code, JS_KEYWORDS, JS_BUILTINS, CLikeFlavor::JsLike)
        }
        Some(Language::Python) => highlight_lines(code, highlight_python_line),
        Some(Language::Json) => highlight_lines(code, highlight_json_line),
        Some(Language::Bash) => highlight_lines(code, highlight_bash_line),
        Some(Language::Yaml) => highlight_lines(code, highlight_yaml_line),
        Some(Language::Toml) => highlight_lines(code, highlight_toml_line),
        None => code.lines().map(|l| Line::from(plain(l))).collect(),
    }
}

/// Map each source line through a per-line tokenizer that has no cross-line
/// state.
fn highlight_lines(code: &str, f: fn(&str) -> Vec<Span<'static>>) -> Vec<Line<'static>> {
    code.lines().map(|l| Line::from(f(l))).collect()
}

// ---------------------------------------------------------------------------
// Language resolution
// ---------------------------------------------------------------------------

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
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield", "box",
];

const RUST_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
    "u64", "u128", "usize", "str", "String", "Vec", "Box", "Option", "Result", "Arc", "Rc",
    "RefCell", "Mutex", "HashMap", "HashSet", "BTreeMap", "BTreeSet",
];

const JS_KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "async",
    "await",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constructor",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "namespace",
    "new",
    "null",
    "of",
    "package",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const JS_BUILTINS: &[&str] = &[
    "Array",
    "Boolean",
    "Date",
    "Error",
    "Function",
    "JSON",
    "Map",
    "Math",
    "Number",
    "Object",
    "Promise",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "console",
    "document",
    "window",
    "globalThis",
    "process",
];

fn highlight_clike(
    code: &str,
    keywords: &[&str],
    types: &[&str],
    flavor: CLikeFlavor,
) -> Vec<Line<'static>> {
    let mut in_block_comment = false;
    code.lines()
        .map(|line| {
            Line::from(highlight_clike_line(
                line,
                keywords,
                types,
                &mut in_block_comment,
                flavor,
            ))
        })
        .collect()
}

fn highlight_clike_line(
    line: &str,
    keywords: &[&str],
    types: &[&str],
    in_block_comment: &mut bool,
    flavor: CLikeFlavor,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = line.as_bytes();
    // Buffer of unclassified characters, flushed as one flat span before any
    // colored token so the flat runs are not fragmented per byte.
    let mut pending = String::new();
    let flush = |spans: &mut Vec<Span<'static>>, pending: &mut String| {
        if !pending.is_empty() {
            spans.push(plain(pending));
            pending.clear();
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if *in_block_comment {
            let start = i;
            while i < bytes.len() {
                if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    *in_block_comment = false;
                    break;
                }
                i += 1;
            }
            spans.push(painted(COMMENT, &line[start..i.min(bytes.len())]));
            if *in_block_comment {
                // EOL while still inside the comment — the rest was painted.
                break;
            }
            continue;
        }
        // Line comment: paints to end of line.
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            flush(&mut spans, &mut pending);
            spans.push(painted(COMMENT, &line[i..]));
            return spans;
        }
        // Block comment open.
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            flush(&mut spans, &mut pending);
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
                // Ran off the end of the line — paint the rest.
                spans.push(painted(COMMENT, &line[start..]));
                return spans;
            }
            spans.push(painted(COMMENT, &line[start..i]));
            continue;
        }
        // String literal.
        if c == '"' || c == '\'' || (matches!(flavor, CLikeFlavor::JsLike) && c == '`') {
            flush(&mut spans, &mut pending);
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
            spans.push(painted(STRING, &line[start..i]));
            continue;
        }
        // Number literal.
        if c.is_ascii_digit() {
            flush(&mut spans, &mut pending);
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'_'
                    || bytes[i] == b'x'
                    || bytes[i] == b'b'
                    || bytes[i] == b'o'
                    || bytes[i].is_ascii_hexdigit())
            {
                i += 1;
            }
            spans.push(painted(NUMBER, &line[start..i]));
            continue;
        }
        // Identifier / keyword / builtin type.
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if keywords.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(KEYWORD, word));
            } else if types.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(BUILTIN, word));
            } else {
                pending.push_str(word);
            }
            continue;
        }
        pending.push(c);
        i += 1;
    }
    flush(&mut spans, &mut pending);
    spans
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case",
];

const PY_BUILTINS: &[&str] = &[
    "abs",
    "all",
    "any",
    "bool",
    "bytes",
    "callable",
    "chr",
    "dict",
    "dir",
    "enumerate",
    "filter",
    "float",
    "frozenset",
    "getattr",
    "hasattr",
    "hash",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "map",
    "max",
    "min",
    "next",
    "object",
    "open",
    "ord",
    "print",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "str",
    "sum",
    "tuple",
    "type",
    "vars",
    "zip",
];

fn highlight_python_line(line: &str) -> Vec<Span<'static>> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut pending = String::new();
    let flush = |spans: &mut Vec<Span<'static>>, pending: &mut String| {
        if !pending.is_empty() {
            spans.push(plain(pending));
            pending.clear();
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            flush(&mut spans, &mut pending);
            spans.push(painted(COMMENT, &line[i..]));
            return spans;
        }
        if c == '"' || c == '\'' {
            flush(&mut spans, &mut pending);
            let quote = c as u8;
            let start = i;
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
            spans.push(painted(STRING, &line[start..i]));
            continue;
        }
        if c.is_ascii_digit() {
            flush(&mut spans, &mut pending);
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
            spans.push(painted(NUMBER, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if PY_KEYWORDS.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(KEYWORD, word));
            } else if PY_BUILTINS.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(BUILTIN, word));
            } else {
                pending.push_str(word);
            }
            continue;
        }
        pending.push(c);
        i += 1;
    }
    flush(&mut spans, &mut pending);
    spans
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn highlight_json_line(line: &str) -> Vec<Span<'static>> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut pending = String::new();
    let flush = |spans: &mut Vec<Span<'static>>, pending: &mut String| {
        if !pending.is_empty() {
            spans.push(plain(pending));
            pending.clear();
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' {
            flush(&mut spans, &mut pending);
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
            // A `"key":` gets the builtin color so keys stand out from values.
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let is_key = j < bytes.len() && bytes[j] == b':';
            let color = if is_key { BUILTIN } else { STRING };
            spans.push(painted(color, &line[start..i]));
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            flush(&mut spans, &mut pending);
            let start = i;
            if c == '-' {
                i += 1;
            }
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E'
                    || bytes[i] == b'+'
                    || bytes[i] == b'-')
            {
                i += 1;
            }
            spans.push(painted(NUMBER, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &line[start..i];
            if matches!(word, "true" | "false" | "null") {
                flush(&mut spans, &mut pending);
                spans.push(painted(KEYWORD, word));
            } else {
                pending.push_str(word);
            }
            continue;
        }
        pending.push(c);
        i += 1;
    }
    flush(&mut spans, &mut pending);
    spans
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

fn highlight_bash_line(line: &str) -> Vec<Span<'static>> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut pending = String::new();
    let flush = |spans: &mut Vec<Span<'static>>, pending: &mut String| {
        if !pending.is_empty() {
            spans.push(plain(pending));
            pending.clear();
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            flush(&mut spans, &mut pending);
            spans.push(painted(COMMENT, &line[i..]));
            return spans;
        }
        if c == '"' || c == '\'' {
            flush(&mut spans, &mut pending);
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
            spans.push(painted(STRING, &line[start..i]));
            continue;
        }
        if c == '$' {
            flush(&mut spans, &mut pending);
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
            spans.push(painted(BUILTIN, &line[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
            {
                i += 1;
            }
            let word = &line[start..i];
            if BASH_KEYWORDS.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(KEYWORD, word));
            } else if BASH_BUILTINS.contains(&word) {
                flush(&mut spans, &mut pending);
                spans.push(painted(BUILTIN, word));
            } else {
                pending.push_str(word);
            }
            continue;
        }
        if c.is_ascii_digit() {
            flush(&mut spans, &mut pending);
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            spans.push(painted(NUMBER, &line[start..i]));
            continue;
        }
        pending.push(c);
        i += 1;
    }
    flush(&mut spans, &mut pending);
    spans
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

fn highlight_yaml_line(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Preserve leading whitespace in output but analyse the trimmed remainder.
    let trimmed_start = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(trimmed_start);
    if !indent.is_empty() {
        spans.push(plain(indent));
    }
    // Comment line.
    if rest.starts_with('#') {
        spans.push(painted(COMMENT, rest));
        return spans;
    }
    // List item marker.
    let rest = if let Some(after) = rest.strip_prefix("- ") {
        spans.push(painted(KEYWORD, "- "));
        after
    } else {
        rest
    };
    // `key:` mapping.
    if let Some(colon_pos) = rest.find(':') {
        let key = &rest[..colon_pos];
        let after = &rest[colon_pos..];
        // Treat as a mapping key only when the char after ':' is space/tab/EOL.
        let mapping_break = after.len() == 1
            || after
                .as_bytes()
                .get(1)
                .map(|b| matches!(b, b' ' | b'\t'))
                .unwrap_or(true);
        if mapping_break && !key.is_empty() && !key.contains(' ') {
            spans.push(painted(BUILTIN, key));
            spans.push(plain(":"));
            let value = &after[1..];
            let leading_ws = value.len() - value.trim_start().len();
            push_yaml_value(value.trim_start(), leading_ws, &mut spans);
            return spans;
        }
    }
    // Otherwise the whole rest is a value.
    push_yaml_value(rest, 0, &mut spans);
    spans
}

fn push_yaml_value(value: &str, leading_ws: usize, spans: &mut Vec<Span<'static>>) {
    if leading_ws > 0 {
        spans.push(plain(&" ".repeat(leading_ws)));
    }
    if value.is_empty() {
        return;
    }
    if value.starts_with('"') || value.starts_with('\'') {
        spans.push(painted(STRING, value));
        return;
    }
    if matches!(value.trim(), "true" | "false" | "null" | "~" | "yes" | "no") {
        spans.push(painted(KEYWORD, value));
        return;
    }
    if value
        .chars()
        .next()
        .map(|c| c.is_ascii_digit() || c == '-')
        .unwrap_or(false)
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
    {
        spans.push(painted(NUMBER, value));
        return;
    }
    spans.push(plain(value));
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

fn highlight_toml_line(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let trimmed_start = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(trimmed_start);
    if !indent.is_empty() {
        spans.push(plain(indent));
    }
    if rest.starts_with('#') {
        spans.push(painted(COMMENT, rest));
        return spans;
    }
    if rest.starts_with('[')
        && let Some(end) = rest.find(']')
    {
        spans.push(painted(KEYWORD, &rest[..=end]));
        if end + 1 < rest.len() {
            spans.push(plain(&rest[end + 1..]));
        }
        return spans;
    }
    if let Some(eq_pos) = rest.find('=') {
        let key = rest[..eq_pos].trim_end();
        let after = &rest[eq_pos..];
        spans.push(painted(BUILTIN, key));
        let key_pad = rest[..eq_pos].len() - key.len();
        if key_pad > 0 {
            spans.push(plain(&" ".repeat(key_pad)));
        }
        spans.push(plain("="));
        let value = &after[1..];
        let value_trimmed = value.trim_start();
        let val_pad = value.len() - value_trimmed.len();
        if val_pad > 0 {
            spans.push(plain(&" ".repeat(val_pad)));
        }
        if value_trimmed.starts_with('"') || value_trimmed.starts_with('\'') {
            spans.push(painted(STRING, value_trimmed));
        } else if matches!(value_trimmed.trim(), "true" | "false") {
            spans.push(painted(KEYWORD, value_trimmed));
        } else if value_trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit() || c == '-')
            .unwrap_or(false)
        {
            spans.push(painted(NUMBER, value_trimmed));
        } else if !value_trimmed.is_empty() {
            spans.push(plain(value_trimmed));
        }
        return spans;
    }
    if !rest.is_empty() {
        spans.push(plain(rest));
    }
    spans
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain concatenated text of a line.
    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Whether any span on `line` carries `content` colored exactly `color`.
    fn has_token(line: &Line<'_>, color: Color, content: &str) -> bool {
        line.spans
            .iter()
            .any(|s| s.style.fg == Some(color) && s.content.as_ref() == content)
    }

    #[test]
    fn unknown_language_flat_default_color() {
        let lines = highlight("hello world", Some("klingon"));
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "hello world");
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.fg == Some(DEFAULT_CODE))
        );
    }

    #[test]
    fn none_language_flat_default_color() {
        let lines = highlight("plain", None);
        assert_eq!(text(&lines[0]), "plain");
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.fg == Some(DEFAULT_CODE))
        );
    }

    #[test]
    fn unknown_language_preserves_every_line() {
        let lines = highlight("one\ntwo\nthree", Some("nope"));
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[2]), "three");
    }

    #[test]
    fn rust_keyword_number_and_type_distinct_colors() {
        let lines = highlight("fn main() { let x: usize = 1; }", Some("rust"));
        let l = &lines[0];
        assert!(has_token(l, KEYWORD, "fn"));
        assert!(has_token(l, KEYWORD, "let"));
        assert!(has_token(l, BUILTIN, "usize"));
        assert!(has_token(l, NUMBER, "1"));
    }

    #[test]
    fn rust_alias_rs_resolves() {
        let lines = highlight("let x = 1;", Some("rs"));
        assert!(has_token(&lines[0], KEYWORD, "let"));
    }

    #[test]
    fn rust_line_comment_paints_rest() {
        let lines = highlight("let x = 1; // trailing", Some("rust"));
        assert!(has_token(&lines[0], COMMENT, "// trailing"));
    }

    #[test]
    fn rust_block_comment_spans_lines_continuously() {
        // Every line of a `/* … */` block is comment-colored, and code resumes
        // (the `let` keyword) only after the close.
        let lines = highlight("/* start\nmiddle\nend */ let x = 1;", Some("rust"));
        assert_eq!(lines.len(), 3);
        assert!(text(&lines[0]).starts_with("/* start"));
        assert!(lines[0].spans.iter().all(|s| s.style.fg == Some(COMMENT)));
        // The whole middle line is one continuous comment span.
        assert!(lines[1].spans.iter().all(|s| s.style.fg == Some(COMMENT)));
        assert_eq!(text(&lines[1]), "middle");
        // Closing line: comment closes, then the `let` keyword reappears.
        assert!(has_token(&lines[2], COMMENT, "end */"));
        assert!(has_token(&lines[2], KEYWORD, "let"));
    }

    #[test]
    fn ts_keyword_and_string() {
        let lines = highlight("const greeting: string = \"hi\";", Some("ts"));
        assert!(has_token(&lines[0], KEYWORD, "const"));
        assert!(has_token(&lines[0], STRING, "\"hi\""));
    }

    #[test]
    fn ts_alias_typescript_and_tsx() {
        assert!(has_token(
            &highlight("const x = 1;", Some("typescript"))[0],
            KEYWORD,
            "const"
        ));
        assert!(has_token(
            &highlight("const x = 1;", Some("tsx"))[0],
            KEYWORD,
            "const"
        ));
    }

    #[test]
    fn js_template_literal_is_string() {
        let lines = highlight("const s = `hi`;", Some("js"));
        assert!(has_token(&lines[0], KEYWORD, "const"));
        assert!(has_token(&lines[0], STRING, "`hi`"));
    }

    #[test]
    fn js_aliases_resolve() {
        for alias in ["js", "jsx", "javascript", "mjs", "cjs"] {
            assert!(
                has_token(&highlight("return 1;", Some(alias))[0], KEYWORD, "return"),
                "alias {alias} failed"
            );
        }
    }

    #[test]
    fn python_keyword_builtin_and_comment() {
        let lines = highlight("def greet(name):  # doc\n    print(name)", Some("python"));
        assert!(has_token(&lines[0], KEYWORD, "def"));
        assert!(has_token(&lines[0], COMMENT, "# doc"));
        assert!(has_token(&lines[1], BUILTIN, "print"));
    }

    #[test]
    fn python_alias_py() {
        assert!(has_token(
            &highlight("return x", Some("py"))[0],
            KEYWORD,
            "return"
        ));
    }

    #[test]
    fn json_key_value_and_number_distinct() {
        let lines = highlight("{\n  \"name\": \"hand\",\n  \"age\": 1\n}", Some("json"));
        assert!(has_token(&lines[1], BUILTIN, "\"name\""));
        assert!(has_token(&lines[1], STRING, "\"hand\""));
        assert!(has_token(&lines[2], NUMBER, "1"));
    }

    #[test]
    fn json_literal_is_keyword() {
        let lines = highlight("{ \"flag\": true }", Some("json"));
        assert!(has_token(&lines[0], KEYWORD, "true"));
    }

    #[test]
    fn bash_keyword_and_variable() {
        let lines = highlight("if [ -n $HOME ]; then echo hi; fi", Some("bash"));
        assert!(has_token(&lines[0], KEYWORD, "if"));
        assert!(has_token(&lines[0], KEYWORD, "then"));
        assert!(has_token(&lines[0], KEYWORD, "fi"));
        assert!(has_token(&lines[0], BUILTIN, "$HOME"));
    }

    #[test]
    fn bash_aliases_resolve() {
        for alias in ["bash", "sh", "shell", "zsh"] {
            assert!(
                has_token(&highlight("echo hi", Some(alias))[0], BUILTIN, "echo"),
                "alias {alias} failed"
            );
        }
    }

    #[test]
    fn bash_comment_paints_rest() {
        let lines = highlight("echo hi # note", Some("sh"));
        assert!(has_token(&lines[0], COMMENT, "# note"));
    }

    #[test]
    fn yaml_key_value_and_number() {
        let lines = highlight("name: hand-ai\nversion: 1", Some("yaml"));
        assert!(has_token(&lines[0], BUILTIN, "name"));
        assert!(has_token(&lines[1], BUILTIN, "version"));
        assert!(has_token(&lines[1], NUMBER, "1"));
    }

    #[test]
    fn yaml_alias_yml_and_comment() {
        let lines = highlight("# comment\nkey: \"v\"", Some("yml"));
        assert!(has_token(&lines[0], COMMENT, "# comment"));
        assert!(has_token(&lines[1], BUILTIN, "key"));
        assert!(has_token(&lines[1], STRING, "\"v\""));
    }

    #[test]
    fn toml_section_and_pair() {
        let lines = highlight("[package]\nname = \"hand\"\nver = 2", Some("toml"));
        assert!(has_token(&lines[0], KEYWORD, "[package]"));
        assert!(has_token(&lines[1], BUILTIN, "name"));
        assert!(has_token(&lines[1], STRING, "\"hand\""));
        assert!(has_token(&lines[2], NUMBER, "2"));
    }

    #[test]
    fn toml_comment_line() {
        let lines = highlight("# a comment", Some("toml"));
        assert!(has_token(&lines[0], COMMENT, "# a comment"));
    }

    #[test]
    fn line_text_is_lossless() {
        // The tokenizer must never drop or duplicate a character.
        let src = "fn main() { let s = \"x\"; /* c */ }";
        let lines = highlight(src, Some("rust"));
        assert_eq!(text(&lines[0]), src);
    }

    #[test]
    fn default_highlighter_round_trips_through_arc() {
        let h = default_highlighter();
        let out = h("let x = 1;", Some("rust"));
        assert!(has_token(&out[0], KEYWORD, "let"));
    }

    #[test]
    fn default_markdown_theme_has_highlighter() {
        let theme = default_markdown_theme();
        assert!(theme.highlight.is_some());
    }

    #[test]
    fn all_categories_use_distinct_colors() {
        // The five token categories plus the flat fallback are pairwise distinct.
        let colors = [KEYWORD, STRING, NUMBER, COMMENT, BUILTIN, DEFAULT_CODE];
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b, "token colors must be distinct");
            }
        }
    }
}
