//! Cloudflare overlays on top of the OpenAI Completions wire protocol.
//!
//! Mirrors `pi-mono/packages/ai/src/providers/cloudflare.ts`. Cloudflare
//! exposes two OpenAI-compatible endpoints:
//!
//! - **Workers AI**: a direct endpoint at
//!   `https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1`. The
//!   upstream does not support OpenAI's strict tool-call mode, so the overlay
//!   sets `supports_strict_mode = false`.
//! - **AI Gateway (OpenAI passthrough)**: routes through Cloudflare at
//!   `https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openai`,
//!   preserving the upstream OpenAI shape verbatim. No compat overrides.
//!
//! These helpers do not implement `ApiProvider`; they just construct `Model`
//! instances with the right `base_url` and `Compat`. The existing
//! `OpenAICompletionsProvider` registered against `Api::OpenAICompletions`
//! handles the wire protocol.

use crate::types::{Api, Compat, Cost, InputType, Model, OpenAICompletionsCompat, Provider};

/// Default context window applied to Cloudflare overlay models when callers
/// do not customise it. 128k matches the typical OpenAI-compatible ceiling
/// and is overridable by mutating the returned `Model`.
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// Default max output tokens for Cloudflare overlay models.
const DEFAULT_MAX_TOKENS: u64 = 16_384;

fn default_cloudflare_model(
    id: String,
    provider: Provider,
    base_url: String,
    compat: Option<Compat>,
) -> Model {
    Model {
        name: id.clone(),
        id,
        api: Api::OpenAICompletions,
        provider,
        base_url,
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_tokens: DEFAULT_MAX_TOKENS,
        headers: None,
        compat,
        thinking_level_map: None,
    }
}

/// Cloudflare Workers AI: thin overlay on `openai-completions`.
///
/// Workers AI does not support OpenAI's strict tool-call mode, so the returned
/// `Model` carries `supports_strict_mode = Some(false)` in its compat overrides.
pub fn cloudflare_workers_ai_model(account_id: &str, model_id: impl Into<String>) -> Model {
    let base_url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1");
    let compat = Compat::OpenAICompletions(Box::new(OpenAICompletionsCompat {
        supports_strict_mode: Some(false),
        ..Default::default()
    }));
    default_cloudflare_model(
        model_id.into(),
        Provider::CloudflareWorkersAi,
        base_url,
        Some(compat),
    )
}

/// Cloudflare AI Gateway (OpenAI passthrough): thin overlay on
/// `openai-completions`.
///
/// The gateway preserves the upstream OpenAI shape verbatim, so no compat
/// overrides are applied.
pub fn cloudflare_ai_gateway_model(
    account_id: &str,
    gateway_id: &str,
    model_id: impl Into<String>,
) -> Model {
    let base_url = format!("https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openai");
    default_cloudflare_model(
        model_id.into(),
        Provider::CloudflareAiGateway,
        base_url,
        None,
    )
}
