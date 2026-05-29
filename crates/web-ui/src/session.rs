//! Per-connection agent session construction.
//!
//! Each WebSocket connection owns exactly one ephemeral, in-memory
//! [`AgentSession`]. The session is built the same way the CLI builds its
//! `--no-session` RPC session: resolve the model, create the built-in tools,
//! and hand the session a fresh [`model::Client`] with all providers
//! registered. API keys are resolved server-side from the process
//! environment and never travel to the browser.

use crate::app::AppState;
use hand_coding_agent::{AgentSession, model_resolver, tools};

/// Build a fresh in-memory agent session for a new connection.
pub fn build_session(state: &AppState) -> AgentSession {
    let resolved = model_resolver::resolve_model(state.provider.as_deref(), &state.model);
    let agent_tools = tools::create_default_tools(&state.cwd);
    AgentSession::in_memory_with_client(resolved.model, agent_tools, model::Client::new())
}
