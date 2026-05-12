//! Environment variable API key retrieval.
//!
//! Retrieves API keys for providers from known environment variables,
//! e.g. OPENAI_API_KEY.

use crate::types::Provider;
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
            // Per-provider env-var candidates. Each entry is tried in order;
            // first non-empty value wins. The aliases (e.g. ZHIPU_API_KEY for
            // zai, MM_API_KEY for minimax) match the conventions used by the
            // pi-mono / hand-ai user community.
            let candidates: &[&str] = match provider.as_str() {
                "openai" => &["OPENAI_API_KEY"],
                "azure-openai-responses" => &["AZURE_OPENAI_API_KEY"],
                "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
                "groq" => &["GROQ_API_KEY"],
                "cerebras" => &["CEREBRAS_API_KEY"],
                "xai" => &["XAI_API_KEY"],
                "openrouter" => &["OPENROUTER_API_KEY"],
                "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
                "zai" => &["ZAI_API_KEY", "ZHIPU_API_KEY"],
                "deepseek" => &["DEEPSEEK_API_KEY"],
                "mistral" => &["MISTRAL_API_KEY"],
                "minimax" => &["MINIMAX_API_KEY", "MM_API_KEY"],
                "minimax-cn" => &["MINIMAX_CN_API_KEY", "MM_API_KEY"],
                "huggingface" => &["HF_TOKEN"],
                "opencode" => &["OPENCODE_API_KEY"],
                "kimi-coding" => &["KIMI_API_KEY"],
                "moonshotai" | "moonshotai-cn" => &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
                _ => &[],
            };
            candidates
                .iter()
                .find_map(|name| env::var(name).ok().filter(|s| !s.is_empty()))
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

    // Fallback to direct env var lookup with aliases (mirrors the per-
    // provider table in get_env_api_key for unknown / aliased provider ids).
    let candidates: &[&str] = match provider {
        "github-copilot" => &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "azure-openai-responses" => &["AZURE_OPENAI_API_KEY"],
        "google" | "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "zai" | "zhipu" => &["ZAI_API_KEY", "ZHIPU_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY", "MM_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY", "MM_API_KEY"],
        "huggingface" => &["HF_TOKEN"],
        "opencode" => &["OPENCODE_API_KEY"],
        "kimi-coding" | "kimi" => &["KIMI_API_KEY"],
        "moonshotai" | "moonshotai-cn" => &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
        _ => &[],
    };
    candidates
        .iter()
        .find_map(|name| env::var(name).ok().filter(|s| !s.is_empty()))
}

/// Clear the Vertex ADC credentials cache (useful for testing).
pub fn clear_vertex_adc_cache() {
    let mut cache = VERTEX_ADC_CACHE.lock().unwrap();
    *cache = None;
}

/// Fetch a Google Cloud OAuth2 access token from Application Default
/// Credentials.
///
/// Resolution order mirrors the canonical ADC flow:
///
/// 1. `GOOGLE_APPLICATION_CREDENTIALS` — path to a JSON credentials file.
/// 2. `~/.config/gcloud/application_default_credentials.json` — written by
///    `gcloud auth application-default login`.
///
/// Only the `authorized_user` credential type (refresh-token grant) is
/// implemented here; service-account JWT signing is out of scope for the
/// initial M8 milestone. Callers that need service-account auth can supply
/// an explicit `api_key` on `StreamOptions` or pre-mint a token through
/// `gcloud auth application-default print-access-token`.
pub async fn vertex_access_token() -> Result<String, String> {
    let creds_path = if let Ok(path) = env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        PathBuf::from(path)
    } else if let Some(home_dir) = dirs::home_dir() {
        home_dir.join(".config/gcloud/application_default_credentials.json")
    } else {
        return Err("Cannot locate Application Default Credentials: $HOME is not set".to_string());
    };

    if !creds_path.exists() {
        return Err(format!(
            "Application Default Credentials not found at {}. Run `gcloud auth application-default login` or set GOOGLE_APPLICATION_CREDENTIALS.",
            creds_path.display(),
        ));
    }

    let raw = std::fs::read_to_string(&creds_path)
        .map_err(|e| format!("Failed to read ADC file {}: {e}", creds_path.display()))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("Failed to parse ADC JSON: {e}"))?;

    let cred_type = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("authorized_user");

    match cred_type {
        "authorized_user" => exchange_refresh_token(&parsed).await,
        "service_account" => Err(
            "service_account ADC type is not yet supported by vertex_access_token; \
             use `gcloud auth application-default print-access-token` and pass it via api_key."
                .to_string(),
        ),
        other => Err(format!("Unsupported ADC credential type: {other}")),
    }
}

async fn exchange_refresh_token(creds: &serde_json::Value) -> Result<String, String> {
    let client_id = creds
        .get("client_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "ADC file missing client_id".to_string())?;
    let client_secret = creds
        .get("client_secret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "ADC file missing client_secret".to_string())?;
    let refresh_token = creds
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "ADC file missing refresh_token".to_string())?;

    let form = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let client = reqwest::Client::new();
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Failed to exchange refresh token: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("Refresh-token exchange failed ({status}): {body}"));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "Token response missing access_token field".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_env_api_key_returns_none_when_not_set() {
        // Most CI environments won't have provider keys set
        // This tests the fallback behavior.
        let result = get_env_api_key(&Provider::Zai);
        // Zai accepts both ZAI_API_KEY (canonical) and ZHIPU_API_KEY
        // (community alias). The result should be None only when *neither*
        // is set.
        let zai_set = env::var("ZAI_API_KEY").is_ok_and(|v| !v.is_empty());
        let zhipu_set = env::var("ZHIPU_API_KEY").is_ok_and(|v| !v.is_empty());
        if !zai_set && !zhipu_set {
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_env_api_key_by_str_invalid_provider() {
        let result = get_env_api_key_by_str("nonexistent-provider");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_env_api_key_by_str_known_provider() {
        // Should return None if env var not set, but should not panic
        let result = get_env_api_key_by_str("openai");
        if env::var("OPENAI_API_KEY").is_err() {
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_clear_vertex_adc_cache() {
        // Should not panic
        clear_vertex_adc_cache();
    }

    #[test]
    fn test_bedrock_credentials_detection() {
        // When no AWS env vars are set, should return None
        if env::var("AWS_PROFILE").is_err()
            && env::var("AWS_ACCESS_KEY_ID").is_err()
            && env::var("AWS_BEARER_TOKEN_BEDROCK").is_err()
            && env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_err()
            && env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_err()
            && env::var("AWS_WEB_IDENTITY_TOKEN_FILE").is_err()
        {
            assert!(get_env_api_key(&Provider::AmazonBedrock).is_none());
        }
    }

    #[test]
    fn test_github_copilot_key_sources() {
        // Should check COPILOT_GITHUB_TOKEN, GH_TOKEN, GITHUB_TOKEN
        if env::var("COPILOT_GITHUB_TOKEN").is_err()
            && env::var("GH_TOKEN").is_err()
            && env::var("GITHUB_TOKEN").is_err()
        {
            assert!(get_env_api_key(&Provider::GitHubCopilot).is_none());
        }
    }

    #[test]
    fn test_anthropic_key_sources() {
        if env::var("ANTHROPIC_OAUTH_TOKEN").is_err() && env::var("ANTHROPIC_API_KEY").is_err() {
            assert!(get_env_api_key(&Provider::Anthropic).is_none());
        }
    }
}
