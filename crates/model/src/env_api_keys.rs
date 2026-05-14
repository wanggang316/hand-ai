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
            // Vertex AI supports two auth paths:
            // 1. Explicit `GOOGLE_CLOUD_API_KEY` — the Cloud API key
            //    issued from the GCP console. When set, it wins and
            //    bypasses the ADC + project + location triad.
            // 2. Application Default Credentials, configured via
            //    `gcloud auth application-default login`, paired with
            //    `GOOGLE_CLOUD_PROJECT`/`GCLOUD_PROJECT` and
            //    `GOOGLE_CLOUD_LOCATION`.
            if let Ok(api_key) = env::var("GOOGLE_CLOUD_API_KEY")
                && !api_key.is_empty()
            {
                return Some(api_key);
            }

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
            // zai, MM_API_KEY for minimax) match the conventions established
            // by the existing user community.
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
                "fireworks" => &["FIREWORKS_API_KEY"],
                "opencode" => &["OPENCODE_API_KEY"],
                "kimi-coding" => &["KIMI_API_KEY"],
                "moonshotai" | "moonshotai-cn" => &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
                // Xiaomi MiMo: API billing endpoint (api.xiaomimimo.com)
                // uses a single key. The Token Plan endpoints are
                // separate providers with their own per-region keys
                // because a platform.xiaomimimo.com key fails against
                // the Token Plan endpoint and vice versa.
                "xiaomi" => &["XIAOMI_API_KEY"],
                "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
                "xiaomi-token-plan-ams" => &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
                "xiaomi-token-plan-sgp" => &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
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
        "fireworks" => &["FIREWORKS_API_KEY"],
        "opencode" => &["OPENCODE_API_KEY"],
        "kimi-coding" | "kimi" => &["KIMI_API_KEY"],
        "moonshotai" | "moonshotai-cn" => &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        "xiaomi-token-plan-ams" => &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
        "xiaomi-token-plan-sgp" => &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
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

    /// Fireworks AI is served via the OpenAI-compatible Completions API
    /// and authenticates with `FIREWORKS_API_KEY`. Both the enum lookup
    /// and the string-keyed fallback must surface a value set in the
    /// environment.
    #[test]
    fn test_fireworks_key_sources() {
        // SAFETY: tests run single-threaded for this crate; we restore
        // the prior value on exit.
        let prior = env::var("FIREWORKS_API_KEY").ok();
        unsafe {
            env::set_var("FIREWORKS_API_KEY", "fw_test_sentinel");
        }
        assert_eq!(
            get_env_api_key(&Provider::Fireworks).as_deref(),
            Some("fw_test_sentinel")
        );
        assert_eq!(
            get_env_api_key_by_str("fireworks").as_deref(),
            Some("fw_test_sentinel")
        );
        unsafe {
            match prior {
                Some(v) => env::set_var("FIREWORKS_API_KEY", v),
                None => env::remove_var("FIREWORKS_API_KEY"),
            }
        }
    }

    /// Vertex AI accepts a direct `GOOGLE_CLOUD_API_KEY` in addition
    /// to the standard ADC + project + location triad. Without the
    /// fast path, callers who exported an API key in their shell
    /// hit the slower ADC code path (and failed if any of the three
    /// triad vars were missing).
    #[test]
    fn vertex_prefers_google_cloud_api_key_when_set() {
        // Skip if a real value happens to be set in the dev env —
        // we don't want this test to flake based on user shell.
        if env::var("GOOGLE_CLOUD_API_KEY").is_ok_and(|v| !v.is_empty()) {
            // Real value set: just exercise the code path.
            let resolved = get_env_api_key(&Provider::GoogleVertex);
            assert!(
                resolved.is_some(),
                "with a real GOOGLE_CLOUD_API_KEY the resolver must hand it back"
            );
            return;
        }
        // No real key set — `get_env_api_key(GoogleVertex)` falls
        // through to the ADC triad check, which returns None when
        // any triad var is missing. Don't try to mutate the
        // process-wide env from a parallel test; pinning the
        // negative path here is enough.
        let resolved = get_env_api_key(&Provider::GoogleVertex);
        let has_triad = env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
            && (env::var("GOOGLE_CLOUD_PROJECT").is_ok() || env::var("GCLOUD_PROJECT").is_ok())
            && env::var("GOOGLE_CLOUD_LOCATION").is_ok();
        if !has_triad {
            assert!(resolved.is_none());
        }
    }

    /// Each Xiaomi MiMo provider variant must read from its own env
    /// var: the API billing endpoint (`xiaomi`) uses `XIAOMI_API_KEY`,
    /// and each Token Plan region uses its own per-region key. A
    /// platform key fails against the Token Plan endpoint and vice
    /// versa, so the mappings must stay distinct.
    #[test]
    fn xiaomi_provider_variants_read_distinct_env_vars() {
        let cases = [
            (Provider::Xiaomi, "XIAOMI_API_KEY"),
            (Provider::XiaomiTokenPlanCn, "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
            (Provider::XiaomiTokenPlanAms, "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
            (Provider::XiaomiTokenPlanSgp, "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
        ];
        for (provider, expected_env) in cases {
            // Skip the assertion if the env var happens to be set in
            // the dev environment — the test should not flake based
            // on the developer's shell.
            if env::var(expected_env).is_ok_and(|v| !v.is_empty()) {
                continue;
            }
            assert!(
                get_env_api_key(&provider).is_none(),
                "{provider:?} should resolve to {expected_env} (empty) -> None"
            );
        }
    }

    /// String-keyed access path (`get_env_api_key_by_str`) must map
    /// each Xiaomi variant onto the same env var as the enum path.
    /// This is the path used by CLI flag parsing where the provider
    /// id arrives as a string.
    #[test]
    fn xiaomi_by_str_path_mirrors_enum_mappings() {
        let ids = [
            "xiaomi",
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-ams",
            "xiaomi-token-plan-sgp",
        ];
        let envs = [
            "XIAOMI_API_KEY",
            "XIAOMI_TOKEN_PLAN_CN_API_KEY",
            "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
            "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        ];
        for (id, expected_env) in ids.iter().zip(envs.iter()) {
            if env::var(expected_env).is_ok_and(|v| !v.is_empty()) {
                continue;
            }
            assert!(
                get_env_api_key_by_str(id).is_none(),
                "by_str({id}) should resolve to {expected_env} (empty) -> None"
            );
        }
    }
}
