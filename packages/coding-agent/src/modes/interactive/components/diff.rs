//! Colorised diff renderer with intra-line word highlighting.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/diff.ts`.
//!
//! Takes the unified-style diff text emitted by the edit/write tools (lines
//! prefixed with `+`, `-`, or space and a line number) and styles it for
//! terminal display:
//!
//! * Context lines render with a dim/gray foreground.
//! * Removed lines render in red, with inverse video on tokens that changed
//!   within a single-line modification.
//! * Added lines render in green, with the same inverse highlighting.
//!
//! Intra-line highlighting is only applied when a removal block has exactly
//! one removed and one added line (signalling a single-line modification);
//! larger blocks are shown line-by-line without word-level diffing, matching
//! pi-mono behaviour.
//!
//! Word-level diffing uses the [`similar`] crate. Its `diff_words` tokenizer
//! emits whitespace as separate tokens (npm `diff.diffWords` groups
//! whitespace with adjacent words); the visible output still highlights only
//! changed tokens, which is the property the human reader cares about.
//!
//! Theming caveat: pi-mono reads `toolDiffContext`, `toolDiffRemoved`,
//! `toolDiffAdded` slots from the coding-agent theme. Until the theme port
//! lands (see parent module docs) we hardcode ANSI defaults that match
//! pi-mono's dark theme spirit.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use similar::{ChangeTag, TextDiff};

/// Bright-black (gray) for context lines.
const CONTEXT_FG: &str = "\x1b[90m";
/// Red for removed lines.
const REMOVED_FG: &str = "\x1b[31m";
/// Green for added lines.
const ADDED_FG: &str = "\x1b[32m";
/// Inverse video for highlighted tokens within a changed line.
const INVERSE: &str = "\x1b[7m";
/// Reset.
const RESET: &str = "\x1b[0m";

/// Replace tabs with three spaces, matching pi-mono.
fn replace_tabs(s: &str) -> String {
    s.replace('\t', "   ")
}

/// Parsed components of a diff line: prefix (`+`/`-`/space), an optional line
/// number, and the body content.
struct ParsedLine<'a> {
    prefix: char,
    line_num: &'a str,
    content: &'a str,
}

/// Parse a diff line into prefix / line-number / content. Returns `None` for
/// lines that don't match the expected unified-style format (e.g. file
/// headers, hunk markers).
fn parse_diff_line(line: &str) -> Option<ParsedLine<'_>> {
    // Format: ([+\- ])(\s*\d*)\s(.*)
    let mut chars = line.char_indices();
    let (_, first) = chars.next()?;
    if !matches!(first, '+' | '-' | ' ') {
        return None;
    }

    // Collect spaces and digits for the line number.
    let mut num_end = 1;
    let mut saw_digit_or_space = false;
    let bytes = line.as_bytes();
    while num_end < bytes.len() {
        let b = bytes[num_end];
        if b == b' ' || b.is_ascii_digit() {
            saw_digit_or_space = true;
            num_end += 1;
        } else {
            break;
        }
    }
    if !saw_digit_or_space || num_end >= bytes.len() {
        return None;
    }

    // The character at `num_end - 1` must be a space (the separator between
    // line number and content). Walk back to split the digits from the
    // separator.
    let sep_idx = num_end - 1;
    if bytes[sep_idx] != b' ' {
        return None;
    }

    let line_num = line[1..sep_idx].trim_end_matches(' ');
    let content = &line[num_end..];

    Some(ParsedLine {
        prefix: first,
        line_num,
        content,
    })
}

/// Render the inverse-highlighted versions of a single removed/added line
/// pair using a word-level diff.
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    let diff = TextDiff::configure().diff_words(old_content, new_content);

    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for change in diff.iter_all_changes() {
        let value = change.value();
        match change.tag() {
            ChangeTag::Delete => {
                let value = if is_first_removed {
                    is_first_removed = false;
                    let leading: String = value.chars().take_while(|c| c.is_whitespace()).collect();
                    removed_line.push_str(&leading);
                    &value[leading.len()..]
                } else {
                    value
                };
                if !value.is_empty() {
                    removed_line.push_str(INVERSE);
                    removed_line.push_str(value);
                    removed_line.push_str(RESET);
                }
            }
            ChangeTag::Insert => {
                let value = if is_first_added {
                    is_first_added = false;
                    let leading: String = value.chars().take_while(|c| c.is_whitespace()).collect();
                    added_line.push_str(&leading);
                    &value[leading.len()..]
                } else {
                    value
                };
                if !value.is_empty() {
                    added_line.push_str(INVERSE);
                    added_line.push_str(value);
                    added_line.push_str(RESET);
                }
            }
            ChangeTag::Equal => {
                removed_line.push_str(value);
                added_line.push_str(value);
                // Once we've passed any equal token, leading-whitespace
                // stripping no longer applies on either side.
                is_first_removed = false;
                is_first_added = false;
            }
        }
    }

    (removed_line, added_line)
}

