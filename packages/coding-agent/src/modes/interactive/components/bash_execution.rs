//! Bash execution component — streaming command output with framing.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/bash-execution.ts`.
//!
//! Models a long-running shell command in the interactive UI:
//!
//! * A header `$ <command>` line.
//! * Streaming output appended via [`Self::append_output`], with ANSI codes
//!   stripped and `\r\n` / `\r` normalised.
//! * Top and bottom borders that adapt to the terminal width.
//! * A loader frame while the command is running, replaced by status text
//!   (cancelled / exit code / truncation note) once
//!   [`Self::set_complete`] is called.
//! * An expand/collapse toggle: when collapsed the last
//!   [`PREVIEW_LINES`] visual lines are shown; when expanded the full
//!   (post-context-truncation) buffer is shown.
//!
//! Theming caveat: pi-mono reads `bashMode`, `dim`, `muted`, `error`,
//! `warning` slots from the coding-agent theme. Until that theme system is
//! ported (see parent module docs) we hardcode ANSI defaults matching the
//! dark-theme spirit. The `bashMode` accent is cyan (matching pi-mono's
//! dark `bashMode` slot), and the `dim` border is bright black.
//!
//! TODO(parity): theme integration deferred — see
//! docs/exec-plans/parity-completion.md §A1.

use hand_tui::utils::strip_ansi;
use hand_tui::{Component, LoaderComponent, TextComponent};

use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::{key_hint_for, key_text};
use super::visual_truncate::truncate_to_visual_lines;

/// Preview line limit when collapsed — matches pi-mono's tool execution.
pub const PREVIEW_LINES: usize = 20;

/// LLM-context line limit (from pi-mono's `DEFAULT_MAX_LINES`).
const CONTEXT_MAX_LINES: usize = 2000;
/// LLM-context byte limit (from pi-mono's `DEFAULT_MAX_BYTES` = 50KiB).
const CONTEXT_MAX_BYTES: usize = 50 * 1024;

/// ANSI cyan, used for the command header and (default) borders.
const BASH_FG: &str = "\x1b[36m";
/// Bright black, used for muted output text and dim borders.
const MUTED_FG: &str = "\x1b[90m";
/// Yellow, used for cancellation / truncation notices.
const WARNING_FG: &str = "\x1b[33m";
/// Red, used for error exit codes.
const ERROR_FG: &str = "\x1b[31m";
/// Bold SGR.
const BOLD: &str = "\x1b[1m";
/// Reset.
const RESET: &str = "\x1b[0m";

/// Execution status reported by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

/// Component rendering a bash execution frame.
pub struct BashExecutionComponent {
    command: String,
    /// Accumulated output (ANSI-stripped, `\r\n`/`\r` normalised), one line
    /// per element. The last element may continue when the next chunk lacks
    /// a leading newline.
    output_lines: Vec<String>,
    status: BashStatus,
    exit_code: Option<i32>,
    expanded: bool,
    /// True when the command is excluded from the LLM context (`!!` prefix);
    /// switches the border color to `dim` instead of the bash accent.
    exclude_from_context: bool,
    /// Optional path to a file holding the full output; surfaced in the
    /// footer when context truncation kicked in.
    full_output_path: Option<String>,
    /// Loader rendered while `status` is `Running`.
    loader: LoaderComponent,
}

impl BashExecutionComponent {
    /// Construct a renderer for `command`. `exclude_from_context` toggles the
    /// dim border treatment used for the `!!` prefix.
    pub fn new(command: impl Into<String>, exclude_from_context: bool) -> Self {
        let cancel_key = key_text("tui.select.cancel");
        let cancel_key = if cancel_key.is_empty() {
            "esc".to_string()
        } else {
            cancel_key
        };
        let loader = LoaderComponent::new(format!("Running... ({cancel_key} to cancel)"));
        Self {
            command: command.into(),
            output_lines: Vec::new(),
            status: BashStatus::Running,
            exit_code: None,
            expanded: false,
            exclude_from_context,
            full_output_path: None,
            loader,
        }
    }

    /// Append a chunk of output. The chunk is sanitised (ANSI stripped, `\r\n`
    /// normalised to `\n`, bare `\r` normalised to `\n`) and appended to the
    /// last logical line if it had no newline ahead of it.
    pub fn append_output(&mut self, chunk: &str) {
        let clean = strip_ansi(chunk).replace("\r\n", "\n").replace('\r', "\n");
        if clean.is_empty() {
            return;
        }
        let new_lines: Vec<&str> = clean.split('\n').collect();
        if let Some(last) = self.output_lines.last_mut()
            && let Some(first_chunk) = new_lines.first()
        {
            last.push_str(first_chunk);
            self.output_lines
                .extend(new_lines.iter().skip(1).map(|s| s.to_string()));
        } else {
            self.output_lines
                .extend(new_lines.iter().map(|s| s.to_string()));
        }
    }

