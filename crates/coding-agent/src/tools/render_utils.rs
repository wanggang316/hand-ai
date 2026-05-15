//! Display-side helpers for tool output rendering.
//!
//! Provides four display utilities used across the various tool
//! renderers:
//!
//! - [`shorten_path`]: replace `$HOME` with `~` for cosmetics in chat
//!   logs and tool labels.
//! - [`replace_tabs`]: a one-liner that replaces tabs with three spaces
//!   so terminal width math doesn't blow up on heterogeneous content.
//! - [`normalize_display_text`]: strip `\r` so Windows-style line endings
//!   render cleanly in the TUI.
//! - [`get_text_output`]: collapse a `Vec<ToolResultContent>` into a
//!   single display string. Honours image-protocol capabilities — when
//!   the terminal lacks Kitty/iTerm2 support or the caller requests
//!   text-only, image blocks are summarised with [`hand_tui::image_fallback`]
//!   placeholders rather than dropped silently.
//!
//! Binary-output sanitisation drops C0 control characters (everything
//! below 0x20 except `\t \n \r`) and the U+FFF9..U+FFFB Unicode format
//! characters that crash terminal-width libraries. Bash tool output
//! can carry BEL, VT, FF, or other binary garbage from poorly-behaved
//! processes; [`get_text_output`] scrubs it so the TUI scrollback never
//! has to render it (and so an attacker cannot smuggle terminal-control
//! sequences into the chat log).

use hand_tui::utils::strip_ansi;
use hand_tui::{ImageDimensions, ImageRenderOptions, TerminalImageCapabilities, image_fallback};
use model::types::ToolResultContent;

/// Replace a leading `$HOME` with `~` for compact display.
///
/// Returns the original string when the path does not start at the home
/// directory or when the home directory cannot be determined.
pub fn shorten_path(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home_str = home.to_string_lossy();
    if path.starts_with(home_str.as_ref()) {
        format!("~{}", &path[home_str.len()..])
    } else {
        path.to_string()
    }
}

/// Replace tabs with three spaces. Mirrors the TS implementation; the
/// width is hard-coded both ends.
pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// Strip `\r` for clean terminal display. The TS port keeps newlines
/// intact and only removes carriage returns.
pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

/// Drop C0 control characters (except `\t \n \r`) and Unicode format
/// characters U+FFF9..U+FFFB. These come from poorly-behaved
/// subprocesses and either crash terminal-width libraries or smuggle
/// terminal-control sequences into the chat log.
///
/// Centralised here because both the bash executor and the tool-result
/// renderer need the same filter applied to their output.
pub fn sanitize_binary_output(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            let code = c as u32;
            if code == 0x09 || code == 0x0A || code == 0x0D {
                return true;
            }
            if code <= 0x1F {
                return false;
            }
            if (0xFFF9..=0xFFFB).contains(&code) {
                return false;
            }
            true
        })
        .collect()
}

/// Decide whether an image block should be rendered inline or replaced
/// with a textual placeholder.
fn images_should_render(caps: &TerminalImageCapabilities, show_images: bool) -> bool {
    show_images && (caps.kitty || caps.iterm2)
}

/// Render a list of [`ToolResultContent`] blocks to a single textual
/// payload suitable for the chat scrollback.
///
/// `show_images` follows the TS contract: when `true` *and* the terminal
/// reports a graphics protocol, image blocks are excluded from the text
/// output (the caller is expected to render them out-of-band via
/// [`hand_tui::render_image`]). Otherwise each image is replaced with a
/// labelled box drawn by [`image_fallback`].
///
/// The TS implementation pipes text through `sanitizeBinaryOutput`. The
/// Rust port currently only strips ANSI and removes `\r`; see the
/// module-level TODO.
pub fn get_text_output(
    content: &[ToolResultContent],
    show_images: bool,
    caps: &TerminalImageCapabilities,
) -> String {
    let mut text_parts: Vec<String> = Vec::new();
    let mut image_blocks: Vec<&model::types::ImageContent> = Vec::new();

    for block in content {
        match block {
            ToolResultContent::Text(t) => {
                // Pipeline: sanitize_binary_output → strip ANSI → drop \r.
                // The strip step has to follow sanitize so the C0 filter
                // sees the bare bytes; \r is dropped last because some
                // ANSI escapes carry CR as part of their payload.
                let cleaned = sanitize_binary_output(&strip_ansi(&t.text)).replace('\r', "");
                text_parts.push(cleaned);
            }
            ToolResultContent::Image(img) => image_blocks.push(img),
        }
    }

    let mut output = text_parts.join("\n");

    if !image_blocks.is_empty() && !images_should_render(caps, show_images) {
        let mut indicators: Vec<String> = Vec::new();
        for img in &image_blocks {
            // Try to read dimensions from the base64 payload so the
            // fallback box scales to roughly the right footprint.
            let bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                img.data.as_bytes(),
            )
            .ok();
            let dims = bytes.as_deref().and_then(hand_tui::get_image_dimensions);
            indicators.push(image_fallback_indicator(&img.mime_type, dims));
        }
        let joined = indicators.join("\n");
        if output.is_empty() {
            output = joined;
        } else {
            output = format!("{output}\n{joined}");
        }
    }

    output
}

