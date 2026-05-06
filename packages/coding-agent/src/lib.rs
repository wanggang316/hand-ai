//! Hand Coding Agent — interactive coding agent with tools and session management.
//!
//! This crate provides the main coding agent application:
//! - 7 built-in tools (Read, Write, Edit, Bash, Grep, Find, Ls)
//! - Session management with JSONL persistence
//! - Settings management (global + project)
//! - System prompt generation
//! - CLI entry point
//! - Multiple run modes (interactive, print)
//!
//! Downstream consumers should prefer `use hand_coding_agent::prelude::*;`
//! for the curated, stable surface. The crate-root re-exports below mirror
//! the pre-prelude layout and remain available for existing call sites.

pub mod cli;
pub mod core;
pub mod modes;
pub mod prelude;
pub mod rpc;
pub mod tools;

// Convenience re-exports at crate root for non-prelude consumers.
// These mirror what was already exported pre-T0.2 to avoid breaking
// `hand_coding_agent::AgentSession` style imports.
pub use core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
pub use core::error::CodingAgentError;
pub use core::export::{export_to_html, export_to_jsonl};
pub use core::model_resolver::{self, ResolvedModel};
pub use core::session_manager::SessionManager;
pub use core::settings::SettingsManager;
pub use core::system_prompt::build_system_prompt;

// Extension system — kept here at crate root for now; will move to
// hand_coding_agent::extensions when Phase 3 lands the new runtime.
pub use core::extensions::{
    ExtensionConfig, ExtensionError, ExtensionHookType, ExtensionManifest, ExtensionRunner,
};

// Slash commands and keybindings — same caveat as extensions; will be
// reshaped in Phases 4/5.
pub use core::keybindings::KeyBindingsConfig;
pub use core::slash_commands::SlashCommandRegistry;
