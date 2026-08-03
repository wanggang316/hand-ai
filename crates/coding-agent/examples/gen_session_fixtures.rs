//! One-shot generator for the resume/replay TUI session fixtures.
//!
//! Run once to (re)generate `tests/fixtures/tui/sessions/*.jsonl` via the real
//! `SessionManager` writer, so the on-disk format is guaranteed valid. Not part of
//! the test suite — invoke manually: `cargo run -p hand-coding-agent --example
//! gen_session_fixtures`.

use std::path::Path;

use hand_coding_agent::SessionManager;
use model::Message;
use model::types::{
    Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent,
    ThinkingContent, ToolResultContent, ToolResultMessage, Usage, UserMessage,
};

fn assistant(text: &str, thinking: Option<&str>, stop: StopReason, err: Option<&str>) -> Message {
    let mut content = Vec::new();
    if let Some(t) = thinking {
        content.push(AssistantContentBlock::Thinking(ThinkingContent::new(t)));
    }
    content.push(AssistantContentBlock::Text(TextContent::new(text)));
    Message::Assistant(AssistantMessage {
        role: "assistant".to_string(),
        content,
        api: Api::AnthropicMessages,
        provider: Provider::Anthropic,
        model: "claude-fixture".to_string(),
        usage: Usage::default(),
        stop_reason: stop,
        error_message: err.map(str::to_string),
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    })
}

fn tool_result(tool_name: &str, body: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: "toolResult".to_string(),
        tool_call_id: "call-fixture".to_string(),
        tool_name: tool_name.to_string(),
        content: vec![ToolResultContent::Text(TextContent::new(body))],
        details: None,
        is_error: false,
        timestamp: 0,
    })
}

fn write_session(dir: &Path, name: &str, messages: Vec<Message>) {
    // Create a session in `dir`, append the messages, then copy the produced JSONL
    // to `<name>.jsonl` so the fixture file has a stable, human-readable name.
    let cwd = std::env::current_dir().unwrap();
    let mut sm = SessionManager::create_in(&cwd, dir).expect("create session");
    for m in messages {
        sm.append_message(m).expect("append message");
    }
    let produced = sm.path().to_path_buf();
    let target = dir.join(format!("{name}.jsonl"));
    std::fs::copy(&produced, &target).expect("copy fixture");
    // Remove the id-named source so only the stable fixture name remains.
    if produced != target {
        let _ = std::fs::remove_file(&produced);
    }
    println!("wrote {}", target.display());
}

fn main() {
    let dir = Path::new("crates/coding-agent/tests/fixtures/tui/sessions");
    std::fs::create_dir_all(dir).expect("create fixtures dir");

    // thinking-blocks: a resumed turn with a thinking block before the answer.
    write_session(
        dir,
        "thinking-blocks",
        vec![
            Message::User(UserMessage::new_text("what is the meaning of life?")),
            assistant(
                "The answer is 42.",
                Some("Let me reason about this carefully."),
                StopReason::Stop,
                None,
            ),
        ],
    );

    // error-ended: the last assistant message stopped with stop_reason=Error, so a
    // resume replay surfaces the present-side red error footnote (VAL-CHAT-029).
    write_session(
        dir,
        "error-ended",
        vec![
            Message::User(UserMessage::new_text("please do the thing")),
            assistant(
                "partial output before the failure",
                None,
                StopReason::Error,
                Some("rate limit exceeded"),
            ),
        ],
    );

    // multi-message-resume: several user/assistant turns plus a tool result, to
    // exercise ordered replay + the dimmed [tool_name] line (VAL-CHAT-012).
    write_session(
        dir,
        "multi-message-resume",
        vec![
            Message::User(UserMessage::new_text("list the files")),
            assistant("I'll read the directory.", None, StopReason::ToolUse, None),
            tool_result("bash", "a.txt\nb.txt\nc.txt"),
            assistant(
                "There are three files: a.txt, b.txt, c.txt.",
                None,
                StopReason::Stop,
                None,
            ),
            Message::User(UserMessage::new_text("thanks")),
            assistant("You're welcome!", None, StopReason::Stop, None),
        ],
    );
}