/// Build a single-line image fallback indicator, mirroring the TS
/// `imageFallback(mimeType, dims)` shape: a box that includes the MIME
/// type and (when known) the pixel dimensions in its label.
fn image_fallback_indicator(mime_type: &str, dims: Option<ImageDimensions>) -> String {
    let label = match dims {
        Some(d) => format!("[{} {}x{}]", mime_type, d.width, d.height),
        None => format!("[{}]", mime_type),
    };
    let opts = ImageRenderOptions {
        label: Some(label),
        ..ImageRenderOptions::default()
    };
    image_fallback(&opts).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hand_tui::CellDimensions;
    use model::types::{ImageContent, TextContent};

    fn fallback_caps() -> TerminalImageCapabilities {
        TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions {
                width: 8,
                height: 16,
            },
        }
    }

    fn graphics_caps() -> TerminalImageCapabilities {
        TerminalImageCapabilities {
            kitty: true,
            iterm2: false,
            cell_dimensions: CellDimensions {
                width: 8,
                height: 16,
            },
        }
    }

    #[test]
    fn shorten_path_replaces_home_with_tilde() {
        let home = dirs::home_dir().expect("home dir");
        let input = home.join("project/file.rs");
        let result = shorten_path(&input.to_string_lossy());
        assert_eq!(result, "~/project/file.rs");
    }

    #[test]
    fn shorten_path_leaves_other_paths_alone() {
        let result = shorten_path("/etc/hosts");
        assert_eq!(result, "/etc/hosts");
    }

    #[test]
    fn replace_tabs_uses_three_spaces() {
        let result = replace_tabs("a\tb\tc");
        assert_eq!(result, "a   b   c");
    }

    #[test]
    fn replace_tabs_handles_no_tabs() {
        let result = replace_tabs("plain text");
        assert_eq!(result, "plain text");
    }

    #[test]
    fn normalize_display_text_strips_cr() {
        let result = normalize_display_text("line1\r\nline2\r\n");
        assert_eq!(result, "line1\nline2\n");
    }

    #[test]
    fn get_text_output_concatenates_text_blocks() {
        let blocks = vec![
            ToolResultContent::Text(TextContent::new("hello")),
            ToolResultContent::Text(TextContent::new("world")),
        ];
        let caps = fallback_caps();
        let result = get_text_output(&blocks, false, &caps);
        assert_eq!(result, "hello\nworld");
    }

    #[test]
    fn get_text_output_strips_ansi_escapes() {
        let blocks = vec![ToolResultContent::Text(TextContent::new(
            "\x1b[31mred\x1b[0m text",
        ))];
        let caps = fallback_caps();
        let result = get_text_output(&blocks, false, &caps);
        assert_eq!(result, "red text");
    }

    /// Tool result rendering must also strip C0 control chars (BEL
    /// etc.) and Unicode format chars from text blocks. Without this,
    /// the TUI scrollback could render raw 0x07 or 0xFFF9, which a
    /// misbehaving tool could exploit to corrupt the user's terminal
    /// or embed prompt-injection sequences in the model's view.
    #[test]
    fn get_text_output_strips_c0_controls_and_format_chars() {
        let blocks = vec![ToolResultContent::Text(TextContent::new(
            "pre\x07\x0B\x0C\u{FFF9}\u{FFFB}mid\tpost",
        ))];
        let caps = fallback_caps();
        let result = get_text_output(&blocks, false, &caps);
        // Tab survives (whitespace); BEL/VT/FF and format chars are gone.
        assert_eq!(result, "premid\tpost");
    }

    #[test]
    fn sanitize_binary_output_pure_helper() {
        // Whitespace passes through, other C0 controls dropped.
        assert_eq!(sanitize_binary_output("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(sanitize_binary_output("\x01\x02hello\x1F"), "hello");
        // Unicode format chars dropped; adjacent code points preserved.
        assert_eq!(
            sanitize_binary_output("x\u{FFF8}y\u{FFF9}z\u{FFFB}w\u{FFFC}"),
            "x\u{FFF8}yzw\u{FFFC}"
        );
        // DEL (0x7F) is not in the C0 range — keep it (pi parity).
        assert_eq!(sanitize_binary_output("a\x7Fb"), "a\x7Fb");
    }

    #[test]
    fn get_text_output_emits_fallback_indicator_for_images_when_no_protocol() {
        let blocks = vec![
            ToolResultContent::Text(TextContent::new("desc")),
            ToolResultContent::Image(ImageContent::new("aGVsbG8=", "image/png")),
        ];
        let caps = fallback_caps();
        let result = get_text_output(&blocks, true, &caps);
        assert!(result.contains("desc"));
        // Fallback box renders the MIME type as part of the label.
        assert!(result.contains("image/png"), "result was: {result}");
    }

    #[test]
    fn get_text_output_skips_indicator_when_graphics_supported_and_show_images() {
        let blocks = vec![
            ToolResultContent::Text(TextContent::new("desc")),
            ToolResultContent::Image(ImageContent::new("aGVsbG8=", "image/png")),
        ];
        let caps = graphics_caps();
        let result = get_text_output(&blocks, true, &caps);
        // Image is expected to be rendered out-of-band; text output
        // contains only the text block.
        assert_eq!(result, "desc");
    }

    #[test]
    fn get_text_output_filters_non_text_when_no_protocol_and_images_hidden() {
        let blocks = vec![
            ToolResultContent::Text(TextContent::new("only text")),
            ToolResultContent::Image(ImageContent::new("aGVsbG8=", "image/png")),
        ];
        let caps = fallback_caps();
        let result = get_text_output(&blocks, false, &caps);
        // Image still renders a fallback box because the terminal can't
        // show graphics — that matches the TS behaviour.
        assert!(result.starts_with("only text"));
        assert!(result.contains("image/png"));
    }
}
