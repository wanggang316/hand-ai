//! Assistant message renderer.
//!
//! Walks an [`AssistantMessage`]'s content blocks and renders each one:
//!
//! * `Text` blocks → markdown body.
//! * `Thinking` blocks → italicised dim markdown, or a static label when
//!   `hide_thinking_block` is set.
//! * `ToolCall` blocks → no inline rendering here; the driver renders tool
//!   executions through a separate component (queued for a later batch).
//!
//! When the message stops with [`StopReason::Aborted`] or
//! [`StopReason::Error`] *and* contains no tool calls, an error footer is
//! appended (tool-call frames carry their own error UI).
//!
//! OSC 133 zone markers wrap the rendered output unless tool calls are
//! present — when they are, the tool-execution component owns the closing
//! zone marker for its own block.
//!
//! Theming caveat: until the coding-agent theme system is ported (see
//! parent module docs) the implementation hardcodes ANSI defaults that
//! match the dark-theme spirit (`thinkingText`, `error`, etc.).

use hand_tui::components::markdown::DefaultTextStyle;
use hand_tui::{Color, Component, Container, MarkdownComponent, NamedColor, TextComponent};
use model::{AssistantContentBlock, AssistantMessage, StopReason};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Default static label shown in place of thinking content when collapsed.
pub const DEFAULT_HIDDEN_THINKING_LABEL: &str = "Thinking...";

/// Component that renders an [`AssistantMessage`].
pub struct AssistantMessageComponent {
    /// When true, thinking blocks render as a static collapsed label rather
    /// than their full body. Local override — only used when
    /// `shared_hide_flag` is None.
    hide_thinking_block: bool,
    /// Optional shared toggle. When set, takes precedence over
    /// `hide_thinking_block` on each render, so a single `Ctrl+T` in the
    /// driver flips collapsed/expanded across every assistant message in
    /// the scrollback in one shot (M5.5).
    shared_hide_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Label shown when a thinking block is collapsed.
    hidden_thinking_label: String,
    /// Latest message handed to the renderer; recomputed on every render so
    /// callers can mutate fields (e.g. `hide_thinking_block`) and re-render
    /// without reconstructing the component.
    message: Option<AssistantMessage>,
}

impl AssistantMessageComponent {
    /// Construct an empty renderer. Use [`Self::set_message`] to populate it.
    pub fn new() -> Self {
        Self {
            hide_thinking_block: false,
            shared_hide_flag: None,
            hidden_thinking_label: DEFAULT_HIDDEN_THINKING_LABEL.to_string(),
            message: None,
        }
    }

    /// Construct a renderer pre-populated with `message`.
    pub fn with_message(message: AssistantMessage) -> Self {
        Self {
            hide_thinking_block: false,
            shared_hide_flag: None,
            hidden_thinking_label: DEFAULT_HIDDEN_THINKING_LABEL.to_string(),
            message: Some(message),
        }
    }

    /// Subscribe to a shared collapse toggle. While set, the local
    /// `hide_thinking_block` field is ignored — each render reads the
    /// atomic so a global Ctrl+T in the driver flips every assistant
    /// message at once.
    pub fn with_shared_hide_flag(
        mut self,
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.shared_hide_flag = Some(flag);
        self
    }

    /// Resolved collapse state — checks the shared flag when set,
    /// otherwise the local field.
    fn resolved_hide_thinking(&self) -> bool {
        if let Some(flag) = &self.shared_hide_flag {
            return flag.load(std::sync::atomic::Ordering::Relaxed);
        }
        self.hide_thinking_block
    }

