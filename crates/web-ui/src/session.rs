//! Per-connection agent session construction.
//!
//! Each WebSocket connection owns exactly one ephemeral, in-memory
//! [`AgentSession`]. The session is built the same way the CLI builds its
//! `--no-session` RPC session: resolve the model, create the built-in tools,
//! and hand the session a fresh [`model::Client`] with all providers
//! registered. API keys are resolved server-side from the process
//! environment and never travel to the browser.
//!
//! In addition to the server-executed built-in tools, the session declares
//! browser-executed tools (e.g. `artifacts`). These are offered to the LLM and
//! routed through a [`BrowserToolHub`]: their `execute` closures suspend until
//! the browser reports a result. The hub is returned alongside the session so
//! the WebSocket layer can resolve those executions from inbound `tool_result`
//! frames.

use crate::app::AppState;
use crate::browser_tools::{
    BrowserToolHub, artifacts_browser_tool, extract_document_browser_tool,
    javascript_repl_browser_tool,
};
use hand_coding_agent::{AgentSession, model_resolver, tools};

/// Build a fresh in-memory agent session for a new connection, paired with the
/// [`BrowserToolHub`] that routes its browser-executed tool calls.
pub fn build_session(state: &AppState) -> (AgentSession, BrowserToolHub) {
    let resolved = model_resolver::resolve_model(state.provider.as_deref(), &state.model);

    let hub = BrowserToolHub::new();
    let mut agent_tools = tools::create_default_tools(&state.cwd);
    agent_tools.push(artifacts_browser_tool(hub.clone()));
    agent_tools.push(javascript_repl_browser_tool(hub.clone()));
    agent_tools.push(extract_document_browser_tool(hub.clone()));

    let session =
        AgentSession::in_memory_with_client(resolved.model, agent_tools, model::Client::new());
    (session, hub)
}
