//! Phase-1 interactive-mode components.
//!
//! Each submodule ports a single self-contained renderer from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/`. See the
//! parent module's docs for the theming caveat.

pub mod assistant_message;
pub mod user_message;

pub use assistant_message::{AssistantMessageComponent, DEFAULT_HIDDEN_THINKING_LABEL};
pub use user_message::UserMessageComponent;