/// Render `diff_text` as a styled string. Each diff line becomes one line of
/// output joined with `\n`, matching pi-mono's `renderDiff` return shape.
pub fn render_diff(diff_text: &str) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let Some(parsed) = parse_diff_line(line) else {
            result.push(format!("{CONTEXT_FG}{line}{RESET}"));
            i += 1;
            continue;
        };

        match parsed.prefix {
            '-' => {
                // Collect consecutive removed lines.
                let mut removed: Vec<(String, String)> = Vec::new();
                while i < lines.len() {
                    let Some(p) = parse_diff_line(lines[i]) else {
                        break;
                    };
                    if p.prefix != '-' {
                        break;
                    }
                    removed.push((p.line_num.to_string(), p.content.to_string()));
                    i += 1;
                }
                // Collect consecutive added lines.
                let mut added: Vec<(String, String)> = Vec::new();
                while i < lines.len() {
                    let Some(p) = parse_diff_line(lines[i]) else {
                        break;
                    };
                    if p.prefix != '+' {
                        break;
                    }
                    added.push((p.line_num.to_string(), p.content.to_string()));
                    i += 1;
                }

                if removed.len() == 1 && added.len() == 1 {
                    let (rnum, rcontent) = &removed[0];
                    let (anum, acontent) = &added[0];
                    let (rline, aline) =
                        render_intra_line_diff(&replace_tabs(rcontent), &replace_tabs(acontent));
                    result.push(format!("{REMOVED_FG}-{rnum} {rline}{RESET}"));
                    result.push(format!("{ADDED_FG}+{anum} {aline}{RESET}"));
                } else {
                    for (num, content) in &removed {
                        result.push(format!(
                            "{REMOVED_FG}-{num} {}{RESET}",
                            replace_tabs(content)
                        ));
                    }
                    for (num, content) in &added {
                        result.push(format!("{ADDED_FG}+{num} {}{RESET}", replace_tabs(content)));
                    }
                }
            }
            '+' => {
                result.push(format!(
                    "{ADDED_FG}+{} {}{RESET}",
                    parsed.line_num,
                    replace_tabs(parsed.content)
                ));
                i += 1;
            }
            _ => {
                // Context line: an extra leading space matches pi-mono's
                // ` ${lineNum} ${content}` template (note the leading space).
                result.push(format!(
                    "{CONTEXT_FG} {} {}{RESET}",
                    parsed.line_num,
                    replace_tabs(parsed.content)
                ));
                i += 1;
            }
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_lines_get_dim_color() {
        let out = render_diff("  1 unchanged");
        assert!(out.contains(CONTEXT_FG), "expected dim FG: {out:?}");
        assert!(out.contains("unchanged"));
    }

    #[test]
    fn standalone_added_line() {
        let out = render_diff("+ 3 hello");
        assert!(out.starts_with(ADDED_FG));
        assert!(out.contains("+3 hello") || out.contains("+ 3 hello"));
        assert!(out.ends_with(RESET));
    }

    #[test]
    fn standalone_removed_line_without_pair() {
        // A removed line followed by something that isn't an added line
        // should render as-is without intra-line diffing.
        let out = render_diff("- 5 deleted\n  6 next");
        assert!(out.contains(REMOVED_FG), "expected red: {out:?}");
        assert!(out.contains("deleted"));
    }

    #[test]
    fn paired_change_uses_intra_line_inverse() {
        let out = render_diff("- 1 hello world\n+ 1 hello rust");
        // Both red and green present.
        assert!(out.contains(REMOVED_FG));
        assert!(out.contains(ADDED_FG));
        // Inverse video applied to changed words.
        assert!(out.contains(INVERSE), "expected inverse SGR: {out:?}");
        // Equal portion ("hello ") appears unmodified somewhere.
        assert!(out.contains("hello"));
    }

    #[test]
    fn multi_line_change_skips_intra_line_diff() {
        let out = render_diff("- 1 a\n- 2 b\n+ 1 a\n+ 2 c");
        // Should not apply inverse video — fall back to plain colored lines.
        assert!(!out.contains(INVERSE));
        assert!(out.contains(REMOVED_FG));
        assert!(out.contains(ADDED_FG));
    }

    #[test]
    fn tabs_are_replaced_with_spaces() {
        let out = render_diff("+ 1 a\tb");
        assert!(out.contains("a   b"), "tabs not replaced: {out:?}");
        assert!(!out.contains('\t'), "tab still present: {out:?}");
    }

    #[test]
    fn lines_without_recognised_prefix_render_as_context() {
        let out = render_diff("@@ hunk @@");
        assert!(out.contains(CONTEXT_FG));
        assert!(out.contains("@@ hunk @@"));
    }
}
