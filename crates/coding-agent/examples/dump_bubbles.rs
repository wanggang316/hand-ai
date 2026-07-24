//! Render the rt-stack message bubbles offline and dump each line with its escape
//! sequences visible. Used to diagnose the "background not solid" complaint — every
//! line painted with a bubble's background SGR should appear with that SGR opening,
//! padded content, and a trailing reset, end to end.
//!
//! The bubbles are the rt-native renderers (`user_bubble_lines`, `tool_box_lines`);
//! each produces owned ratatui [`Line`]s whose spans carry a [`Style`]. This example
//! serializes those styled lines to terminal escape sequences with
//! [`line_to_ansi`], so the diagnostic output is exactly what the rt scheduler would
//! flush for that row: a background SGR at the start of every tinted span and a
//! reset at the end of the line.
//!
//! Run:
//!     cargo run --example dump_bubbles -p hand-coding-agent

use std::fmt::Write as _;

use hand_coding_agent::modes::interactive::rt_driver::messages::user_bubble_lines;
use hand_coding_agent::modes::interactive::rt_driver::tools::{ToolState, tool_box_lines};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use serde_json::json;

/// The SGR reset (`\x1b[0m`) closing every serialized line.
const RESET: &str = "\x1b[0m";

/// Serialize a styled [`Line`] to a terminal string: each span opens its style's
/// SGR codes, then emits the span text; the whole line ends with a reset so a
/// background tint never bleeds past the row.
///
/// The output signature the diagnostic checks for is exactly this: a background SGR
/// (`\x1b[48;…m`) opening a tinted span and a trailing `\x1b[0m` reset.
fn line_to_ansi(line: &Line<'_>) -> String {
    let mut out = String::new();
    for span in &line.spans {
        out.push_str(&style_to_sgr(span.style));
        out.push_str(span.content.as_ref());
        // Reset after each span so an unstyled following span starts clean.
        out.push_str(RESET);
    }
    out
}

/// Build the SGR opening sequence for a ratatui [`Style`]: background first (so the
/// tint is the leading code the diagnostic looks for), then foreground, then the
/// text modifiers. An empty string when the style sets nothing.
fn style_to_sgr(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    if let Some(bg) = style.bg {
        codes.push(bg_code(bg));
    }
    if let Some(fg) = style.fg {
        codes.push(fg_code(fg));
    }
    for m in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
    ] {
        if style.add_modifier.contains(m.0) {
            codes.push(m.1.to_string());
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        let mut seq = String::from("\x1b[");
        let mut first = true;
        for code in codes {
            if !first {
                seq.push(';');
            }
            // A background/foreground code is already a full `48;…` / `38;…` run, but
            // the modifier codes are bare digits; join everything with `;` under one
            // escape so the line stays compact and readable in the DBG dump.
            let _ = write!(seq, "{code}");
            first = false;
        }
        seq.push('m');
        seq
    }
}

/// The `48;…` background body for a ratatui color (no leading `\x1b[`, no trailing
/// `m` — [`style_to_sgr`] joins the runs under one escape).
fn bg_code(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
        Color::Indexed(i) => format!("48;5;{i}"),
        other => named_code(other, 40, 100).unwrap_or_else(|| "49".to_string()),
    }
}

/// The `38;…` foreground body for a ratatui color.
fn fg_code(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        Color::Indexed(i) => format!("38;5;{i}"),
        other => named_code(other, 30, 90).unwrap_or_else(|| "39".to_string()),
    }
}

/// Map a named ratatui color to its SGR code, offset by `base` for the eight
/// standard colors and `bright` for the eight bright ones. `None` for a color that
/// is not one of the sixteen named ones (Rgb / Indexed / Reset are handled by the
/// callers).
fn named_code(color: Color, base: u8, bright: u8) -> Option<String> {
    let code = match color {
        Color::Black => base,
        Color::Red => base + 1,
        Color::Green => base + 2,
        Color::Yellow => base + 3,
        Color::Blue => base + 4,
        Color::Magenta => base + 5,
        Color::Cyan => base + 6,
        Color::Gray => base + 7,
        Color::DarkGray => bright,
        Color::LightRed => bright + 1,
        Color::LightGreen => bright + 2,
        Color::LightYellow => bright + 3,
        Color::LightBlue => bright + 4,
        Color::LightMagenta => bright + 5,
        Color::LightCyan => bright + 6,
        Color::White => bright + 7,
        _ => return None,
    };
    Some(code.to_string())
}

/// Print a labelled block of styled lines: each row twice — a VIS line the terminal
/// renders, and a DBG line with every ESC shown as `\e` so the raw escapes are
/// inspectable.
fn dump(label: &str, lines: &[Line<'_>]) {
    println!("==== {label} ({} lines) ====", lines.len());
    for (i, line) in lines.iter().enumerate() {
        let ansi = line_to_ansi(line);
        // Visible — what the terminal renders (already reset-terminated).
        println!("  L{i:02} VIS: {ansi}");
        // Debug — the raw bytes, with ESC printed as `\e`.
        let dbg = ansi.replace('\x1b', "\\e");
        println!("  L{i:02} DBG: {dbg}");
    }
}

fn main() {
    let width = 80u16;

    dump("user 你好", &user_bubble_lines("你好", width));
    dump("user hi @ width 80", &user_bubble_lines("hi", 80));
    dump("user hi @ width 30", &user_bubble_lines("hi", 30));

    let error = tool_box_lines(
        "ls",
        &json!(""),
        "Invalid arguments for tool 'ls': \"\" is not of type \"object\" (path: )",
        ToolState::from_result(Some(true)),
        width,
    );
    dump("tool ls (error)", &error);
}
