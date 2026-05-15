//! Coverage tests for `register_builtins`.
//!
//! Verifies that `register_builtins` covers every canonical `Api` variant,
//! that re-invocation is safe, and that `Client::default()` ships with the
//! same coverage out of the box.

use model::api_registry::ApiProviderRegistry;
use model::types::Api;
use model::{Client, register_builtins};

/// The set of canonical APIs that `register_builtins` must populate.
///
/// `Api::Faux` is intentionally excluded — it lives behind the `faux` feature
/// and is only registered by `register_builtins_with_faux`.
const CANONICAL_APIS: &[Api] = &[
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

#[test]
fn register_builtins_registers_all_known_apis() {
    let registry = ApiProviderRegistry::new();
    register_builtins(&registry);
    for api in CANONICAL_APIS {
        assert!(
            registry.has(api),
            "register_builtins did not register {api:?}"
        );
    }
}

#[test]
fn register_builtins_is_idempotent() {
    let registry = ApiProviderRegistry::new();
    register_builtins(&registry);
    register_builtins(&registry);
    // Second invocation must not panic, drop providers, or otherwise corrupt
    // the registry. Every canonical api still resolves.
    for api in CANONICAL_APIS {
        assert!(
            registry.has(api),
            "{api:?} disappeared after second register_builtins call"
        );
    }
}

#[test]
fn client_default_uses_register_builtins() {
    let client = Client::default();
    for api in CANONICAL_APIS {
        assert!(
            client.registry.get(api).is_some(),
            "Client::default() missing provider for {api:?}"
        );
    }
}
