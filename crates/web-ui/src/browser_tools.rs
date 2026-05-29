//! Browser-tool execution bridge.
//!
//! Some tools the agent offers the LLM do not execute on the server: their
//! real implementation lives in the browser (the artifacts panel, future REPL,
//! etc.). The server still *declares* these tools so they appear in the LLM
//! context, but their `execute` closure does not do the work itself — it
//! suspends, waiting for the browser to run the tool and report a result.
//!
//! The mechanism:
//!
//! 1. The agent loop calls the browser tool's `execute` closure with a
//!    `tool_call_id`. The closure registers a one-shot channel keyed by that
//!    id in the per-connection [`BrowserToolHub`] and awaits the receiver.
//! 2. The normal `tool_execution_start` agent event (already forwarded over the
//!    WebSocket) tells the browser to run the tool locally.
//! 3. The browser sends back a `tool_result` frame. The WebSocket inbound task
//!    (see [`crate::ws`]) intercepts it and calls [`BrowserToolHub::resolve`],
//!    which completes the one-shot channel and unblocks the awaiting closure.
//!
//! The inbound task and the dispatcher task run concurrently, so a suspended
//! browser-tool execution can be resolved while the dispatcher is mid-prompt —
//! there is no deadlock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hand_agent::types::{AgentTool, ToolResult};
use tokio::sync::oneshot;

/// Per-connection registry of in-flight browser-tool executions.
///
/// Cloneable: every clone shares the same inner map via `Arc`, so the agent
/// loop's tool closures and the WebSocket inbound task observe the same set of
/// pending executions.
#[derive(Clone, Default)]
pub struct BrowserToolHub {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ToolResult>>>>,
}

impl BrowserToolHub {
    /// Create an empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending execution and return the receiver the caller awaits.
    ///
    /// If a sender already exists for `tool_call_id` it is replaced; the old
    /// receiver then resolves with a channel-closed error, which the tool
    /// closure maps to an error `ToolResult`. Tool-call ids are unique per run,
    /// so this is a defensive measure rather than an expected path.
    pub fn register(&self, tool_call_id: String) -> oneshot::Receiver<ToolResult> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("BrowserToolHub mutex poisoned")
            .insert(tool_call_id, tx);
        rx
    }

    /// Deliver a result to the awaiting execution, if any.
    ///
    /// Removing and sending is a no-op when no execution is registered for
    /// `tool_call_id` (e.g. a duplicate or late `tool_result` frame).
    pub fn resolve(&self, tool_call_id: &str, result: ToolResult) {
        let sender = self
            .pending
            .lock()
            .expect("BrowserToolHub mutex poisoned")
            .remove(tool_call_id);
        if let Some(tx) = sender {
            // The receiver is dropped only if the awaiting closure was
            // cancelled; ignore the send error in that case.
            let _ = tx.send(result);
        }
    }
}

/// Build an [`AgentTool`] whose execution is delegated to the browser.
///
/// The returned tool is declared to the LLM with `name`, `description`, and
/// `parameters_json`, but its `execute` closure does no local work: it
/// registers a pending execution on `hub` and awaits the browser's result.
pub fn browser_tool(
    name: impl Into<String>,
    description: impl Into<String>,
    parameters_json: serde_json::Value,
    hub: BrowserToolHub,
) -> AgentTool {
    let name = name.into();
    AgentTool::simple(
        name.clone(),
        description,
        parameters_json,
        name,
        move |tool_call_id: String, _args: serde_json::Value| {
            let hub = hub.clone();
            async move {
                let rx = hub.register(tool_call_id);
                match rx.await {
                    Ok(result) => result,
                    Err(_) => ToolResult::error("browser tool channel closed"),
                }
            }
        },
    )
}

// ---------------------------------------------------------------------------
// Artifacts browser tool: server-side declaration.
// ---------------------------------------------------------------------------

/// Brand-neutral name of the artifacts tool. Must match the client tool name
/// (`artifacts`) registered as the browser executor.
pub const ARTIFACTS_TOOL_NAME: &str = "artifacts";

/// Description shown to the LLM for the artifacts tool. The full runtime-helper
/// detail lives client-side; the server only needs enough for the model to call
/// the tool correctly. Kept brand-neutral.
pub const ARTIFACTS_TOOL_DESCRIPTION: &str = "\
Create and manage persistent files (artifacts) that live alongside the \
conversation and are rendered in the browser.

