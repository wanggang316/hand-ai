//! Generic tool-execution component (fallback path only).
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/tool-execution.ts`.
//!
//! pi-mono's full `ToolExecutionComponent` orchestrates three parallel
//! rendering paths:
//!
//! 1. A built-in or extension-registered call/result renderer with optional
//!    `default`/`self` shells.
//! 2. An image-block renderer for `image/*` MCP content (with on-the-fly
//!    PNG conversion for kitty-protocol terminals).
//! 3. A generic textual fallback used when no renderer is registered.
//!
//! Paths #1 and #2 require infrastructure that is not yet ported to the Rust
//! crate: the `ToolDefinition` / `ToolRenderContext` extension surface, the
//! `createAllToolDefinitions` registry, the kitty-protocol image pipeline,
//! and the [`hand_tui::ImageComponent`] driver.
//!
//! This Rust port covers **path #3 only** — the fallback rendering — which
//! is what `formatToolExecution()` produces in pi-mono. It boxes the tool
//! name in a tinted background that flips between three states (pending /
//! error / success), shows the args as pretty-printed JSON, and appends any
//! text content from the [`ToolResult`].
//!
//! TODO(parity): once the extension surface and tool registry land, extend
//! this component to dispatch to registered renderers. See
//! docs/exec-plans/parity-completion.md §A1.
//!
//! Theming caveat: pi-mono reads `toolPendingBg`, `toolErrorBg`,
//! `toolSuccessBg`, `toolTitle`, `toolOutput` slots from the coding-agent
//! theme. Until that theme system is ported (see parent module docs) we
//! hardcode 256-color ANSI defaults that approximate the dark-theme spirit.

use hand_agent::types::ToolResult;
use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{BoxComponent, Color, Component, NamedColor, TextComponent};
use model::ToolResultContent;
use serde_json::Value;

/// Background ANSI for an in-flight tool call (muted gray-blue).
const PENDING_BG: &str = "\x1b[48;5;236m";
/// Background ANSI for a failed tool call (muted dark red).
const ERROR_BG: &str = "\x1b[48;5;52m";
/// Background ANSI for a successful tool call (muted dark green).
const SUCCESS_BG: &str = "\x1b[48;5;22m";
/// Bold + bright cyan title color.
const TITLE_FG: &str = "\x1b[1m\x1b[96m";
/// Reset.
const RESET: &str = "\x1b[0m";

/// Lifecycle status of the tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionStatus {
    /// Args still streaming or the tool is executing.
    #[default]
    Pending,
    /// Result received, no `is_error` flag.
    Complete,
    /// Result received with `is_error = true`.
    Error,
}

/// Generic, renderer-less tool-execution renderer.
///
/// Mirrors pi-mono's `formatToolExecution()` fallback path: a tinted box
/// containing `<bold tool name>\n\n<args JSON>\n<output text>`.
pub struct ToolExecutionComponent {
    tool_name: String,
    args: Value,
    status: ToolExecutionStatus,
    /// Latest result, or `None` while still executing.
    result: Option<ToolResult>,
}

impl ToolExecutionComponent {
    /// Construct a renderer for a freshly-emitted tool call.
    pub fn new(tool_name: impl Into<String>, args: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            args,
            status: ToolExecutionStatus::Pending,
            result: None,
        }
    }

    /// Replace the args (use while streaming partial JSON).
    pub fn set_args(&mut self, args: Value) {
        self.args = args;
    }

    /// Stash a streaming partial result without flipping the lifecycle
    /// status. The generic renderer overwrites the recorded output but
    /// keeps the pending background until [`Self::set_result`] is called.
    pub fn set_partial_result(&mut self, partial: ToolResult) {
        self.result = Some(partial);
        self.status = ToolExecutionStatus::Pending;
    }

    /// Apply a final result. `is_error` mirrors the flag the agent loop
    /// records on the corresponding [`model::ToolResultMessage`] — it lives
    /// on the message, not on [`ToolResult`] itself.
    pub fn set_result(&mut self, result: ToolResult, is_error: bool) {
        self.status = if is_error {
            ToolExecutionStatus::Error
        } else {
            ToolExecutionStatus::Complete
        };
        self.result = Some(result);
    }

    /// Tool name.
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Current status.
    pub fn status(&self) -> ToolExecutionStatus {
        self.status
    }

    /// Whether a result has been recorded.
    pub fn has_result(&self) -> bool {
        self.result.is_some()
    }

    fn background_ansi(&self) -> &'static str {
        match self.status {
            ToolExecutionStatus::Pending => PENDING_BG,
            ToolExecutionStatus::Error => ERROR_BG,
            ToolExecutionStatus::Complete => SUCCESS_BG,
        }
    }

    /// Concatenate the text portions of the recorded result (image blocks
    /// are dropped here — that's the fallback path's behaviour). Empty when
    /// no result is recorded yet.
    fn text_output(&self) -> String {
        let Some(r) = &self.result else {
            return String::new();
        };
        let mut parts = Vec::new();
        for block in &r.content {
            if let ToolResultContent::Text(t) = block {
                parts.push(t.text.clone());
            }
        }
        parts.join("\n")
    }
}

impl Component for ToolExecutionComponent {
    fn render(&self, width: u16) -> Vec<String> {
        // Render args as pretty JSON, but skip the section entirely when args
        // are an empty object — matches pi-mono's `JSON.stringify(args)` which
        // would otherwise render a bare `{}`.
        let args_text = match &self.args {
            Value::Object(map) if map.is_empty() => String::new(),
            _ => serde_json::to_string_pretty(&self.args).unwrap_or_default(),
        };
        let output_text = self.text_output();

        let mut body = format!("{TITLE_FG}{}{RESET}", self.tool_name);
        if !args_text.is_empty() {
            body.push_str("\n\n");
            body.push_str(&args_text);
        }
        if !output_text.is_empty() {
            body.push('\n');
            body.push_str(&output_text);
        }

        let bg = self.background_ansi();
        let mut bx = BoxComponent::new().with_padding(1, 1).with_background(bg);
        let text = TextComponent::new(body).with_padding(0, 0).with_bg_code(bg);
        bx.add_child(Box::new(text));
        bx.render(width)
    }
}

/// Wrap [`TextComponent`] with markdown-style default text style. Used by
/// future renderer-aware code paths; kept here so the public API survives
/// the eventual extension wiring without churn.
#[allow(dead_code)]
fn _markdown_style() -> DefaultTextStyle {
    DefaultTextStyle {
        fg: Some(Color::Named(NamedColor::White)),
        bg: None,
        italic: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_status_uses_pending_background() {
        let comp = ToolExecutionComponent::new("read", json!({"path": "/x"}));
        assert_eq!(comp.status(), ToolExecutionStatus::Pending);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains(PENDING_BG), "{joined:?}");
    }

    #[test]
    fn complete_status_uses_success_background() {
        let mut comp = ToolExecutionComponent::new("read", json!({}));
        comp.set_result(ToolResult::text("ok"), false);
        assert_eq!(comp.status(), ToolExecutionStatus::Complete);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains(SUCCESS_BG), "{joined:?}");
        assert!(joined.contains("ok"));
    }

    #[test]
    fn error_status_uses_error_background() {
        let mut comp = ToolExecutionComponent::new("read", json!({}));
        comp.set_result(ToolResult::error("bad"), true);
        assert_eq!(comp.status(), ToolExecutionStatus::Error);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains(ERROR_BG), "{joined:?}");
        assert!(joined.contains("bad"));
    }

    #[test]
    fn renders_tool_name_in_bold() {
        let comp = ToolExecutionComponent::new("bash", json!({"command": "ls"}));
        let joined = comp.render(60).join("\n");
        assert!(joined.contains(TITLE_FG));
        assert!(joined.contains("bash"));
    }

    #[test]
    fn renders_args_as_pretty_json() {
        let comp = ToolExecutionComponent::new("bash", json!({"command": "ls"}));
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("\"command\""));
        assert!(joined.contains("\"ls\""));
    }

    #[test]
    fn empty_args_skip_json_section() {
        let comp = ToolExecutionComponent::new("noop", json!({}));
        let joined = comp.render(60).join("\n");
        assert!(
            !joined.contains("{}"),
            "should not render empty object: {joined:?}"
        );
    }

    #[test]
    fn set_args_updates_subsequent_renders() {
        let mut comp = ToolExecutionComponent::new("bash", json!({}));
        let pre = comp.render(60).join("\n");
        comp.set_args(json!({"command": "echo hi"}));
        let post = comp.render(60).join("\n");
        assert!(!pre.contains("echo hi"));
        assert!(post.contains("echo hi"));
    }

    #[test]
    fn set_partial_result_keeps_pending_status_and_renders_text() {
        let mut comp = ToolExecutionComponent::new("read", json!({}));
        comp.set_partial_result(ToolResult::text("streaming chunk"));
        assert_eq!(comp.status(), ToolExecutionStatus::Pending);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("streaming chunk"));
        assert!(joined.contains(PENDING_BG), "should keep pending bg");
    }

    #[test]
    fn text_blocks_are_concatenated_image_blocks_dropped() {
        use model::{ImageContent, TextContent};
        let mut comp = ToolExecutionComponent::new("multi", json!({}));
        let result = ToolResult {
            content: vec![
                ToolResultContent::Text(TextContent::new("first")),
                ToolResultContent::Image(ImageContent {
                    content_type: "image".to_string(),
                    data: "AAAA".to_string(),
                    mime_type: "image/png".to_string(),
                }),
                ToolResultContent::Text(TextContent::new("second")),
            ],
            details: None,
            terminate: None,
        };
        comp.set_result(result, false);
        let joined = comp.render(60).join("\n");
        assert!(joined.contains("first"));
        assert!(joined.contains("second"));
        // Image data should not appear in the textual fallback.
        assert!(!joined.contains("AAAA"));
    }
}
