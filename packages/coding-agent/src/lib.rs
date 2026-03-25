//! Hand Coding Agent — interactive coding agent with tools and session management.
//!
//! This crate provides the main coding agent application:
//! - 7 built-in tools (Read, Write, Edit, Bash, Grep, Find, Ls)
//! - Session management with JSONL persistence
//! - Settings management (global + project)
//! - System prompt generation
//! - CLI entry point
//! - Multiple run modes (interactive, print)

pub mod core;
pub mod tools;

// Re-export commonly used items
pub use core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
pub use core::error::CodingAgentError;
pub use core::export::{export_to_html, export_to_jsonl};
pub use core::model_resolver::{self, ResolvedModel};
pub use core::session_manager::SessionManager;
pub use core::settings::SettingsManager;
pub use core::system_prompt::build_system_prompt;