    /// Mark the command as complete. `cancelled` overrides any non-zero exit
    /// code with the cancelled status. `full_output_path` (if any) is shown in
    /// the truncation notice.
    pub fn set_complete(
        &mut self,
        exit_code: Option<i32>,
        cancelled: bool,
        full_output_path: Option<String>,
    ) {
        self.exit_code = exit_code;
        self.status = if cancelled {
            BashStatus::Cancelled
        } else if matches!(exit_code, Some(c) if c != 0) {
            BashStatus::Error
        } else {
            BashStatus::Complete
        };
        self.full_output_path = full_output_path;
    }

    /// Toggle the expanded view.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// Whether the renderer is currently expanded.
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Raw output as a single string (mirrors pi-mono's `getOutput()`).
    pub fn output(&self) -> String {
        self.output_lines.join("\n")
    }

    /// Command that was executed.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Status of the execution.
    pub fn status(&self) -> BashStatus {
        self.status
    }

    /// Advance the loader frame. No-op when not running.
    pub fn tick(&mut self) {
        if self.status == BashStatus::Running {
            self.loader.tick();
        }
    }

    /// ANSI prefix used for borders / header / accent text.
    fn accent_fg(&self) -> &'static str {
        if self.exclude_from_context {
            MUTED_FG
        } else {
            BASH_FG
        }
    }
}

impl Component for BashExecutionComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let accent = self.accent_fg();
        let border = DynamicBorderComponent::with_color(accent.to_string());

        // Apply context-level truncation up front so the displayed buffer
        // matches what would actually be sent to the model.
        let full_output = self.output_lines.join("\n");
        let (context_content, context_truncated) =
            truncate_tail_simple(&full_output, CONTEXT_MAX_LINES, CONTEXT_MAX_BYTES);
        let available_lines: Vec<&str> = if context_content.is_empty() {
            Vec::new()
        } else {
            context_content.split('\n').collect()
        };

        // Apply preview truncation (collapsed) or pass through (expanded).
        let preview_lines: Vec<&str> = if self.expanded {
            available_lines.clone()
        } else if available_lines.len() > PREVIEW_LINES {
            available_lines
                .iter()
                .skip(available_lines.len() - PREVIEW_LINES)
                .copied()
                .collect()
        } else {
            available_lines.clone()
        };
        let hidden_logical = available_lines.len() - preview_lines.len();

        // Build lines directly. The TS version stuffs everything into a
        // Container; here it's clearer and avoids needing to clone the
        // already-stateful loader.
        let mut out: Vec<String> = Vec::new();
        out.push(String::new());
        out.extend(border.render(width));

        // Header line: `$ <command>` with bold accent and one cell of left
        // padding (matching the TS `paddingX = 1`).
        let header = format!("{accent}{BOLD}$ {}{RESET}", self.command);
        out.extend(TextComponent::new(header).with_padding(1, 0).render(width));

        // Output body.
        if !preview_lines.is_empty() {
            let styled: String = preview_lines
                .iter()
                .map(|line| format!("{MUTED_FG}{line}{RESET}"))
                .collect::<Vec<_>>()
                .join("\n");
            let body_text = if self.expanded {
                format!("\n{styled}")
            } else {
                let truncated =
                    truncate_to_visual_lines(&format!("\n{styled}"), PREVIEW_LINES, width, 1);
                truncated.visual_lines.join("\n")
            };
            out.extend(
                TextComponent::new(body_text)
                    .with_padding(1, 0)
                    .render(width),
            );
        }

        // Footer: loader while running, else status text.
        if matches!(self.status, BashStatus::Running) {
            out.extend(self.loader.render(width));
        } else {
            let mut parts: Vec<String> = Vec::new();

            if hidden_logical > 0 {
                let hint = if self.expanded {
                    format!("({})", key_hint_for("app.tools.expand", "to collapse"))
                } else {
                    format!(
                        "{MUTED_FG}... {hidden_logical} more lines{RESET} ({})",
                        key_hint_for("app.tools.expand", "to expand")
                    )
                };
                parts.push(hint);
            }

            match self.status {
                BashStatus::Cancelled => parts.push(format!("{WARNING_FG}(cancelled){RESET}")),
                BashStatus::Error => {
                    let code = self
                        .exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    parts.push(format!("{ERROR_FG}(exit {code}){RESET}"));
                }
                _ => {}
            }

            if context_truncated && let Some(path) = &self.full_output_path {
                parts.push(format!(
                    "{WARNING_FG}Output truncated. Full output: {path}{RESET}"
                ));
            }

            if !parts.is_empty() {
                let body = format!("\n{}", parts.join("\n"));
                out.extend(TextComponent::new(body).with_padding(1, 0).render(width));
            }
        }

        out.extend(DynamicBorderComponent::with_color(accent.to_string()).render(width));
        out
    }
}