    /// Toggle the collapsed-thinking state.
    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.hide_thinking_block = hide;
    }

    /// Replace the static label shown when thinking is collapsed.
    pub fn set_hidden_thinking_label(&mut self, label: impl Into<String>) {
        self.hidden_thinking_label = label.into();
    }

    /// Replace the message. Subsequent renders reflect the new content.
    pub fn set_message(&mut self, message: AssistantMessage) {
        self.message = Some(message);
    }

    /// Borrow the current message, if any.
    pub fn message(&self) -> Option<&AssistantMessage> {
        self.message.as_ref()
    }

    /// Build the underlying container from the current message state. Mirrors
    /// `updateContent` in the TS source.
    fn build(&self) -> (Container, bool) {
        let mut container = Container::new();
        let Some(message) = &self.message else {
            return (container, false);
        };

        let has_visible_content = message.content.iter().any(visible_content);
        if has_visible_content {
            container.add_child(Box::new(blank_line()));
        }

        for (i, block) in message.content.iter().enumerate() {
            match block {
                AssistantContentBlock::Text(t) if !t.text.trim().is_empty() => {
                    let mut md = MarkdownComponent::new(t.text.trim().to_string());
                    md.set_theme(
                        crate::modes::interactive::syntax_highlight::default_markdown_theme(),
                    );
                    md.set_default_style(DefaultTextStyle::default());
                    container.add_child(Box::new(md));
                }
                AssistantContentBlock::Thinking(t) if !t.thinking.trim().is_empty() => {
                    let has_visible_after = message.content.iter().skip(i + 1).any(visible_content);
                    if self.resolved_hide_thinking() {
                        container.add_child(Box::new(TextComponent::new(format!(
                            "{}{}\x1b[0m",
                            italic_dim_prefix(),
                            self.hidden_thinking_label,
                        ))));
                    } else {
                        let mut md = MarkdownComponent::new(t.thinking.trim().to_string());
                        md.set_theme(
                            crate::modes::interactive::syntax_highlight::default_markdown_theme(),
                        );
                        md.set_default_style(DefaultTextStyle {
                            fg: Some(Color::Named(NamedColor::BrightBlack)),
                            bg: None,
                            italic: true,
                        });
                        container.add_child(Box::new(md));
                    }
                    if has_visible_after {
                        container.add_child(Box::new(blank_line()));
                    }
                }
                _ => {}
            }
        }

        let has_tool_calls = message
            .content
            .iter()
            .any(|c| matches!(c, AssistantContentBlock::ToolCall(_)));

        if !has_tool_calls {
            match message.stop_reason {
                StopReason::Aborted => {
                    let abort_msg = message
                        .error_message
                        .as_deref()
                        .filter(|m| *m != "Request was aborted")
                        .unwrap_or("Operation aborted");
                    container.add_child(Box::new(blank_line()));
                    container.add_child(Box::new(TextComponent::new(format!(
                        "{}{abort_msg}\x1b[0m",
                        error_prefix(),
                    ))));
                }
                StopReason::Error => {
                    let err = message.error_message.as_deref().unwrap_or("Unknown error");
                    container.add_child(Box::new(blank_line()));
                    container.add_child(Box::new(TextComponent::new(format!(
                        "{}Error: {err}\x1b[0m",
                        error_prefix(),
                    ))));
                }
                _ => {}
            }
        }

        (container, has_tool_calls)
    }
}

impl Default for AssistantMessageComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for AssistantMessageComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let (container, has_tool_calls) = self.build();
        let mut lines = container.render(width);
        if has_tool_calls || lines.is_empty() {
            return lines;
        }
        let last = lines.len() - 1;
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }
}

/// Whether a content block contributes a visible (non-tool) body to the
/// rendered output.
fn visible_content(block: &AssistantContentBlock) -> bool {
    match block {
        AssistantContentBlock::Text(t) => !t.text.trim().is_empty(),
        AssistantContentBlock::Thinking(t) => !t.thinking.trim().is_empty(),
        AssistantContentBlock::ToolCall(_) => false,
    }
}

/// Single blank line — emitted between blocks for vertical breathing room.
fn blank_line() -> TextComponent {
    TextComponent::new("")
}

/// ANSI prefix for italic + bright-black text, used for the hidden-thinking
/// placeholder.
fn italic_dim_prefix() -> &'static str {
    // \x1b[3m = italic, \x1b[90m = bright black.
    "\x1b[3m\x1b[90m"
}

