//! Phase-1 interactive-mode components.
//!
//! Each submodule ports a single self-contained renderer from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/`. See the
//! parent module's docs for the theming caveat.

pub mod assistant_message;
pub mod compaction_summary_message;
pub mod custom_message;
pub mod user_message;

pub use assistant_message::{AssistantMessageComponent, DEFAULT_HIDDEN_THINKING_LABEL};
pub use compaction_summary_message::{
    CompactionSummaryData, CompactionSummaryMessageComponent, DEFAULT_EXPAND_HINT,
};
pub use custom_message::{CustomMessageComponent, CustomMessageData};
pub use user_message::UserMessageComponent;