/// Minimal tail truncator: mirrors pi-mono's `truncateTail` only as far as
/// needed by this renderer (post-truncation content + a `truncated` flag).
///
/// Walks backward from the end, accumulating lines until either the line
/// limit or byte limit (counting newline separators) is hit.
fn truncate_tail_simple(content: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();
    let total_bytes = content.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return (content.to_string(), false);
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    for line in lines.iter().rev() {
        if kept.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + if kept.is_empty() { 0 } else { 1 };
        if bytes + line_bytes > max_bytes {
            break;
        }
        kept.push(line);
        bytes += line_bytes;
    }
    kept.reverse();
    (kept.join("\n"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_header_and_borders_while_running() {
        let comp = BashExecutionComponent::new("ls -la", false);
        let lines = comp.render(40);
        let joined = lines.join("\n");
        assert!(joined.contains("$ ls -la"), "missing header: {joined:?}");
        assert!(joined.contains('─'), "missing border");
        // Loader text contains "Running...".
        assert!(joined.contains("Running"), "missing loader: {joined:?}");
    }

    #[test]
    fn append_output_normalises_and_strips_ansi() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        comp.append_output("hello\r\n\x1b[31mworld\x1b[0m\r");
        let buf = comp.output();
        assert!(buf.contains("hello"));
        assert!(buf.contains("world"));
        assert!(!buf.contains("\x1b["), "ANSI not stripped: {buf:?}");
        assert!(!buf.contains('\r'), "CR not normalised: {buf:?}");
    }

    #[test]
    fn append_output_continues_partial_last_line() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        comp.append_output("hello ");
        comp.append_output("world\nnext");
        let buf = comp.output();
        assert_eq!(buf, "hello world\nnext");
    }

    #[test]
    fn complete_with_zero_exit_marks_complete() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        comp.set_complete(Some(0), false, None);
        assert_eq!(comp.status(), BashStatus::Complete);
    }

    #[test]
    fn complete_with_nonzero_exit_marks_error_and_renders_code() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        comp.set_complete(Some(2), false, None);
        assert_eq!(comp.status(), BashStatus::Error);
        let joined = comp.render(40).join("\n");
        assert!(joined.contains("exit 2"), "missing exit code: {joined:?}");
    }

    #[test]
    fn cancelled_overrides_exit_code() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        comp.set_complete(Some(1), true, None);
        assert_eq!(comp.status(), BashStatus::Cancelled);
        let joined = comp.render(40).join("\n");
        assert!(joined.contains("cancelled"), "{joined:?}");
    }

    #[test]
    fn collapsed_view_indicates_hidden_lines() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        for i in 0..30 {
            comp.append_output(&format!("line {i}\n"));
        }
        comp.set_complete(Some(0), false, None);
        let joined = comp.render(80).join("\n");
        assert!(
            joined.contains("more lines"),
            "expected hidden-lines hint: {joined:?}"
        );
    }

    #[test]
    fn excluded_from_context_uses_dim_border() {
        let comp = BashExecutionComponent::new("cmd", true);
        let joined = comp.render(20).join("\n");
        // Border line should use the muted prefix, not the bash cyan.
        assert!(joined.contains(MUTED_FG));
    }

    #[test]
    fn full_output_path_appears_when_context_truncated() {
        let mut comp = BashExecutionComponent::new("cmd", false);
        // Push enough output to exceed the byte limit: 60KB > 50KB default.
        let big = "x".repeat(CONTEXT_MAX_BYTES + 10_000);
        comp.append_output(&big);
        comp.set_complete(Some(0), false, Some("/tmp/full.log".to_string()));
        let joined = comp.render(80).join("\n");
        assert!(
            joined.contains("/tmp/full.log"),
            "expected truncation footer: {joined:?}"
        );
    }
}
