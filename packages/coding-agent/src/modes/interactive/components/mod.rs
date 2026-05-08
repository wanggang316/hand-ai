//! Phase-1 interactive-mode components.
//!
//! Each submodule ports a single self-contained renderer from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/`. See the
//! parent module's docs for the theming caveat.

pub mod assistant_message;
pub mod bash_execution;
pub mod bordered_loader;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod countdown_timer;
pub mod custom_message;
pub mod diff;
pub mod dynamic_border;
pub mod earendil_announcement;
pub mod extension_input;
pub mod extension_selector;
pub mod footer;
pub mod keybinding_hints;
pub mod model_selector;
pub mod oauth_selector;
pub mod scoped_models_selector;
pub mod session_selector_search;
pub mod show_images_selector;
pub mod skill_invocation_message;
pub mod theme_selector;
pub mod thinking_selector;
pub mod tool_execution;
pub mod user_message;
pub mod user_message_selector;
pub mod visual_truncate;

pub use assistant_message::{AssistantMessageComponent, DEFAULT_HIDDEN_THINKING_LABEL};
pub use bash_execution::{BashExecutionComponent, BashStatus, PREVIEW_LINES};
pub use bordered_loader::BorderedLoaderComponent;
pub use branch_summary_message::{BranchSummaryData, BranchSummaryMessageComponent};
pub use compaction_summary_message::{CompactionSummaryData, CompactionSummaryMessageComponent};
pub use countdown_timer::{CountdownTimer, DEFAULT_TICK_INTERVAL};
pub use custom_message::{CustomMessageComponent, CustomMessageData};
pub use diff::render_diff;
pub use dynamic_border::DynamicBorderComponent;
pub use earendil_announcement::{
    BLOG_URL as EARENDIL_BLOG_URL, EarendilAnnouncementComponent,
    IMAGE_FILENAME as EARENDIL_IMAGE_FILENAME, IMAGE_MAX_WIDTH_CELLS as EARENDIL_IMAGE_MAX_WIDTH,
};
pub use extension_input::{ExtensionInputComponent, ExtensionInputEvent};
pub use extension_selector::{ExtensionSelectorComponent, ExtensionSelectorEvent};
pub use footer::{FooterComponent, FooterViewModel, TokenUsageSummary};
pub use keybinding_hints::{format_keys, key_hint_for, key_text, raw_key_hint};
pub use model_selector::{ModelOutcome, ModelScope, ModelSelectorComponent};
pub use oauth_selector::{
    AuthSelectorMode, AuthSelectorProvider, OAuthOutcome, OAuthSelectorComponent,
};
pub use scoped_models_selector::{
    ScopedModelsConfig, ScopedModelsOutcome, ScopedModelsSelectorComponent,
};
pub use session_selector_search::{
    MatchResult, NameFilter, ParsedSearchQuery, SearchToken, SortMode, TokenKind,
    filter_and_sort_sessions, has_session_name, match_session, parse_search_query,
};
pub use show_images_selector::{ShowImagesOutcome, ShowImagesSelectorComponent};
pub use skill_invocation_message::{ParsedSkillBlockData, SkillInvocationMessageComponent};
pub use theme_selector::{ThemeOutcome, ThemeSelectorComponent};
pub use thinking_selector::{ThinkingOutcome, ThinkingSelectorComponent};
pub use tool_execution::{ToolExecutionComponent, ToolExecutionStatus};
pub use user_message::UserMessageComponent;
pub use user_message_selector::{
    UserMessageItem, UserMessageSelectorComponent, UserMessageSelectorEvent,
};
pub use visual_truncate::{VisualTruncateResult, truncate_to_visual_lines};
