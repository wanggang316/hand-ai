//! Integration tests for the model crate.
//!
//! These tests verify the public API surface and ensure that the model registry
//! works correctly with the embedded models.json data.

use model::models::{
    calculate_cost, get_model, get_model_by_provider, get_models, get_models_by_provider,
    get_provider_keys, get_providers, models, models_are_equal, supports_xhigh,
};
use model::types::{
    Api, Compat, Cost, InputType, Model, OpenAICompletionsCompat, Provider, Usage, UsageCost,
};

// =============================================================================
// Model Registry Tests
// =============================================================================

#[test]
fn test_models_json_loads_successfully() {
    let result = models();
    assert!(result.is_ok(), "models.json should parse successfully");

    let registry = result.unwrap();
    assert!(!registry.is_empty(), "model registry should not be empty");
}

#[test]
fn test_provider_keys_returns_sorted_list() {
    let keys = get_provider_keys();
    assert!(!keys.is_empty(), "should have at least one provider");

    // Verify sorted order
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "provider keys should be sorted");
}

#[test]
fn test_get_providers_returns_valid_enums() {
    let providers = get_providers();
    assert!(
        !providers.is_empty(),
        "should have at least one known provider"
    );

    // All returned providers should be valid Provider variants
    for provider in &providers {
        let key = provider.as_str();
        assert!(!key.is_empty(), "provider should have a non-empty key");
    }
}

// =============================================================================
// Model Lookup Tests
// =============================================================================

#[test]
fn test_get_model_by_string_key() {
    // Test with a known provider (OpenAI)
    let model = get_model("openai", "gpt-4o");
    assert!(model.is_some(), "should find gpt-4o model");

    let model = model.unwrap();
    assert_eq!(model.id, "gpt-4o");
    assert_eq!(model.provider, Provider::OpenAI);
}

#[test]
fn test_get_model_by_provider_enum() {
    let model = get_model_by_provider(Provider::OpenAI, "gpt-4o");
    assert!(model.is_some(), "should find gpt-4o using provider enum");

    let model = model.unwrap();
    assert_eq!(model.id, "gpt-4o");
}

#[test]
fn test_get_model_returns_none_for_invalid_id() {
    let model = get_model("openai", "nonexistent-model-12345");
    assert!(model.is_none(), "should return None for invalid model id");
}

#[test]
fn test_get_model_returns_none_for_invalid_provider() {
    let model = get_model("invalid-provider-12345", "gpt-4o");
    assert!(model.is_none(), "should return None for invalid provider");
}

#[test]
fn test_get_models_by_string_key() {
    let models = get_models("openai");
    assert!(!models.is_empty(), "should have OpenAI models");

    // Verify all returned models belong to OpenAI
    for model in &models {
        assert_eq!(model.provider, Provider::OpenAI);
    }
}

#[test]
fn test_get_models_by_provider_enum() {
    let models = get_models_by_provider(Provider::Anthropic);

    // If there are Anthropic models, verify they have the correct provider
    for model in &models {
        assert_eq!(model.provider, Provider::Anthropic);
    }
}

#[test]
fn test_get_models_returns_empty_for_unknown_provider() {
    let models = get_models("unknown-provider-xyz");
    assert!(
        models.is_empty(),
        "should return empty vec for unknown provider"
    );
}

// =============================================================================
// Cost Calculation Tests
// =============================================================================

#[test]
fn test_calculate_cost_basic() {
    let model = create_test_model();
    let mut usage = Usage {
        input: 1_000_000,
        output: 1_000_000,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 2_000_000,
        cost: UsageCost::default(),
    };

    let cost = calculate_cost(&model, &mut usage);

    // input: 1.0/1M * 1M = 1.0
    // output: 2.0/1M * 1M = 2.0
    assert!((cost.input - 1.0).abs() < 0.001, "input cost should be 1.0");
    assert!(
        (cost.output - 2.0).abs() < 0.001,
        "output cost should be 2.0"
    );
    assert!((cost.total - 3.0).abs() < 0.001, "total cost should be 3.0");
}

#[test]
fn test_calculate_cost_with_cache() {
    let model = create_test_model();
    let mut usage = Usage {
        input: 1_000_000,
        output: 500_000,
        cache_read: 2_000_000,
        cache_write: 4_000_000,
        total_tokens: 7_500_000,
        cost: UsageCost::default(),
    };

    let cost = calculate_cost(&model, &mut usage);

    // input: 1.0/1M * 1M = 1.0
    // output: 2.0/1M * 500K = 1.0
    // cache_read: 0.5/1M * 2M = 1.0
    // cache_write: 0.25/1M * 4M = 1.0
    // total = 4.0
    assert!((cost.input - 1.0).abs() < 0.001);
    assert!((cost.output - 1.0).abs() < 0.001);
    assert!((cost.cache_read - 1.0).abs() < 0.001);
    assert!((cost.cache_write - 1.0).abs() < 0.001);
    assert!((cost.total - 4.0).abs() < 0.001);
}

