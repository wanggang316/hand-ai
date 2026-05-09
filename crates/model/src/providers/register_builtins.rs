//! Registration helpers for the built-in API providers.
//!
//! Mirrors `pi-mono/packages/ai/src/providers/register-builtins.ts`. The TS
//! implementation lazy-imports each provider; here every provider is already
//! linked into the crate, so we just construct one of each and insert it
//! against its canonical `Api` key.
//!
//! Calling `register_builtins` twice is safe — later registrations overwrite
//! earlier ones, matching `ApiProviderRegistry::register`'s last-wins
//! semantics.

use crate::api_registry::ApiProviderRegistry;
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

const BUILTIN_SOURCE_ID: &str = "builtin";

/// Register every built-in provider against its canonical `Api` key.
///
/// Idempotent: re-invoking simply overwrites the previous registration with a
/// fresh provider instance, which is fine because providers are stateless apart
/// from their internal `reqwest::Client`.
pub fn register_builtins(registry: &ApiProviderRegistry) {
    registry.register(
        Api::OpenAICompletions,
        Box::new(OpenAICompletionsProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::OpenAIResponses,
        Box::new(OpenAIResponsesProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::OpenAICodexResponses,
        Box::new(OpenAICodexResponsesProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::AzureOpenAiResponses,
        Box::new(AzureOpenAIResponsesProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::AnthropicMessages,
        Box::new(AnthropicMessagesProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::BedrockConverseStream,
        Box::new(BedrockProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::GoogleGenerativeAi,
        Box::new(GoogleGenerativeAiProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    // The Gemini CLI variant rides the same Generative AI wire format; reuse
    // the provider so callers that target `GoogleGeminiCli` resolve cleanly.
    registry.register(
        Api::GoogleGeminiCli,
        Box::new(GoogleGenerativeAiProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::GoogleVertex,
        Box::new(GoogleVertexProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
    registry.register(
        Api::MistralConversations,
        Box::new(MistralProvider::new()),
        Some(BUILTIN_SOURCE_ID.to_string()),
    );
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
