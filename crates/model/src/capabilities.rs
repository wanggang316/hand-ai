//! Provider and API capability introspection.
//!
//! Exposes intrinsic facts about each provider (vendor) and each API
//! (wire protocol) so downstream embedders can drive UI surfaces such
//! as provider selection panels or auth-mode picker without having to
//! reverse-engineer the catalog.
//!
//! ## Layering
//!
//! Capability lives at the layer where it is invariant:
//! - [`ProviderCapabilities`] — facts that hold for the vendor regardless
//!   of model or API (e.g. OAuth login flow availability).
//! - [`ApiCapabilities`] — facts that hold for the wire protocol regardless
//!   of vendor (e.g. native tool/function calling support).
//! - Per-model facts (multimodal input, thinking, context window, cost) live
//!   on [`crate::types::Model`] directly via `input`, `reasoning`,
//!   `context_window`, `cost`. They are intentionally not duplicated here.
//!
//! ## Forward compatibility
//!
//! Both structs are `#[non_exhaustive]`. New capability fields can be added
//! without a SemVer-major bump; consumers must construct via library APIs and
//! read fields explicitly. New `Provider` or `Api` enum variants force an
//! exhaustive match update here, so capability tables cannot silently drift.

use serde::{Deserialize, Serialize};

use crate::types::{Api, Provider};

/// Capabilities intrinsic to a provider (vendor).
///
/// Captures vendor-level facts only. Model-level facts (multimodal,
/// reasoning) come from [`crate::types::Model`]; protocol-level facts (tools)
/// come from [`ApiCapabilities`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Provider accepts simple API-key authentication (typically via header).
    ///
    /// False when the vendor only supports OAuth or external credential
    /// chains (e.g. GitHub Copilot, AWS Bedrock, Google Vertex).
    pub api_key_auth: bool,

    /// Provider supports an interactive OAuth login flow exposed by this
    /// library (matches [`crate::oauth::OAuthProviderId`] coverage).
    pub oauth_auth: bool,

    /// Provider allows callers to override the request base URL.
    ///
    /// True for OpenAI-compatible vendors (OpenRouter, Mistral, Z.AI, …) and
    /// for vendors that *require* a tenant-specific endpoint (Azure). False
    /// for vendors with a fixed first-party endpoint.
    pub custom_base_url: bool,
}

impl ProviderCapabilities {
    /// Conservative default: API-key auth only.
    pub const fn api_key_only() -> Self {
        Self {
            api_key_auth: true,
            oauth_auth: false,
            custom_base_url: false,
        }
    }
}

/// Capabilities intrinsic to an API protocol (wire format).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiCapabilities {
    /// Protocol natively supports tool / function calling.
    ///
    /// Reflects protocol *design*, not runtime guarantees. A third-party
    /// server claiming OpenAI-compatibility may still reject tool calls; that
    /// is a deployment fact this library cannot assert.
    pub tools: bool,
}

impl Provider {
    /// Vendor-level capabilities for this provider.
    ///
    /// Match is exhaustive on purpose: new `Provider` variants must
    /// explicitly declare their capabilities.
    pub const fn capabilities(self) -> ProviderCapabilities {
        match self {
            // OAuth-capable vendors. See `crate::oauth::OAuthProviderId`.
            Provider::Anthropic => ProviderCapabilities {
                api_key_auth: true,
                oauth_auth: true,
                custom_base_url: true,
            },
            Provider::OpenAICodex => ProviderCapabilities {
                api_key_auth: true,
                oauth_auth: true,
                custom_base_url: false,
            },
            Provider::GitHubCopilot => ProviderCapabilities {
                api_key_auth: false,
                oauth_auth: true,
                custom_base_url: false,
            },

            // Cloud vendors that authenticate via external credential chains.
            Provider::AmazonBedrock => ProviderCapabilities {
                api_key_auth: false,
                oauth_auth: false,
                custom_base_url: false,
            },
            Provider::GoogleVertex => ProviderCapabilities {
                api_key_auth: false,
                oauth_auth: false,
                custom_base_url: false,
            },

            // Tenant-endpoint vendors: API key + mandatory custom base URL.
            Provider::AzureOpenAiResponses => ProviderCapabilities {
                api_key_auth: true,
                oauth_auth: false,
                custom_base_url: true,
            },

            // First-party SaaS with fixed endpoints.
            Provider::OpenAI | Provider::Google => ProviderCapabilities::api_key_only(),

            // Google sub-surfaces. Conservative until per-surface auth is
            // separately audited.
            Provider::GoogleGeminiCli | Provider::GoogleAntigravity => {
                ProviderCapabilities::api_key_only()
            }

            // OpenAI-compatible aggregators / hosters. All accept API keys and
            // expose a base URL so the same client can target alternate
            // regions or proxies.
            Provider::Openrouter
            | Provider::VercelAiGateway
            | Provider::Mistral
            | Provider::Zai
            | Provider::Xai
            | Provider::Groq
            | Provider::Cerebras
            | Provider::Fireworks
            | Provider::Deepseek
            | Provider::Moonshotai
            | Provider::MoonshotaiCn
            | Provider::Minimax
            | Provider::MinimaxCn
            | Provider::Huggingface
            | Provider::Opencode
            | Provider::OpencodeGo
            | Provider::KimiCoding
            | Provider::CloudflareWorkersAi
            | Provider::CloudflareAiGateway
            | Provider::Xiaomi
            | Provider::XiaomiTokenPlanCn
            | Provider::XiaomiTokenPlanAms
            | Provider::XiaomiTokenPlanSgp => ProviderCapabilities {
                api_key_auth: true,
                oauth_auth: false,
                custom_base_url: true,
            },
        }
    }
}