#[test]
fn test_calculate_cost_zero_tokens() {
    let model = create_test_model();
    let mut usage = Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 0,
        cost: UsageCost::default(),
    };

    let cost = calculate_cost(&model, &mut usage);

    assert!(cost.input.abs() < f64::EPSILON);
    assert!(cost.output.abs() < f64::EPSILON);
    assert!(cost.total.abs() < f64::EPSILON);
}

// =============================================================================
// Model Comparison Tests
// =============================================================================

#[test]
fn test_models_are_equal_same_model() {
    let model = create_test_model();
    assert!(models_are_equal(Some(&model), Some(&model)));
}

#[test]
fn test_models_are_equal_identical_models() {
    let a = create_test_model();
    let b = create_test_model();
    assert!(models_are_equal(Some(&a), Some(&b)));
}

#[test]
fn test_models_are_equal_different_ids() {
    let a = create_test_model();
    let mut b = create_test_model();
    b.id = "different-id".to_string();
    assert!(!models_are_equal(Some(&a), Some(&b)));
}

#[test]
fn test_models_are_equal_different_providers() {
    let mut a = create_test_model();
    a.provider = Provider::OpenAI;
    let mut b = create_test_model();
    b.provider = Provider::Anthropic;
    assert!(!models_are_equal(Some(&a), Some(&b)));
}

#[test]
fn test_models_are_equal_with_none() {
    let model = create_test_model();
    assert!(!models_are_equal(Some(&model), None));
    assert!(!models_are_equal(None, Some(&model)));
    assert!(!models_are_equal(None, None));
}

// =============================================================================
// XHigh Support Tests
// =============================================================================

#[test]
fn test_supports_xhigh_true() {
    let mut model = create_test_model();
    model.id = "gpt-5.1-codex-max".to_string();
    assert!(supports_xhigh(&model));

    model.id = "gpt-5.2".to_string();
    assert!(supports_xhigh(&model));

    model.id = "gpt-5.2-codex".to_string();
    assert!(supports_xhigh(&model));
}

#[test]
fn test_supports_xhigh_false() {
    let mut model = create_test_model();
    model.id = "gpt-4o".to_string();
    assert!(!supports_xhigh(&model));

    model.id = "gpt-5.1".to_string();
    assert!(!supports_xhigh(&model));

    model.id = "claude-3-opus".to_string();
    assert!(!supports_xhigh(&model));
}

// =============================================================================
// Provider Serialization Tests
// =============================================================================

#[test]
fn test_provider_roundtrip_all_variants() {
    let providers = vec![
        Provider::AmazonBedrock,
        Provider::Anthropic,
        Provider::Google,
        Provider::GoogleGeminiCli,
        Provider::GoogleAntigravity,
        Provider::GoogleVertex,
        Provider::OpenAI,
        Provider::AzureOpenAiResponses,
        Provider::OpenAICodex,
        Provider::GitHubCopilot,
        Provider::Xai,
        Provider::Groq,
        Provider::Cerebras,
        Provider::Openrouter,
        Provider::VercelAiGateway,
        Provider::Zai,
        Provider::Mistral,
        Provider::Minimax,
        Provider::MinimaxCn,
        Provider::Huggingface,
        Provider::Opencode,
        Provider::KimiCoding,
    ];

    for provider in providers {
        let key = provider.as_str();
        let parsed = Provider::from_str(key);
        assert!(parsed.is_some(), "should parse {key} back to Provider");
        assert_eq!(parsed.unwrap(), provider);
    }
}

#[test]
fn test_provider_from_str_invalid() {
    assert!(Provider::from_str("invalid-provider").is_none());
    assert!(Provider::from_str("").is_none());
    assert!(Provider::from_str("OPENAI").is_none()); // case sensitive
}

// =============================================================================
// Model JSON Validation Tests
// =============================================================================

#[test]
fn test_all_models_have_required_fields() {
    let registry = models().expect("models should parse");

    for (provider_key, models_map) in &registry {
        for (model_id, model) in models_map {
            assert!(
                !model.id.is_empty(),
                "model {model_id} under {provider_key} should have an id"
            );
            assert!(
                !model.name.is_empty(),
                "model {model_id} should have a name"
            );
            assert!(
                model.context_window > 0,
                "model {model_id} should have a positive context_window"
            );
            assert!(
                model.max_tokens > 0,
                "model {model_id} should have a positive max_tokens"
            );
            // base_url is optional for some models (e.g., Azure OpenAI)
        }
    }
}

