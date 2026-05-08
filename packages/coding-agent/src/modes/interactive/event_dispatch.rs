//! Bridges [`AgentSessionEvent`]s into component-update intents the driver
//! applies to the chat container.
//!
//! pi-mono's `interactive-mode.ts` handles ~20 distinct event variants with
//! complex state (streaming components, pending tool calls, compaction
//! loaders, ...). This skeleton ports the happy path: render
//! `MessageStart`/`MessageEnd` for assistant + user messages and surface
//! errors. Tool execution, streaming deltas, compaction UI, etc. are deferred.

use crate::core::agent_session::AgentSessionEvent;
use hand_agent::types::AgentEvent;
use model::Message;

/// Driver-side instruction emitted from the event dispatcher.
///
/// The driver (single-threaded over the Tui) maps these to concrete
/// `Container` mutations so this layer stays synchronous and mock-friendly.
#[derive(Debug, Clone)]
pub enum ChatUpdate {
    /// Append a user message renderer.
    AppendUser { text: String },
    /// Append an assistant message renderer with the latest snapshot.
    ///
    /// On `MessageStart` the snapshot may be empty; on `MessageEnd` it carries
    /// the final content. The driver replaces the trailing assistant component
    /// in place so the rendered result mirrors the streaming behaviour
    /// upstream.
    SetOrUpdateAssistant {
        message: Box<model::AssistantMessage>,
    },
    /// Append a tool-result line (compact form for the skeleton).
    AppendToolResult { text: String },
    /// Append a transient status line (used for compaction notices, errors,
    /// `/help` output, ...).
    AppendStatus { text: String },
}

/// Translate a single [`AgentSessionEvent`] into zero or more
/// [`ChatUpdate`]s. Returning a `Vec` keeps the API symmetric for variants
/// that need to emit multiple updates (e.g. `MessageEnd` of a tool result).
pub fn dispatch(event: &AgentSessionEvent) -> Vec<ChatUpdate> {
    match event {
        AgentSessionEvent::Agent(boxed) => dispatch_agent_event(boxed.as_ref()),
        AgentSessionEvent::CompactionStart => vec![ChatUpdate::AppendStatus {
            text: "[Compacting context...]".to_string(),
        }],
        AgentSessionEvent::CompactionEnd { .. } => vec![ChatUpdate::AppendStatus {
            text: "[Compaction complete]".to_string(),
        }],
        AgentSessionEvent::Error(err) => vec![ChatUpdate::AppendStatus {
            text: format!("Error: {err}"),
        }],
    }
}

/// Pull a `ChatUpdate` chain from a raw [`AgentEvent`].
///
/// We key off `MessageStart` / `MessageEnd` for the skeleton — streaming
/// `MessageUpdate` deltas are rendered at end-of-message, so the user sees
/// the final assistant response when the agent loop returns. Streaming with
/// per-delta repaints is a follow-up batch.
pub fn dispatch_agent_event(event: &AgentEvent) -> Vec<ChatUpdate> {
    match event {
        AgentEvent::MessageStart { message } => match message {
            Message::User(u) => vec![ChatUpdate::AppendUser {
                text: user_text_of(u),
            }],
            Message::Assistant(a) => vec![ChatUpdate::SetOrUpdateAssistant {
                message: Box::new(a.clone()),
            }],
            Message::ToolResult(_) => vec![],
        },
        AgentEvent::MessageEnd { message } => match message {
            Message::Assistant(a) => vec![ChatUpdate::SetOrUpdateAssistant {
                message: Box::new(a.clone()),
            }],
            Message::ToolResult(t) => {
                let text = tool_result_summary(t);
                vec![ChatUpdate::AppendToolResult { text }]
            }
            Message::User(_) => vec![],
        },
        // TODO(parity): handle MessageUpdate, ToolExecutionStart/Update/End,
        // TurnStart/TurnEnd to drive streaming/tool overlays.
        _ => vec![],
    }
}

fn user_text_of(message: &model::UserMessage) -> String {
    match &message.content {
        model::UserContent::Text(s) => s.clone(),
        model::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                model::UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn tool_result_summary(message: &model::ToolResultMessage) -> String {
    let body: String = message
        .content
        .iter()
        .filter_map(|c| match c {
            model::ToolResultContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prefix = if message.is_error { "[error] " } else { "" };
    format!("[{}] {}{}", message.tool_name, prefix, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
        UserMessage,
    };

    fn make_assistant(text: &str) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    #[test]
    fn user_message_start_emits_append_user() {
        let user = UserMessage::new_text("hello");
        let event = AgentEvent::MessageStart {
            message: Message::User(user),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        match &updates[0] {
            ChatUpdate::AppendUser { text } => assert_eq!(text, "hello"),
            other => panic!("expected AppendUser, got {:?}", other),
        }
    }

    #[test]
    fn assistant_message_end_emits_assistant_update() {
        let event = AgentEvent::MessageEnd {
            message: Message::Assistant(make_assistant("hi back")),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        assert!(matches!(
            &updates[0],
            ChatUpdate::SetOrUpdateAssistant { .. }
        ));
    }

    #[test]
    fn compaction_event_emits_status() {
        let updates = dispatch(&AgentSessionEvent::CompactionStart);
        assert!(matches!(&updates[0], ChatUpdate::AppendStatus { .. }));
    }

    #[test]
    fn error_event_emits_status_with_message() {
        let updates = dispatch(&AgentSessionEvent::Error("boom".to_string()));
        match &updates[0] {
            ChatUpdate::AppendStatus { text } => assert!(text.contains("boom")),
            other => panic!("expected AppendStatus, got {:?}", other),
        }
    }
}
