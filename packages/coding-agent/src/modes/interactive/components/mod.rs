//! Phase-1 interactive-mode components.
//!
//! Each submodule ports a single self-contained renderer from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/`. See the
//! parent module's docs for the theming caveat.

pub mod assistant_message;
pub mod bordered_loader;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod countdown_timer;
pub mod custom_message;
pub mod diff;
pub mod dynamic_border;
pub mod keybinding_hints;
pub mod user_message;
pub mod visual_truncate;

pub use assistant_message::{AssistantMessageComponent, DEFAULT_HIDDEN_THINKING_LABEL};
pub use bordered_loader::BorderedLoaderComponent;
pub use branch_summary_message::{BranchSummaryData, BranchSummaryMessageComponent};
pub use compaction_summary_message::{CompactionSummaryData, CompactionSummaryMessageComponent};
pub use countdown_timer::{CountdownTimer, DEFAULT_TICK_INTERVAL};
pub use custom_message::{CustomMessageComponent, CustomMessageData};
pub use diff::render_diff;
pub use dynamic_border::DynamicBorderComponent;
pub use keybinding_hints::{format_keys, key_hint_for, key_text, raw_key_hint};
pub use user_message::UserMessageComponent;
pub use visual_truncate::{VisualTruncateResult, truncate_to_visual_lines};
