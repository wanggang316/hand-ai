//! Compat-resolution coverage for the OpenAI Completions provider.
//!
//! Verifies the precedence rules of `resolve_compat`:
//! 1. Explicit `model.compat` overrides win.
//! 2. URL substring matches on `model.base_url` populate provider-specific
//!    knobs (OpenRouter, Z.ai/bigmodel.cn, Qwen/dashscope, DeepSeek,
//!    Cloudflare Workers AI).
//! 3. Unknown URLs return the OpenAI defaults.

use model::providers::resolve_compat;
use model::types::{
    Api, Compat, Cost, InputType, Model, OpenAICompletionsCompat, OpenRouterRouting, Provider,
    SessionAffinityFormat,
};

fn base_model() -> Model {
    Model {
        id: "test".to_string(),
        name: "Test".to_string(),
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

#[test]
fn resolve_compat_explicit_override_wins() {
    let mut model = base_model();
    model.base_url = "https://api.deepseek.com/v1".to_string(); // would auto-detect "deepseek"
    model.compat = Some(Compat::OpenAICompletions(Box::new(
        OpenAICompletionsCompat {
            thinking_format: Some("custom".to_string()),
            supports_strict_mode: Some(true),
            ..Default::default()
        },
    )));

    let resolved = resolve_compat(&model);
    assert_eq!(resolved.thinking_format.as_deref(), Some("custom"));
    assert!(resolved.supports_strict_mode);
}

#[test]
fn resolve_compat_openrouter_url_detection() {
    let mut model = base_model();
    model.base_url = "https://openrouter.ai/api/v1".to_string();

    let resolved = resolve_compat(&model);
    assert!(
        resolved.open_router_routing.is_some(),
        "openrouter.ai base_url should populate open_router_routing default"
    );
    assert_eq!(resolved.thinking_format.as_deref(), Some("openrouter"));
    assert_eq!(
        resolved.session_affinity_format,
        SessionAffinityFormat::OpenRouter,
        "openrouter.ai base_url should pick the x-session-id affinity format"
    );
}

#[test]
fn resolve_compat_zai_url_detection() {
    let mut model = base_model();
    model.base_url = "https://open.bigmodel.cn/api/paas/v4".to_string();

    let resolved = resolve_compat(&model);
    assert!(
        resolved.zai_tool_stream,
        "bigmodel.cn base_url should set zai_tool_stream"
    );
    assert_eq!(resolved.thinking_format.as_deref(), Some("zai"));
}

#[test]
fn resolve_compat_qwen_url_detection() {
    let mut model = base_model();
    model.base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string();

    let resolved = resolve_compat(&model);
    assert_eq!(resolved.thinking_format.as_deref(), Some("qwen"));
}

#[test]
fn resolve_compat_deepseek_url_detection() {
    let mut model = base_model();
    model.base_url = "https://api.deepseek.com/v1".to_string();

    let resolved = resolve_compat(&model);
    assert_eq!(resolved.thinking_format.as_deref(), Some("deepseek"));
}

#[test]
fn resolve_compat_cloudflare_workers_ai_detection() {
    let mut model = base_model();
    model.base_url = "https://api.cloudflare.com/client/v4/accounts/abc/ai/v1".to_string();

    let resolved = resolve_compat(&model);
    assert!(
        !resolved.supports_strict_mode,
        "Cloudflare Workers AI does not support OpenAI strict mode"
    );
}

#[test]
fn resolve_compat_unknown_url_returns_default() {
    let mut model = base_model();
    model.base_url = "https://example.com/v1".to_string();

    let resolved = resolve_compat(&model);
    // Unknown URL with the OpenAI provider falls through to the OpenAI defaults.
    assert_eq!(resolved.thinking_format.as_deref(), Some("openai"));
    assert!(resolved.open_router_routing.is_none());
    assert!(!resolved.zai_tool_stream);
    assert!(resolved.supports_strict_mode);
    assert!(resolved.supports_store);
    assert!(resolved.supports_developer_role);
    assert!(resolved.supports_reasoning_effort);
    assert_eq!(
        resolved.session_affinity_format,
        SessionAffinityFormat::OpenAI
    );
}

#[test]
fn resolve_compat_session_affinity_format_override_wins() {
    // Explicit compat pins the OpenAI-style header set even though the
    // base_url would auto-detect the OpenRouter format.
    let mut model = base_model();
    model.base_url = "https://openrouter.ai/api/v1".to_string();
    model.compat = Some(Compat::OpenAICompletions(Box::new(
        OpenAICompletionsCompat {
            session_affinity_format: Some(SessionAffinityFormat::OpenAI),
            ..Default::default()
        },
    )));

    let resolved = resolve_compat(&model);
    assert_eq!(
        resolved.session_affinity_format,
        SessionAffinityFormat::OpenAI
    );
}

#[test]
fn resolve_compat_session_affinity_format_deserializes_from_catalog_json() {
    // Model catalog compat blocks are plain JSON; the kebab-case wire
    // values must round-trip into the typed enum.
    let compat: Compat = serde_json::from_str(
        r#"{
            "type": "openai-completions",
            "sendSessionAffinityHeaders": true,
            "sessionAffinityFormat": "openai-nosession"
        }"#,
    )
    .expect("catalog compat block should deserialize");

    let mut model = base_model();
    model.compat = Some(compat);
    let resolved = resolve_compat(&model);
    assert!(resolved.send_session_affinity_headers);
    assert_eq!(
        resolved.session_affinity_format,
        SessionAffinityFormat::OpenAINoSession
    );
}

#[test]
fn resolve_compat_explicit_open_router_routing_wins() {
    // Sanity: explicit OpenRouter routing override applies even when the
    // base_url doesn't carry the openrouter.ai marker.
    let mut model = base_model();
    let routing = OpenRouterRouting {
        allow_fallbacks: Some(false),
        ..Default::default()
    };
    model.compat = Some(Compat::OpenAICompletions(Box::new(
        OpenAICompletionsCompat {
            open_router_routing: Some(routing),
            ..Default::default()
        },
    )));

    let resolved = resolve_compat(&model);
    let r = resolved
        .open_router_routing
        .expect("explicit routing should pass through");
    assert_eq!(r.allow_fallbacks, Some(false));
}
