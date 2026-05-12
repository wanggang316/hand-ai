//! Render the user-message and tool-execution components offline and dump
//! each line with its escape sequences visible. Used to diagnose the
//! "background not solid" complaint — every line painted with the bubble's
//! background SGR should appear with that SGR opening, padded content, and
//! a trailing reset, end-to-end.
//!
//! Run:
//!     cargo run --example dump_bubbles -p hand-coding-agent

use hand_coding_agent::modes::interactive::components::tool_execution::ToolExecutionComponent;
use hand_coding_agent::modes::interactive::components::user_message::UserMessageComponent;
use hand_tui::Component;
use serde_json::json;

fn dump(label: &str, lines: &[String]) {
    println!("==== {label} ({} lines) ====", lines.len());
    for (i, line) in lines.iter().enumerate() {
        // Visible — what the terminal renders.
        println!("  L{i:02} VIS: {line}\x1b[0m");
        // Debug — the raw bytes, with ESC printed as `\e`.
        let dbg = line.replace('\x1b', "\\e");
        println!("  L{i:02} DBG: {dbg}");
    }
}

fn main() {
    let width = 80u16;

    let bubble = UserMessageComponent::new("你好");
    dump("user 你好", &bubble.render(width));

    let h = UserMessageComponent::new("hi");
    dump("user hi @ width 80", &h.render(80));
    dump("user hi @ width 30", &h.render(30));

    let mut tool = ToolExecutionComponent::new("ls", json!(""));
    tool.set_result(
        hand_agent::types::ToolResult::text("Invalid arguments for tool 'ls': \"\" is not of type \"object\" (path: )"),
        true,
    );
    dump("tool ls (error)", &tool.render(width));
}
