//! Parity tests for the Cloudflare overlay helpers.
//!
//! These overlays are pure `Model` constructors on top of the existing
//! `openai-completions` wire protocol. The tests assert the URL shape and
//! compat overrides so the registry-driven pipeline routes Cloudflare-keyed
//! models through `OpenAICompletionsProvider` with the right base URL.

use model::providers::{cloudflare_ai_gateway_model, cloudflare_workers_ai_model};
use model::types::{Api, Compat, Provider};

#[test]
fn cloudflare_workers_ai_url_construction() {
    let model = cloudflare_workers_ai_model("acct-123", "@cf/meta/llama-3.1-8b-instruct");

    assert_eq!(
        model.base_url,
        "https://api.cloudflare.com/client/v4/accounts/acct-123/ai/v1"
    );
    assert_eq!(model.api, Api::OpenAICompletions);
    assert_eq!(model.provider, Provider::CloudflareWorkersAi);
    assert_eq!(model.id, "@cf/meta/llama-3.1-8b-instruct");
}

#[test]
fn cloudflare_workers_ai_compat_disables_strict_mode() {
    let model = cloudflare_workers_ai_model("acct-123", "@cf/meta/llama-3.1-8b-instruct");

    let compat = model.compat.expect("workers-ai overlay must set compat");
    match compat {
        Compat::OpenAICompletions(inner) => {
            assert_eq!(inner.supports_strict_mode, Some(false));
            // The overlay only flips strict-mode; nothing else should be
            // pre-configured. Spot-check a couple of fields to guard against
            // accidental defaults leaking in.
            assert_eq!(inner.supports_store, None);
            assert_eq!(inner.thinking_format, None);
        }
        other => panic!("expected OpenAICompletions compat, got {other:?}"),
    }
}

#[test]
fn cloudflare_ai_gateway_url_construction() {
    let model = cloudflare_ai_gateway_model("acct-123", "gw-abc", "gpt-4o-mini");

    assert_eq!(
        model.base_url,
        "https://gateway.ai.cloudflare.com/v1/acct-123/gw-abc/openai"
    );
    assert_eq!(model.api, Api::OpenAICompletions);
    assert_eq!(model.provider, Provider::CloudflareAiGateway);
    assert_eq!(model.id, "gpt-4o-mini");
}

#[test]
fn cloudflare_ai_gateway_no_compat_overrides() {
    let model = cloudflare_ai_gateway_model("acct-123", "gw-abc", "gpt-4o-mini");

    assert!(
        model.compat.is_none(),
        "AI Gateway passthrough must not introduce compat overrides; got {:?}",
        model.compat
    );
}
