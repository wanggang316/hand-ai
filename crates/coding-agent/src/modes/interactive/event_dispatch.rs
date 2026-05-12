//! Bridges [`AgentSessionEvent`]s into component-update intents the driver
//! applies to the chat container.
//!
//! pi-mono's `interactive-mode.ts` handles ~20 distinct event variants with
//! complex state (streaming components, pending tool calls, compaction
//! loaders, ...). The Rust port covers the happy path: render
//! `MessageStart` / `MessageUpdate` / `MessageEnd` for assistant + user
//! messages, surface tool-execution lifecycle events, and forward errors.
//! Compaction-summary UI, login dialogs, etc. remain deferred.

use crate::core::agent_session::AgentSessionEvent;
use hand_agent::types::{AgentEvent, ToolResult};
use model::{Message, ToolResultContent};

/// Driver-side instruction emitted from the event dispatcher.
///
/// The driver (single-threaded over the Tui) maps these to concrete
/// `Container` mutations so this layer stays synchronous and mock-friendly.
#[derive(Debug, Clone)]
pub enum ChatUpdate {
    /// Append a user message renderer.
    AppendUser { text: String },
    /// Append a fresh assistant message renderer with the given snapshot.
    ///
    /// Emitted on `MessageStart` (snapshot may be empty) and on `MessageEnd`
    /// (final content). The driver appends a new component each time so the
    /// chat history retains a clear separator between turns.
    AppendAssistant {
        message: Box<model::AssistantMessage>,
    },
    /// Replace the trailing in-flight assistant message with the given
    /// snapshot. Emitted on streaming `MessageUpdate` deltas so the user sees
    /// partial output mirror the upstream behaviour. If the trailing component
    /// is not an assistant message (defensive case), the driver appends.
    ReplaceLastAssistant {
        message: Box<model::AssistantMessage>,
    },
    /// Begin tracking a tool execution. The driver spawns a tool component
    /// (or a [`super::components::BashExecutionComponent`] when `tool_name`
    /// is `bash`) and remembers it under `tool_call_id` for subsequent
    /// updates.
    ToolStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    /// Apply streaming output to an in-flight tool execution. The driver
    /// looks up the component by `tool_call_id` and feeds the partial result
    /// into it.
    ToolUpdate {
        tool_call_id: String,
        partial_text: String,
    },
    /// Mark a tracked tool execution as complete. The driver finalises the
    /// component (status, output, exit code).
    ToolEnd {
        tool_call_id: String,
        result_text: String,
        is_error: bool,
        exit_code: Option<i32>,
    },
    /// Append a tool-result line (compact form, fallback path used when a
    /// tool result arrives without a matching `ToolStart`).
    AppendToolResult { text: String },
    /// Append a transient status line (used for compaction notices, errors,
    /// `/help` output, ...).
    AppendStatus { text: String },
    /// The active theme changed. Carries the theme's short name so renderers
    /// that cache palette state can refresh. Emitted by `/theme <name>` and
    /// the theme-selector overlay; the driver currently logs the change as
    /// a status line — the live palette swap lands with the theme bridge
    /// (see `docs/exec-plans/parity-completion.md` §A1).
    ThemeChanged { theme: String },
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
        // Errors are handled in the driver's event pump via push_error so
        // they render as the unmissable red banner instead of a dim yellow
        // status line.
        AgentSessionEvent::Error(_) => Vec::new(),
    }
}