impl Api {
    /// Protocol-level capabilities for this API.
    pub const fn capabilities(self) -> ApiCapabilities {
        match self {
            // All modern wire protocols here advertise tool/function calling.
            // Faux is the in-memory harness and advertises tools so test
            // scripts can exercise the full event surface.
            Api::OpenAICompletions
            | Api::OpenAIResponses
            | Api::AzureOpenAiResponses
            | Api::OpenAICodexResponses
            | Api::AnthropicMessages
            | Api::BedrockConverseStream
            | Api::GoogleGenerativeAi
            | Api::GoogleGeminiCli
            | Api::GoogleVertex
            | Api::MistralConversations
            | Api::Faux => ApiCapabilities { tools: true },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_capable_providers_match_oauth_provider_id_set() {
        // The three known OAuth-capable vendors. If this drifts from the
        // `OAuthProviderId` enum, capability metadata is lying to consumers.
        assert!(Provider::Anthropic.capabilities().oauth_auth);
        assert!(Provider::OpenAICodex.capabilities().oauth_auth);
        assert!(Provider::GitHubCopilot.capabilities().oauth_auth);
    }

    #[test]
    fn non_oauth_providers_do_not_advertise_oauth() {
        for p in [
            Provider::OpenAI,
            Provider::Google,
            Provider::AmazonBedrock,
            Provider::GoogleVertex,
            Provider::AzureOpenAiResponses,
            Provider::Openrouter,
            Provider::Mistral,
        ] {
            assert!(
                !p.capabilities().oauth_auth,
                "{:?} unexpectedly advertises oauth_auth=true",
                p
            );
        }
    }

    #[test]
    fn github_copilot_is_oauth_only() {
        let caps = Provider::GitHubCopilot.capabilities();
        assert!(!caps.api_key_auth, "Copilot has no API-key auth path");
        assert!(caps.oauth_auth);
    }

    #[test]
    fn bedrock_and_vertex_authenticate_externally() {
        for p in [Provider::AmazonBedrock, Provider::GoogleVertex] {
            let caps = p.capabilities();
            assert!(
                !caps.api_key_auth,
                "{:?} should not advertise api_key_auth",
                p
            );
            assert!(!caps.oauth_auth);
        }
    }

    #[test]
    fn azure_requires_custom_base_url() {
        let caps = Provider::AzureOpenAiResponses.capabilities();
        assert!(caps.api_key_auth);
        assert!(caps.custom_base_url);
    }

    #[test]
    fn all_apis_advertise_tools() {
        // Conservative: every API in the catalog advertises tool support at
        // the protocol level. If a future API lacks it, this test will force
        // the override to be added explicitly.
        for api in [
            Api::OpenAICompletions,
            Api::OpenAIResponses,
            Api::AzureOpenAiResponses,
            Api::OpenAICodexResponses,
            Api::AnthropicMessages,
            Api::BedrockConverseStream,
            Api::GoogleGenerativeAi,
            Api::GoogleGeminiCli,
            Api::GoogleVertex,
            Api::MistralConversations,
            Api::Faux,
        ] {
            assert!(api.capabilities().tools, "{:?} should advertise tools", api);
        }
    }

    #[test]
    fn capabilities_are_const() {
        // Forces compile-time evaluation; const fns can be used in const
        // contexts such as `static` declarations downstream.
        const _: ProviderCapabilities = Provider::OpenAI.capabilities();
        const _: ApiCapabilities = Api::OpenAICompletions.capabilities();
    }
}
