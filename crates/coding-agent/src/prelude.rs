//! Curated public surface for `hand_coding_agent` consumers.
//!
//! `use hand_coding_agent::prelude::*;` brings in the high-level types
//! a typical SDK consumer needs. Items not included here are still
//! accessible via their full module path
//! (e.g., `hand_coding_agent::Extension` for implementing a Tier 1 extension).
//!
//! # Stability
//!
//! The prelude is the **stable** entry point for downstream code.
//! Items are added here only after their API has settled. Phases 3-6
//! will introduce new items; existing items will not change shape
//! without a deprecation cycle.
//!
//! # Example
//!
//! ```no_run
//! use hand_coding_agent::prelude::*;
//!
//! # fn _example(model: model::Model) {
//! let session = AgentSession::in_memory(model, vec![]);
//! assert_eq!(session.message_count(), 0);
//! # }
//! ```
pub use crate::cli::Args;
pub use crate::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
pub use crate::core::error::CodingAgentError;
pub use crate::core::model_resolver::ResolvedModel;
pub use crate::core::session_manager::{SessionBackend, SessionManager};
pub use crate::core::settings::SettingsManager;
pub use crate::core::system_prompt::build_system_prompt;