/// ANSI prefix for the error footer (bright red).
fn error_prefix() -> &'static str {
    "\x1b[91m"
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::Provider;
    use model::{Api, TextContent, ThinkingContent, ToolCall, Usage};

    fn empty_message(stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: Vec::new(),
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test-model".to_string(),
            usage: Usage::default(),
            stop_reason,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    fn with_content(blocks: Vec<AssistantContentBlock>) -> AssistantMessage {
        let mut m = empty_message(StopReason::Stop);
        m.content = blocks;
        m
    }

    #[test]
    fn renders_text_block_with_zone_markers() {
        let msg = with_content(vec![AssistantContentBlock::Text(TextContent::new(
            "hello world",
        ))]);
        let comp = AssistantMessageComponent::with_message(msg);
        let lines = comp.render(40);
        assert!(!lines.is_empty());
        assert!(
            lines[0].starts_with(OSC133_ZONE_START),
            "expected zone start marker, got {:?}",
            lines[0]
        );
        let last = lines.last().unwrap();
        assert!(last.contains(OSC133_ZONE_END));
        assert!(last.contains(OSC133_ZONE_FINAL));
        let joined = lines.join("\n");
        assert!(joined.contains("hello world"), "missing body: {joined:?}");
    }

    #[test]
    fn renders_thinking_block_with_dim_italic() {
        let msg = with_content(vec![AssistantContentBlock::Thinking(ThinkingContent::new(
            "pondering",
        ))]);
        let comp = AssistantMessageComponent::with_message(msg);
        let lines = comp.render(40);
        let joined = lines.join("\n");
        assert!(joined.contains("pondering"), "missing thinking text");
        // Italic SGR (\x1b[3m) should appear from default-style prefix.
        assert!(
            joined.contains("\x1b[3m"),
            "expected italic SGR in thinking output: {joined:?}"
        );
    }

    #[test]
    fn collapsed_thinking_uses_static_label() {
        let msg = with_content(vec![AssistantContentBlock::Thinking(ThinkingContent::new(
            "secret reasoning",
        ))]);
        let mut comp = AssistantMessageComponent::with_message(msg);
        comp.set_hide_thinking_block(true);
        comp.set_hidden_thinking_label("Reasoning...");
        let joined = comp.render(40).join("\n");
        assert!(
            joined.contains("Reasoning..."),
            "expected hidden label: {joined:?}"
        );
        assert!(
            !joined.contains("secret reasoning"),
            "must not leak thinking body when collapsed: {joined:?}"
        );
    }

    #[test]
    fn skips_zone_markers_when_message_has_tool_calls() {
        let msg = with_content(vec![
            AssistantContentBlock::Text(TextContent::new("running tool")),
            AssistantContentBlock::ToolCall(ToolCall::new(
                "id-1",
                "Read",
                serde_json::json!({"path": "/etc/hostname"}),
            )),
        ]);
        let comp = AssistantMessageComponent::with_message(msg);
        let lines = comp.render(40);
        assert!(!lines.is_empty());
        // First line must NOT carry the zone start when tool calls are present.
        assert!(
            !lines[0].starts_with(OSC133_ZONE_START),
            "tool-call message should not own the zone marker: {:?}",
            lines[0]
        );
    }

    #[test]
    fn appends_aborted_footer_without_tool_calls() {
        let mut msg = with_content(vec![AssistantContentBlock::Text(TextContent::new(
            "partial",
        ))]);
        msg.stop_reason = StopReason::Aborted;
        msg.error_message = Some("custom abort reason".to_string());
        let comp = AssistantMessageComponent::with_message(msg);
        let joined = comp.render(40).join("\n");
        assert!(
            joined.contains("custom abort reason"),
            "expected abort message in footer: {joined:?}"
        );
        // Default fallback path: stock "Operation aborted" when no errorMessage.
        let mut msg2 = empty_message(StopReason::Aborted);
        msg2.content = vec![AssistantContentBlock::Text(TextContent::new("partial"))];
        let comp2 = AssistantMessageComponent::with_message(msg2);
        let joined2 = comp2.render(40).join("\n");
        assert!(
            joined2.contains("Operation aborted"),
            "expected default abort label: {joined2:?}"
        );
    }

    #[test]
    fn appends_error_footer_without_tool_calls() {
        let mut msg = empty_message(StopReason::Error);
        msg.error_message = Some("rate limit".to_string());
        let comp = AssistantMessageComponent::with_message(msg);
        let joined = comp.render(40).join("\n");
        assert!(joined.contains("Error: rate limit"));
    }

    #[test]
    fn empty_message_yields_no_output() {
        let comp = AssistantMessageComponent::new();
        let lines = comp.render(40);
        assert!(lines.is_empty());
    }
}