Use this tool when YOU are the author of a file: research summaries, analysis, \
documentation, markdown notes, or self-contained HTML applications and \
visualizations.

Commands (the `command` field):
- create: create a new file. Requires `filename` and `content`.
- update: targeted edit; replace `old_str` with `new_str` in an existing file. \
PREFERRED for small changes. Requires `filename`, `old_str`, `new_str`.
- rewrite: replace the entire file content. LAST RESORT. Requires `filename` \
and `content`.
- get: retrieve the current content of a file. Requires `filename`.
- delete: delete a file. Requires `filename`.
- logs: return console logs from an HTML artifact. Requires `filename`.

Supported text file types: .md, .txt, .html, .js, .css, .json, .csv, .svg. \
Prefer `update` over `rewrite` for token efficiency.";

/// JSON Schema for the artifacts tool parameters. Mirrors the client tool's
/// schema (`crates/web-ui/web/src/artifacts/artifacts-panel.ts`).
pub fn artifacts_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["create", "update", "rewrite", "get", "delete", "logs"],
                "description": "The operation to perform"
            },
            "filename": {
                "type": "string",
                "description": "Filename including extension (e.g., 'index.html', 'script.js')"
            },
            "content": { "type": "string", "description": "File content" },
            "old_str": { "type": "string", "description": "String to replace (for update command)" },
            "new_str": { "type": "string", "description": "Replacement string (for update command)" }
        },
        "required": ["command", "filename"]
    })
}

/// Build the artifacts browser tool bound to `hub`.
pub fn artifacts_browser_tool(hub: BrowserToolHub) -> AgentTool {
    browser_tool(
        ARTIFACTS_TOOL_NAME,
        ARTIFACTS_TOOL_DESCRIPTION,
        artifacts_parameters(),
        hub,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{TextContent, ToolResultContent};

    fn result_text(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ToolResultContent::Text(t) => Some(t.text.clone()),
                ToolResultContent::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[tokio::test]
    async fn register_then_resolve_delivers_result() {
        let hub = BrowserToolHub::new();
        let rx = hub.register("call-1".to_string());

        let expected = ToolResult {
            content: vec![ToolResultContent::Text(TextContent::new("created file.md"))],
            details: None,
            terminate: None,
        };
        hub.resolve("call-1", expected);

        let got = rx.await.expect("sender should have delivered a result");
        assert_eq!(result_text(&got), "created file.md");
    }

    #[tokio::test]
    async fn resolve_unknown_id_is_noop() {
        let hub = BrowserToolHub::new();
        // No registration for this id; resolve must not panic and must drop the
        // result silently.
        hub.resolve("missing", ToolResult::text("ignored"));

        // A subsequent register/resolve on a real id still works, proving the
        // hub is left in a sane state.
        let rx = hub.register("call-2".to_string());
        hub.resolve("call-2", ToolResult::text("ok"));
        let got = rx.await.expect("sender should have delivered a result");
        assert_eq!(result_text(&got), "ok");
    }

    #[tokio::test]
    async fn browser_tool_awaits_until_resolved() {
        let hub = BrowserToolHub::new();
        let tool = artifacts_browser_tool(hub.clone());
        assert_eq!(tool.name, ARTIFACTS_TOOL_NAME);

        // Drive the tool's execute closure and resolve it concurrently.
        let ctx = hand_agent::types::ToolExecuteCtx {
            tool_call_id: "call-3".to_string(),
            args: serde_json::json!({ "command": "get", "filename": "a.md" }),
            cancel: hand_agent::CancellationToken::new(),
            on_update: std::sync::Arc::new(|_| {}),
        };
        let fut = (tool.execute)(ctx);

        let resolver = {
            let hub = hub.clone();
            tokio::spawn(async move {
                // Spin until the closure has registered, then resolve.
                loop {
                    let registered = hub.pending.lock().expect("mutex").contains_key("call-3");
                    if registered {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                hub.resolve("call-3", ToolResult::text("file contents"));
            })
        };

        let result = fut.await.expect("execute returns Ok");
        resolver.await.expect("resolver task");
        assert_eq!(result_text(&result), "file contents");
    }
}
