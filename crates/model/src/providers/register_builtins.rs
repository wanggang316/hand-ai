//! Registration helpers for the built-in API providers.
//!
//! Every provider is linked into the crate, so we just construct one of each
//! and insert it against its canonical `Api` key.
//!
//! Calling `register_builtins` twice is safe — later registrations overwrite
//! earlier ones, matching `ApiProviderRegistry::register`'s last-wins
//! semantics.

use crate::api_registry::{ApiProviderRegistry, BoxedApiProvider};
use crate::providers::anthropic_messages::AnthropicMessagesProvider;
use crate::providers::azure_openai_responses::AzureOpenAIResponsesProvider;
use crate::providers::bedrock::BedrockProvider;
use crate::providers::google_generative_ai::GoogleGenerativeAiProvider;
use crate::providers::google_vertex::GoogleVertexProvider;
use crate::providers::mistral::MistralProvider;
use crate::providers::openai_codex_responses::OpenAICodexResponsesProvider;
use crate::providers::openai_completions::OpenAICompletionsProvider;
use crate::providers::openai_responses::OpenAIResponsesProvider;
use crate::types::Api;

pub(crate) const BUILTIN_SOURCE_ID: &str = "builtin";

/// Build a fresh instance of the built-in provider for `api`, if one
/// exists. Returns `None` for non-builtin or test-only `Api` variants
/// so `ClientBuilder::with_builtin` can surface that cleanly. The
/// match is the single source of truth shared with
/// [`register_builtins`] and `BUILTIN_APIS`.
pub(crate) fn make_builtin_provider(api: Api) -> Option<BoxedApiProvider> {
    Some(match api {
        Api::OpenAICompletions => Box::new(OpenAICompletionsProvider::new()),
        Api::OpenAIResponses => Box::new(OpenAIResponsesProvider::new()),
        Api::OpenAICodexResponses => Box::new(OpenAICodexResponsesProvider::new()),
        Api::AzureOpenAiResponses => Box::new(AzureOpenAIResponsesProvider::new()),
        Api::AnthropicMessages => Box::new(AnthropicMessagesProvider::new()),
        Api::BedrockConverseStream => Box::new(BedrockProvider::new()),
        // The Gemini CLI variant rides the same Generative AI wire format;
        // reuse the provider so callers that target either resolve cleanly.
        Api::GoogleGenerativeAi | Api::GoogleGeminiCli => {
            Box::new(GoogleGenerativeAiProvider::new())
        }
        Api::GoogleVertex => Box::new(GoogleVertexProvider::new()),
        Api::MistralConversations => Box::new(MistralProvider::new()),
        _ => return None,
    })
}

/// Every `Api` variant that has a corresponding built-in provider.
/// Used by `register_builtins` and `ClientBuilder::with_all_builtins`
/// so adding a new built-in is a one-line edit in `make_builtin_provider`
/// plus an entry here.
pub(crate) const BUILTIN_APIS: &[Api] = &[
    Api::OpenAICompletions,
    Api::OpenAIResponses,
    Api::OpenAICodexResponses,
    Api::AzureOpenAiResponses,
    Api::AnthropicMessages,
    Api::BedrockConverseStream,
    Api::GoogleGenerativeAi,
    Api::GoogleGeminiCli,
    Api::GoogleVertex,
    Api::MistralConversations,
];

/// Register every built-in provider against its canonical `Api` key.
///
/// Idempotent: re-invoking simply overwrites the previous registration with a
/// fresh provider instance, which is fine because providers are stateless apart
/// from their internal `reqwest::Client`.
pub fn register_builtins(registry: &ApiProviderRegistry) {
    for &api in BUILTIN_APIS {
        if let Some(provider) = make_builtin_provider(api) {
            registry.register(api, provider, Some(BUILTIN_SOURCE_ID.to_string()));
        }
    }
}

/// Variant that also installs the in-memory faux provider used by tests and
/// the parity harness. Gated behind the `faux` feature (or `cfg(test)`) so it
/// never ships in a production build.
#[cfg(any(test, feature = "faux"))]
pub fn register_builtins_with_faux(registry: &ApiProviderRegistry) {
    use crate::providers::faux::FauxProvider;
    use crate::types::Provider;

    register_builtins(registry);
    registry.register(
        Api::Faux,
        Box::new(FauxProvider::new(Api::Faux, Provider::OpenAI, vec![])),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_all_canonical_apis() {
        let registry = ApiProviderRegistry::new();
        register_builtins(&registry);
        for api in [
            Api::OpenAICompletions,
            Api::OpenAIResponses,
            Api::OpenAICodexResponses,
            Api::AzureOpenAiResponses,
            Api::AnthropicMessages,
            Api::BedrockConverseStream,
            Api::GoogleGenerativeAi,
            Api::GoogleGeminiCli,
            Api::GoogleVertex,
            Api::MistralConversations,
        ] {
            assert!(registry.has(&api), "missing provider for {api:?}");
        }
    }

    #[test]
    fn idempotent_when_called_twice() {
        let registry = ApiProviderRegistry::new();
        register_builtins(&registry);
        register_builtins(&registry);
        assert!(registry.has(&Api::OpenAICompletions));
        assert!(registry.has(&Api::AnthropicMessages));
    }

    #[test]
    fn faux_variant_registers_faux_too() {
        let registry = ApiProviderRegistry::new();
        register_builtins_with_faux(&registry);
        assert!(registry.has(&Api::Faux));
        assert!(registry.has(&Api::OpenAICompletions));
    }
}