/// Pull a `ChatUpdate` chain from a raw [`AgentEvent`].
///
/// `MessageStart` and `MessageEnd` append fresh user/assistant components.
/// `MessageUpdate` replaces the trailing in-flight assistant snapshot so the
/// rendered output reflects streaming progress. Tool-execution events drive
/// per-call components keyed off `tool_call_id`.
pub fn dispatch_agent_event(event: &AgentEvent) -> Vec<ChatUpdate> {
    match event {
        AgentEvent::MessageStart { message } => match message {
            // User messages are pushed by the submit handler in the driver
            // BEFORE `send_message` is awaited (see `driver.rs` immediate
            // echo), and by `replay_messages_into` for history. Emitting an
            // additional `AppendUser` from this event fires AFTER the
            // immediate echo and renders a second identical bubble.
            // pi-mono uses the event-driven path *exclusively*; we keep
            // the immediate echo for input responsiveness, so we drop the
            // event-driven path instead.
            Message::User(_) => vec![],
            Message::Assistant(a) => vec![ChatUpdate::AppendAssistant {
                message: Box::new(a.clone()),
            }],
            Message::ToolResult(_) => vec![],
        },
        AgentEvent::MessageUpdate {
            message: Message::Assistant(a),
            ..
        } => vec![ChatUpdate::ReplaceLastAssistant {
            message: Box::new(a.clone()),
        }],
        // Streaming user / tool-result deltas don't currently re-render.
        AgentEvent::MessageUpdate { .. } => vec![],
        AgentEvent::MessageEnd { message } => match message {
            Message::Assistant(a) => vec![ChatUpdate::ReplaceLastAssistant {
                message: Box::new(a.clone()),
            }],
            // Tool results are rendered inline with the matching tool-call
            // bubble (see `ChatUpdate::ToolEnd`, driven from
            // `AgentEvent::ToolExecutionEnd`). Don't emit a duplicate dim
            // `[tool] [error] body` line below the bubble — pi-mono drops
            // this branch and our previous behaviour left a white-gapped
            // copy of the bubble's contents below every tool call.
            Message::ToolResult(_) => vec![],
            Message::User(_) => vec![],
        },
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => vec![ChatUpdate::ToolStart {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
        }],
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
            ..
        } => vec![ChatUpdate::ToolUpdate {
            tool_call_id: tool_call_id.clone(),
            partial_text: tool_result_text(partial_result),
        }],
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
            ..
        } => vec![ChatUpdate::ToolEnd {
            tool_call_id: tool_call_id.clone(),
            result_text: tool_result_text(result),
            is_error: *is_error,
            exit_code: bash_exit_code(result),
        }],
        // TODO(parity): TurnStart/TurnEnd hooks for compaction loaders etc.
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

/// Concatenate text-typed content blocks of a [`ToolResult`]. Image blocks
/// are dropped — the textual fallback path is what the chat-update consumer
/// renders.
fn tool_result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Best-effort exit-code lookup for `bash`-flavoured tool results. The agent
/// loop stores the spawned process's exit status under `details.exit_code`
/// when it is available.
fn bash_exit_code(result: &ToolResult) -> Option<i32> {
    result
        .details
        .as_ref()?
        .get("exit_code")?
        .as_i64()
        .map(|v| v as i32)
}


