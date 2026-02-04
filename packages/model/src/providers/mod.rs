//! Provider implementations for different AI APIs.

pub mod openai_completions;

pub use openai_completions::{
    OpenAICompletionsOptions, OpenAICompletionsProvider, ResolvedCompat, convert_messages,
    normalize_mistral_tool_id, stream_openai_completions,
};
