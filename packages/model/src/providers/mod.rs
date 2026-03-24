//! Provider implementations for different AI APIs.

pub mod anthropic_messages;
pub mod google_generative_ai;
pub mod openai_completions;

pub use anthropic_messages::AnthropicMessagesProvider;
pub use google_generative_ai::GoogleGenerativeAiProvider;
pub use openai_completions::{
    OpenAICompletionsOptions, OpenAICompletionsProvider, ResolvedCompat, convert_messages,
    normalize_mistral_tool_id, stream_openai_completions,
};