#[test]
fn test_model_costs_are_non_negative() {
    let registry = models().expect("models should parse");

    for (provider_key, models_map) in &registry {
        for (model_id, model) in models_map {
            assert!(
                model.cost.input >= 0.0,
                "model {model_id} under {provider_key} should have non-negative input cost"
            );
            assert!(
                model.cost.output >= 0.0,
                "model {model_id} should have non-negative output cost"
            );
            assert!(
                model.cost.cache_read >= 0.0,
                "model {model_id} should have non-negative cache_read cost"
            );
            assert!(
                model.cost.cache_write >= 0.0,
                "model {model_id} should have non-negative cache_write cost"
            );
        }
    }
}

#[test]
fn test_model_input_types_are_valid() {
    let registry = models().expect("models should parse");

    for models_map in registry.values() {
        for model in models_map.values() {
            // Input should not be empty
            assert!(
                !model.input.is_empty(),
                "model should have at least one input type"
            );

            // All input types should be valid (Text or Image)
            for input_type in &model.input {
                match input_type {
                    InputType::Text | InputType::Image => {}
                }
            }
        }
    }
}

// =============================================================================
// Api Tests
// =============================================================================

#[test]
fn test_api_variants_exist() {
    // This test ensures all expected API variants compile and exist
    // This test ensures all expected API variants compile and exist
    let _apis = [
        Api::OpenAICompletions,
        Api::OpenAIResponses,
        Api::AzureOpenAiResponses,
        Api::OpenAICodexResponses,
        Api::AnthropicMessages,
        Api::BedrockConverseStream,
        Api::GoogleGenerativeAi,
        Api::GoogleGeminiCli,
        Api::GoogleVertex,
    ];
}

// =============================================================================
// Serialization Tests
// =============================================================================

#[test]
fn test_model_serialization_roundtrip() {
    let original = create_test_model();

    let json = serde_json::to_string(&original).expect("should serialize");
    let deserialized: Model = serde_json::from_str(&json).expect("should deserialize");

    assert_eq!(original.id, deserialized.id);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.provider, deserialized.provider);
    assert_eq!(original.api, deserialized.api);
    assert_eq!(original.base_url, deserialized.base_url);
}

#[test]
fn test_usage_serialization_roundtrip() {
    let original = Usage {
        input: 1000,
        output: 500,
        cache_read: 200,
        cache_write: 100,
        total_tokens: 1800,
        cost: UsageCost {
            input: 0.001,
            output: 0.002,
            cache_read: 0.0001,
            cache_write: 0.0002,
            total: 0.0033,
        },
    };

    let json = serde_json::to_string(&original).expect("should serialize");
    let deserialized: Usage = serde_json::from_str(&json).expect("should deserialize");

    assert_eq!(original.input, deserialized.input);
    assert_eq!(original.output, deserialized.output);
    assert!((original.cost.total - deserialized.cost.total).abs() < f64::EPSILON);
}

#[test]
fn test_compat_serialization() {
    let compat = Compat::OpenAICompletions(OpenAICompletionsCompat {
        supports_store: Some(true),
        supports_developer_role: Some(false),
        supports_reasoning_effort: None,
        thinking_format: Some("openai".to_string()),
        ..Default::default()
    });

    let json = serde_json::to_string(&compat).expect("should serialize");
    assert!(json.contains("openai-completions"));
    assert!(json.contains("supportsStore"));
}

// =============================================================================
// Integration Tests with Real Data
// =============================================================================

#[test]
fn test_common_models_exist() {
    // Test that some common models exist in the registry
    let common_checks = vec![("openai", "gpt-4o"), ("openai", "gpt-4o-mini")];

    for (provider, model_id) in common_checks {
        let model = get_model(provider, model_id);
        assert!(
            model.is_some(),
            "should find common model {model_id} under {provider}"
        );
    }
}

#[test]
fn test_model_provider_consistency() {
    // Ensure that when we look up models by provider key,
    // the returned models have matching provider enums
    let registry = models().expect("models should parse");

    for (provider_key, models_map) in &registry {
        for (model_id, model) in models_map {
            // The model's provider should serialize to the key
            let model_provider_key = model.provider.as_str();
            assert_eq!(
                model_provider_key, provider_key,
                "model {model_id} provider mismatch: enum says {model_provider_key}, but stored under {provider_key}"
            );
        }
    }
}

#[test]
fn test_reasoning_flag_consistency() {
    let registry = models().expect("models should parse");

    for models_map in registry.values() {
        for model in models_map.values() {
            // Models explicitly marked with "-non-reasoning" should have reasoning=false
            if model.id.to_lowercase().contains("non-reasoning") {
                assert!(
                    !model.reasoning,
                    "model {} with 'non-reasoning' in name should have reasoning=false",
                    model.id
                );
            }
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn create_test_model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: Api::OpenAIResponses,
        provider: Provider::OpenAI,
        base_url: "https://api.example.com".to_string(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 0.25,
        },
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}
