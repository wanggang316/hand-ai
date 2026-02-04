//! Environment variable API key retrieval.
//!
//! Retrieves API keys for providers from known environment variables,
//! e.g. OPENAI_API_KEY.

use crate::types::Provider;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Mutex;

/// Cached result for Vertex ADC credentials check.
static VERTEX_ADC_CACHE: Mutex<Option<bool>> = Mutex::new(None);

/// Check if Vertex ADC credentials exist.
fn has_vertex_adc_credentials() -> bool {
    let mut cache = VERTEX_ADC_CACHE.lock().unwrap();

    if let Some(cached) = *cache {
        return cached;
    }

    let result = check_vertex_adc_credentials();
    *cache = Some(result);
    result
}

fn check_vertex_adc_credentials() -> bool {
    // Check GOOGLE_APPLICATION_CREDENTIALS env var first (standard way)
    if let Ok(gac_path) = env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        return PathBuf::from(gac_path).exists();
    }

    // Fall back to default ADC path
    if let Some(home_dir) = dirs::home_dir() {
        let adc_path = home_dir.join(".config/gcloud/application_default_credentials.json");
        return adc_path.exists();
    }

    false
}

/// Get API key for provider from known environment variables.
///
/// Will not return API keys for providers that require OAuth tokens.
pub fn get_env_api_key(provider: &Provider) -> Option<String> {
    let key = match provider {
        Provider::GitHubCopilot => env::var("COPILOT_GITHUB_TOKEN")
            .or_else(|_| env::var("GH_TOKEN"))
            .or_else(|_| env::var("GITHUB_TOKEN"))
            .ok(),
        Provider::Anthropic => {
            // ANTHROPIC_OAUTH_TOKEN takes precedence over ANTHROPIC_API_KEY
            env::var("ANTHROPIC_OAUTH_TOKEN")
                .or_else(|_| env::var("ANTHROPIC_API_KEY"))
                .ok()
        }
        Provider::GoogleVertex => {
            // Vertex AI uses Application Default Credentials, not API keys.
            // Auth is configured via `gcloud auth application-default login`.
            let has_credentials = has_vertex_adc_credentials();
            let has_project =
                env::var("GOOGLE_CLOUD_PROJECT").is_ok() || env::var("GCLOUD_PROJECT").is_ok();
            let has_location = env::var("GOOGLE_CLOUD_LOCATION").is_ok();

            if has_credentials && has_project && has_location {
                Some("<authenticated>".to_string())
            } else {
                None
            }
        }
        Provider::AmazonBedrock => {
            // Amazon Bedrock supports multiple credential sources:
            // 1. AWS_PROFILE - named profile from ~/.aws/credentials
            // 2. AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY - standard IAM keys
            // 3. AWS_BEARER_TOKEN_BEDROCK - Bedrock API keys (bearer token)
            // 4. AWS_CONTAINER_CREDENTIALS_RELATIVE_URI - ECS task roles
            // 5. AWS_CONTAINER_CREDENTIALS_FULL_URI - ECS task roles (full URI)
            // 6. AWS_WEB_IDENTITY_TOKEN_FILE - IRSA (IAM Roles for Service Accounts)
            if env::var("AWS_PROFILE").is_ok()
                || (env::var("AWS_ACCESS_KEY_ID").is_ok()
                    && env::var("AWS_SECRET_ACCESS_KEY").is_ok())
                || env::var("AWS_BEARER_TOKEN_BEDROCK").is_ok()
                || env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_ok()
                || env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_ok()
                || env::var("AWS_WEB_IDENTITY_TOKEN_FILE").is_ok()
            {
                Some("<authenticated>".to_string())
            } else {
                None
            }
        }
        _ => {
            // Map provider to environment variable name
            let env_map: HashMap<String, String> = [
                ("openai", "OPENAI_API_KEY"),
                ("azure-openai-responses", "AZURE_OPENAI_API_KEY"),
                ("google", "GEMINI_API_KEY"),
                ("groq", "GROQ_API_KEY"),
                ("cerebras", "CEREBRAS_API_KEY"),
                ("xai", "XAI_API_KEY"),
                ("openrouter", "OPENROUTER_API_KEY"),
                ("vercel-ai-gateway", "AI_GATEWAY_API_KEY"),
                ("zai", "ZAI_API_KEY"),
                ("mistral", "MISTRAL_API_KEY"),
                ("minimax", "MINIMAX_API_KEY"),
                ("minimax-cn", "MINIMAX_CN_API_KEY"),
                ("huggingface", "HF_TOKEN"),
                ("opencode", "OPENCODE_API_KEY"),
                ("kimi-coding", "KIMI_API_KEY"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

            let provider_key = provider.as_str();
            env_map
                .get(provider_key)
                .and_then(|var_name| env::var(var_name).ok())
        }
    };

    key.filter(|s| !s.is_empty())
}

/// Get API key for a provider by string key.
pub fn get_env_api_key_by_str(provider: &str) -> Option<String> {
    // First try to parse as Provider enum
    if let Some(provider) = Provider::from_str(provider) {
        return get_env_api_key(&provider);
    }

    // Fallback to direct env var lookup
    let env_map: HashMap<String, String> = [
        ("github-copilot", "COPILOT_GITHUB_TOKEN"),
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("openai", "OPENAI_API_KEY"),
        ("azure-openai-responses", "AZURE_OPENAI_API_KEY"),
        ("google", "GEMINI_API_KEY"),
        ("groq", "GROQ_API_KEY"),
        ("cerebras", "CEREBRAS_API_KEY"),
        ("xai", "XAI_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("vercel-ai-gateway", "AI_GATEWAY_API_KEY"),
        ("zai", "ZAI_API_KEY"),
        ("mistral", "MISTRAL_API_KEY"),
        ("minimax", "MINIMAX_API_KEY"),
        ("minimax-cn", "MINIMAX_CN_API_KEY"),
        ("huggingface", "HF_TOKEN"),
        ("opencode", "OPENCODE_API_KEY"),
        ("kimi-coding", "KIMI_API_KEY"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    env_map
        .get(provider)
        .and_then(|var_name| env::var(var_name).ok())
        .filter(|s| !s.is_empty())
}

/// Clear the Vertex ADC credentials cache (useful for testing).
pub fn clear_vertex_adc_cache() {
    let mut cache = VERTEX_ADC_CACHE.lock().unwrap();
    *cache = None;
}
