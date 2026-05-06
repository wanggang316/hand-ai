//! Provider implementations for different AI APIs.

pub mod anthropic_messages;
pub mod azure_openai_responses;
pub mod bedrock;
#[cfg(any(test, feature = "faux"))]
pub mod faux;
pub mod google_generative_ai;
pub mod mistral;
pub mod openai_completions;
pub mod openai_responses;
pub(crate) mod openai_responses_shared;

pub use anthropic_messages::AnthropicMessagesProvider;
pub use azure_openai_responses::{AzureOpenAIResponsesOptions, AzureOpenAIResponsesProvider};
pub use bedrock::BedrockProvider;
#[cfg(any(test, feature = "faux"))]
pub use faux::{FauxProvider, FauxScriptStep, faux_model};
pub use google_generative_ai::GoogleGenerativeAiProvider;
pub use mistral::{MistralOptions, MistralProvider};
pub use openai_completions::{
    OpenAICompletionsOptions, OpenAICompletionsProvider, ResolvedCompat, convert_messages,
    normalize_mistral_tool_id, stream_openai_completions,
};
pub use openai_responses::OpenAIResponsesProvider;