#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Provider, StopReason,
        TextContent, Usage, UserMessage,
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
    fn user_message_start_does_not_emit_append_user() {
        // Regression: `MessageStart{User}` USED to produce `AppendUser`,
        // which combined with the driver's immediate echo to render every
        // user message twice. The event-driven path is now intentionally
        // a no-op for user messages — see [`dispatch_agent_event`].
        let user = UserMessage::new_text("hello");
        let event = AgentEvent::MessageStart {
            message: Message::User(user),
        };
        let updates = dispatch_agent_event(&event);
        assert!(updates.is_empty(), "expected no updates, got {updates:?}");
    }

    #[test]
    fn assistant_message_start_emits_append_assistant() {
        let event = AgentEvent::MessageStart {
            message: Message::Assistant(make_assistant("")),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        assert!(matches!(&updates[0], ChatUpdate::AppendAssistant { .. }));
    }

    #[test]
    fn assistant_message_end_emits_replace_last_assistant() {
        let event = AgentEvent::MessageEnd {
            message: Message::Assistant(make_assistant("hi back")),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        assert!(matches!(
            &updates[0],
            ChatUpdate::ReplaceLastAssistant { .. }
        ));
    }

    #[test]
    fn assistant_message_update_emits_replace_last_with_partial() {
        let event = AgentEvent::MessageUpdate {
            message: Message::Assistant(make_assistant("partial body")),
            assistant_message_event: Box::new(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "partial body".to_string(),
                partial: make_assistant("partial body"),
            }),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        match &updates[0] {
            ChatUpdate::ReplaceLastAssistant { message } => {
                let text = match &message.content[0] {
                    AssistantContentBlock::Text(t) => t.text.clone(),
                    _ => String::new(),
                };
                assert_eq!(text, "partial body");
            }
            other => panic!("expected ReplaceLastAssistant, got {:?}", other),
        }
    }

    #[test]
    fn user_message_update_emits_no_chat_update() {
        let event = AgentEvent::MessageUpdate {
            message: Message::User(UserMessage::new_text("ignored")),
            assistant_message_event: Box::new(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: String::new(),
                partial: make_assistant(""),
            }),
        };
        let updates = dispatch_agent_event(&event);
        assert!(updates.is_empty());
    }

    #[test]
    fn tool_execution_start_emits_tool_start() {
        let event = AgentEvent::ToolExecutionStart {
            tool_call_id: "call-1".into(),
            tool_name: "read".into(),
            args: serde_json::json!({"path": "/x"}),
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        match &updates[0] {
            ChatUpdate::ToolStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                assert_eq!(tool_call_id, "call-1");
                assert_eq!(tool_name, "read");
                assert_eq!(args["path"], "/x");
            }
            other => panic!("expected ToolStart, got {:?}", other),
        }
    }

    #[test]
    fn tool_execution_update_emits_tool_update_with_text() {
        let partial = ToolResult::text("streaming...");
        let event = AgentEvent::ToolExecutionUpdate {
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!({}),
            partial_result: partial,
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        match &updates[0] {
            ChatUpdate::ToolUpdate {
                tool_call_id,
                partial_text,
            } => {
                assert_eq!(tool_call_id, "call-1");
                assert!(partial_text.contains("streaming"));
            }
            other => panic!("expected ToolUpdate, got {:?}", other),
        }
    }

    #[test]
    fn tool_execution_end_emits_tool_end_with_exit_code() {
        let mut result = ToolResult::text("done");
        result.details = Some(serde_json::json!({"exit_code": 0}));
        let event = AgentEvent::ToolExecutionEnd {
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            result,
            is_error: false,
        };
        let updates = dispatch_agent_event(&event);
        assert_eq!(updates.len(), 1);
        match &updates[0] {
            ChatUpdate::ToolEnd {
                tool_call_id,
                result_text,
                is_error,
                exit_code,
            } => {
                assert_eq!(tool_call_id, "call-1");
                assert!(result_text.contains("done"));
                assert!(!is_error);
                assert_eq!(*exit_code, Some(0));
            }
            other => panic!("expected ToolEnd, got {:?}", other),
        }
    }

    #[test]
    fn tool_execution_end_propagates_is_error() {
        let event = AgentEvent::ToolExecutionEnd {
            tool_call_id: "call-x".into(),
            tool_name: "read".into(),
            result: ToolResult::error("nope"),
            is_error: true,
        };
        let updates = dispatch_agent_event(&event);
        match &updates[0] {
            ChatUpdate::ToolEnd {
                is_error,
                result_text,
                exit_code,
                ..
            } => {
                assert!(is_error);
                assert!(result_text.contains("nope"));
                assert_eq!(*exit_code, None);
            }
            other => panic!("expected ToolEnd, got {:?}", other),
        }
    }

    #[test]
    fn compaction_event_emits_status() {
        let updates = dispatch(&AgentSessionEvent::CompactionStart);
        assert!(matches!(&updates[0], ChatUpdate::AppendStatus { .. }));
    }

    #[test]
    fn error_event_yields_no_chat_updates() {
        // Errors are routed through the driver's push_error helper (red
        // banner) instead of an AppendStatus line; dispatch() no longer
        // emits anything for the Error variant.
        let updates = dispatch(&AgentSessionEvent::Error("boom".to_string()));
        assert!(updates.is_empty(), "expected no chat updates, got {updates:?}");
    }
}
