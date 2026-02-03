//! Provider implementations for different AI APIs.

pub mod openai_completions;

pub use openai_completions::{
    stream_openai_completions,
    OpenAICompletionsOptions, normalize_mistral_tool_id, convert_messages,
    ResolvedCompat, OpenAICompletionsProvider,
};
