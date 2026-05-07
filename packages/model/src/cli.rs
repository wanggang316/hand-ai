//! CLI tool for the model package.
//!
//! Provides commands to list providers, models, check API keys, manage OAuth
//! credentials, and run streaming chat completions.

use std::collections::HashMap;
use std::env;

use crate::{
    CacheRetention, Client, Compat, Context, Message, OAuthAuthInfo, OAuthLoginCallbacks,
    OAuthProviderId, OAuthRegistry, SimpleStreamOptions, StreamOptions, Transport, UserMessage,
    get_env_api_key_by_str, get_model, get_models, get_provider_keys, get_providers, models,
};

/// Print help message.
fn print_help() {
    println!(
        r#"Usage: cargo run --bin model-cli <command> [args]

Commands:
  list-providers              List all available providers (with OAuth status)
  list-models [provider]      List models for a provider (or all if not specified)
  check-keys                  Check API key configuration status
  model-info <provider> <id>  Show details for a specific model
  chat <provider> <model_id> <prompt> [flags]
                              Send a chat completion request
                              Flags:
                                --transport <sse|websocket|auto>
                                --cache-retention <none|short|long>
  oauth login <provider>      Run interactive OAuth login for a provider
  oauth status                Show authenticated OAuth providers
  oauth logout <provider>     Remove stored OAuth credentials
  help, --help, -h            Show this help message

OAuth providers:
  anthropic, openai-codex, github-copilot

Examples:
  cargo run --bin model-cli list-providers
  cargo run --bin model-cli list-models openai
  cargo run --bin model-cli check-keys
  cargo run --bin model-cli model-info openai gpt-4o
  cargo run --bin model-cli chat openai gpt-4o "Hello, how are you?"
  cargo run --bin model-cli chat openai-codex gpt-5-codex "Hi" --transport websocket
  cargo run --bin model-cli oauth login anthropic
  cargo run --bin model-cli oauth status
  cargo run --bin model-cli oauth logout anthropic
"#
    );
}

/// Build a fresh OAuthRegistry and load the stored credential map.
///
/// Returns the registry alongside the loaded credential map so callers can
/// reuse the registry without paying for a second `load()` round-trip.
async fn load_oauth_state() -> (OAuthRegistry, HashMap<OAuthProviderId, OAuthAuthInfo>) {
    let registry = OAuthRegistry::new();
    let map = match registry.load().await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Warning: failed to load OAuth credential store: {e}");
            HashMap::new()
        }
    };
    (registry, map)
}

/// List all available providers, including OAuth status when applicable.
async fn list_providers() {
    println!("Available providers:\n");

    let providers = get_providers();
    let provider_keys = get_provider_keys();

    if providers.is_empty() {
        println!("No providers found.");
        return;
    }

    let (_registry, oauth_map) = load_oauth_state().await;

    for provider_key in provider_keys {
        let has_key = get_env_api_key_by_str(&provider_key).is_some();
        let status = if has_key { "✓" } else { "✗" };
        let oauth_marker = match oauth_provider_id_for(&provider_key) {
            Some(id) => {
                if let Some(info) = oauth_map.get(&id) {
                    let suffix = expires_in_label(info.credentials.expires_at);
                    format!("[oauth: authenticated{suffix}]")
                } else {
                    "[oauth: not authenticated]".to_string()
                }
            }
            None => String::new(),
        };
        println!(
            "  {} {} {} {}",
            status,
            provider_key
                .chars()
                .take(25)
                .collect::<String>()
                .pad_to_width(25),
            if has_key { "(configured)" } else { "" },
            oauth_marker,
        );
    }

    println!("\n✓ = API key configured");
    println!("✗ = API key not configured");
}

/// Map a provider catalog key to its `OAuthProviderId` if one exists.
///
/// Catalog keys live in `models.json` and use slugs like `anthropic`,
/// `openai-codex`, and `github-copilot` — the same slugs that
/// `OAuthProviderId::as_str()` emits.
fn oauth_provider_id_for(provider_key: &str) -> Option<OAuthProviderId> {
    parse_oauth_provider_id(provider_key)
}

/// Parse a CLI/provider slug into an `OAuthProviderId`.
fn parse_oauth_provider_id(slug: &str) -> Option<OAuthProviderId> {
    match slug {
        "anthropic" => Some(OAuthProviderId::Anthropic),
        "openai-codex" => Some(OAuthProviderId::OpenAICodex),
        "github-copilot" => Some(OAuthProviderId::GithubCopilot),
        _ => None,
    }
}

/// Format an `expires_at` (Unix epoch ms) as a `, expires in 2h` style suffix.
///
/// Returns an empty string when `expires_at` is `None` or already past so we
/// don't lie about how fresh a token is.
fn expires_in_label(expires_at_ms: Option<u64>) -> String {
    let Some(exp) = expires_at_ms else {
        return String::new();
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if exp <= now_ms {
        return ", expired".to_string();
    }
    let remaining_secs = (exp - now_ms) / 1000;
    format!(", expires in {}", humanize_duration_secs(remaining_secs))
}

/// Convert a duration (seconds) into a human label like `2h`, `45m`, `30s`.
fn humanize_duration_secs(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// List models for a specific provider or all providers.
fn list_models(provider: Option<&str>) {
    match provider {
        Some(p) => {
            println!("Models for provider '{p}':\n");
            let models = get_models(p);
            if models.is_empty() {
                println!("No models found for provider '{p}'.");
                return;
            }

            for model in models {
                println!(
                    "  {} {}",
                    model
                        .id
                        .chars()
                        .take(40)
                        .collect::<String>()
                        .pad_to_width(40),
                    model.name
                );
                println!(
                    "    API: {:?}, Context: {}, Max tokens: {}",
                    model.api, model.context_window, model.max_tokens
                );
                println!(
                    "    Cost: ${:.4}/1M input, ${:.4}/1M output",
                    model.cost.input, model.cost.output
                );
                println!();
            }
        }
        None => {
            println!("All models:\n");
            match models() {
                Ok(registry) => {
                    let mut provider_keys: Vec<_> = registry.keys().collect();
                    provider_keys.sort();

                    for provider_key in provider_keys {
                        if let Some(provider_models) = registry.get(provider_key) {
                            println!("{provider_key}:");
                            let mut model_ids: Vec<_> = provider_models.keys().collect();
                            model_ids.sort();
                            for model_id in model_ids {
                                if let Some(model) = provider_models.get(model_id) {
                                    println!(
                                        "  {} {}",
                                        model
                                            .id
                                            .chars()
                                            .take(40)
                                            .collect::<String>()
                                            .pad_to_width(40),
                                        model.name
                                    );
                                }
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error loading models: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

/// Check API key configuration status for all providers.
fn check_keys() {
    println!("API Key Configuration Status:\n");

    let provider_keys = get_provider_keys();

    if provider_keys.is_empty() {
        println!("No providers found.");
        return;
    }

    let mut configured_count = 0;
    let mut total_count = 0;

    for provider_key in &provider_keys {
        total_count += 1;
        let key_result = get_env_api_key_by_str(provider_key);

        let (status, details) = match key_result {
            Some(key) => {
                configured_count += 1;
                if key == "<authenticated>" {
                    ("✓", "Authenticated via credentials".to_string())
                } else if key.len() > 20 {
                    ("✓", format!("{}...{}", &key[..8], &key[key.len() - 4..]))
                } else {
                    ("✓", "Configured".to_string())
                }
            }
            None => ("✗", "Not configured".to_string()),
        };

        println!(
            "  {} {} {}",
            status,
            provider_key
                .chars()
                .take(25)
                .collect::<String>()
                .pad_to_width(25),
            details
        );
    }

    println!("\n{configured_count}/{total_count} providers configured");
}

/// Show details for a specific model.
fn model_info(provider: &str, model_id: &str) {
    match get_model(provider, model_id) {
        Some(model) => {
            println!("Model Information:\n");
            println!("  ID: {}", model.id);
            println!("  Name: {}", model.name);
            println!("  Provider: {:?}", model.provider);
            println!("  API: {:?}", model.api);
            println!("  Base URL: {}", model.base_url);
            println!("  Reasoning: {}", model.reasoning);
            println!("  Input types: {:?}", model.input);
            println!("  Context window: {}", model.context_window);
            println!("  Max tokens: {}", model.max_tokens);
            println!("\nCost (per million tokens):");
            println!("  Input: ${:.4}", model.cost.input);
            println!("  Output: ${:.4}", model.cost.output);
            println!("  Cache read: ${:.4}", model.cost.cache_read);
            println!("  Cache write: ${:.4}", model.cost.cache_write);

            if let Some(compat) = &model.compat {
                println!("\nCompatibility:");
                match compat {
                    Compat::OpenAICompletions(oai_compat) => {
                        println!("  OpenAI Completions: {oai_compat:?}");
                    }
                    Compat::OpenAIResponses(oai_resp_compat) => {
                        println!("  OpenAI Responses: {oai_resp_compat:?}");
                    }
                    Compat::AnthropicMessages(anth_compat) => {
                        println!("  Anthropic Messages: {anth_compat:?}");
                    }
                }
            }

            if let Some(headers) = &model.headers {
                println!("\nCustom Headers:");
                for (key, value) in headers {
                    println!("  {key}: {value}");
                }
            }
        }
        None => {
            eprintln!("Model not found: {provider} / {model_id}");
            eprintln!("Use 'list-models {provider}' to see available models.");
            std::process::exit(1);
        }
    }
}

/// Trait extension for padding strings.
trait PadToWidth {
    fn pad_to_width(&self, width: usize) -> String;
}

impl PadToWidth for String {
    fn pad_to_width(&self, width: usize) -> String {
        if self.len() >= width {
            self.clone()
        } else {
            format!("{}{}", self, " ".repeat(width - self.len()))
        }
    }
}

/// Parse `--transport` flag values.
fn parse_transport(value: &str) -> Option<Transport> {
    match value {
        "sse" => Some(Transport::Sse),
        "websocket" => Some(Transport::Websocket),
        "auto" => Some(Transport::Auto),
        _ => None,
    }
}

/// Parse `--cache-retention` flag values.
fn parse_cache_retention(value: &str) -> Option<CacheRetention> {
    match value {
        "none" => Some(CacheRetention::None),
        "short" => Some(CacheRetention::Short),
        "long" => Some(CacheRetention::Long),
        _ => None,
    }
}

/// Flags accepted by the `chat` subcommand after the positional arguments.
#[derive(Default)]
struct ChatFlags {
    transport: Option<Transport>,
    cache_retention: Option<CacheRetention>,
}

/// Parse named flags for `chat` from a slice of remaining argv tokens.
///
/// Returns `Err(message)` on unknown flags or missing values so the caller can
/// print a usage hint and exit non-zero.
fn parse_chat_flags(rest: &[String]) -> Result<ChatFlags, String> {
    let mut flags = ChatFlags::default();
    let mut i = 0;
    while i < rest.len() {
        let arg = &rest[i];
        match arg.as_str() {
            "--transport" => {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| "--transport requires a value".to_string())?;
                flags.transport = Some(parse_transport(value).ok_or_else(|| {
                    format!("invalid --transport value '{value}' (expected sse|websocket|auto)")
                })?);
                i += 2;
            }
            "--cache-retention" => {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| "--cache-retention requires a value".to_string())?;
                flags.cache_retention = Some(parse_cache_retention(value).ok_or_else(|| {
                    format!("invalid --cache-retention value '{value}' (expected none|short|long)")
                })?);
                i += 2;
            }
            other => return Err(format!("unknown flag for chat: {other}")),
        }
    }
    Ok(flags)
}

/// Send a chat completion request using the Client.
async fn chat(provider: &str, model_id: &str, prompt: &str, flags: ChatFlags) {
    // Get the model
    let model = match get_model(provider, model_id) {
        Some(m) => m,
        None => {
            eprintln!("Model not found: {provider} / {model_id}");
            std::process::exit(1);
        }
    };

    // Check if API key is configured
    let api_key = get_env_api_key_by_str(provider);
    if api_key.is_none() {
        eprintln!("Error: API key not configured for provider '{provider}'");
        std::process::exit(1);
    }

    // Create client and context
    let client = Client::new();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text(prompt))],
        tools: None,
    };

    // Build options, applying transport / cache_retention from CLI flags.
    let options = SimpleStreamOptions {
        base: StreamOptions {
            transport: flags.transport,
            cache_retention: flags.cache_retention,
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };

    // Send request
    println!("Sending request to {provider} / {model_id}...\n");

    match client.stream_simple(&model, context, Some(options)) {
        Ok(mut stream) => {
            use futures::StreamExt;
            while let Some(event) = stream.next().await {
                match event {
                    crate::AssistantMessageEvent::TextDelta { delta, .. } => {
                        print!("{delta}");
                    }
                    crate::AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        eprint!("[Thinking: {delta}]");
                    }
                    crate::AssistantMessageEvent::ToolCallDelta { .. } => {
                        println!("\n[Tool call received]");
                    }
                    crate::AssistantMessageEvent::Done { message, .. } => {
                        println!("\n\n--- Done ---");
                        println!("Stop reason: {:?}", message.stop_reason);
                        if message.usage.total_tokens > 0 {
                            println!("Total tokens: {}", message.usage.total_tokens);
                        }
                    }
                    crate::AssistantMessageEvent::Error { error, .. } => {
                        eprintln!("\nError: {}", error.error_message.unwrap_or_default());
                        std::process::exit(1);
                    }
                    _ => {}
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to start stream: {e}");
            std::process::exit(1);
        }
    }
}

/// Run the interactive OAuth login flow for a provider.
async fn oauth_login(provider_slug: &str) {
    let Some(id) = parse_oauth_provider_id(provider_slug) else {
        eprintln!(
            "Error: unknown OAuth provider '{provider_slug}' (expected anthropic|openai-codex|github-copilot)"
        );
        std::process::exit(1);
    };

    let registry = OAuthRegistry::new();
    let provider_impl = match registry.get(id) {
        Some(p) => p,
        None => {
            eprintln!("Error: OAuth provider '{provider_slug}' is not registered");
            std::process::exit(1);
        }
    };

    // Use stderr-printing callbacks so the login URL/device-code shows up
    // even if stdout is being piped elsewhere.
    let callbacks = OAuthLoginCallbacks::stderr();

    eprintln!("Starting OAuth login for {provider_slug}...");
    let creds = match provider_impl.login(&callbacks).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("OAuth login failed: {e}");
            std::process::exit(1);
        }
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let info = OAuthAuthInfo {
        provider_id: id,
        credentials: creds,
        created_at_ms: now_ms,
    };
    if let Err(e) = registry.save(&info).await {
        eprintln!("Failed to persist credentials: {e}");
        std::process::exit(1);
    }

    println!("OAuth login succeeded for {provider_slug}.");
    println!(
        "Credentials stored at {}.",
        registry.storage_path().display()
    );
}

/// Print authenticated providers and freshness for each registered provider.
async fn oauth_status() {
    let (registry, map) = load_oauth_state().await;

    println!("OAuth status:\n");
    for id in registry.ids() {
        let slug = id.as_str();
        match map.get(&id) {
            Some(info) => {
                let suffix = expires_in_label(info.credentials.expires_at);
                if suffix.is_empty() {
                    println!("  {slug}: authenticated");
                } else {
                    // Strip the leading ", " so the line reads naturally.
                    let stripped = suffix.trim_start_matches(", ");
                    println!("  {slug}: authenticated, {stripped}");
                }
            }
            None => println!("  {slug}: not authenticated"),
        }
    }

    println!("\nStorage: {}", registry.storage_path().display());
}

/// Remove stored credentials for a provider.
async fn oauth_logout(provider_slug: &str) {
    let Some(id) = parse_oauth_provider_id(provider_slug) else {
        eprintln!(
            "Error: unknown OAuth provider '{provider_slug}' (expected anthropic|openai-codex|github-copilot)"
        );
        std::process::exit(1);
    };

    let registry = OAuthRegistry::new();
    if let Err(e) = registry.remove(id).await {
        eprintln!("Failed to remove credentials: {e}");
        std::process::exit(1);
    }

    println!("Removed OAuth credentials for {provider_slug}.");
}

/// Spawn a Tokio runtime and drive an async future to completion.
fn run_async<F: std::future::Future<Output = ()>>(fut: F) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to start tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(fut);
}

/// Main CLI entry point.
pub fn main() {
    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(|s| s.as_str());

    match command {
        Some("list-providers") => run_async(list_providers()),
        Some("list-models") => {
            let provider = args.get(2).map(|s| s.as_str());
            list_models(provider);
        }
        Some("check-keys") => check_keys(),
        Some("model-info") => {
            if args.len() < 4 {
                eprintln!("Error: model-info requires provider and model_id arguments");
                eprintln!("Usage: cargo run --bin model-cli model-info <provider> <model_id>");
                std::process::exit(1);
            }
            model_info(&args[2], &args[3]);
        }
        Some("chat") => {
            if args.len() < 5 {
                eprintln!("Error: chat requires provider, model_id, and prompt arguments");
                eprintln!(
                    "Usage: cargo run --bin model-cli chat <provider> <model_id> \"<prompt>\" [--transport <sse|websocket|auto>] [--cache-retention <none|short|long>]"
                );
                std::process::exit(1);
            }
            let flags = match parse_chat_flags(&args[5..]) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            run_async(chat(&args[2], &args[3], &args[4], flags));
        }
        Some("oauth") => {
            let sub = args.get(2).map(|s| s.as_str());
            match sub {
                Some("login") => {
                    let Some(provider) = args.get(3) else {
                        eprintln!("Error: oauth login requires a provider argument");
                        eprintln!(
                            "Usage: cargo run --bin model-cli oauth login <anthropic|openai-codex|github-copilot>"
                        );
                        std::process::exit(1);
                    };
                    run_async(oauth_login(provider));
                }
                Some("status") => run_async(oauth_status()),
                Some("logout") => {
                    let Some(provider) = args.get(3) else {
                        eprintln!("Error: oauth logout requires a provider argument");
                        eprintln!(
                            "Usage: cargo run --bin model-cli oauth logout <anthropic|openai-codex|github-copilot>"
                        );
                        std::process::exit(1);
                    };
                    run_async(oauth_logout(provider));
                }
                Some(other) => {
                    eprintln!("Unknown oauth subcommand: {other}");
                    eprintln!("Use 'help' for usage information.");
                    std::process::exit(1);
                }
                None => {
                    eprintln!("Error: oauth requires a subcommand (login|status|logout)");
                    std::process::exit(1);
                }
            }
        }
        Some("help") | Some("--help") | Some("-h") | None => print_help(),
        Some(cmd) => {
            eprintln!("Unknown command: {cmd}");
            eprintln!("Use 'help' for usage information.");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transport_accepts_known_values() {
        assert_eq!(parse_transport("sse"), Some(Transport::Sse));
        assert_eq!(parse_transport("websocket"), Some(Transport::Websocket));
        assert_eq!(parse_transport("auto"), Some(Transport::Auto));
        assert_eq!(parse_transport("websocket-cached"), None);
        assert_eq!(parse_transport(""), None);
    }

    #[test]
    fn parse_cache_retention_accepts_known_values() {
        assert_eq!(parse_cache_retention("none"), Some(CacheRetention::None));
        assert_eq!(parse_cache_retention("short"), Some(CacheRetention::Short));
        assert_eq!(parse_cache_retention("long"), Some(CacheRetention::Long));
        assert_eq!(parse_cache_retention("medium"), None);
    }

    #[test]
    fn parse_oauth_provider_id_recognizes_all_three_slugs() {
        assert_eq!(
            parse_oauth_provider_id("anthropic"),
            Some(OAuthProviderId::Anthropic)
        );
        assert_eq!(
            parse_oauth_provider_id("openai-codex"),
            Some(OAuthProviderId::OpenAICodex)
        );
        assert_eq!(
            parse_oauth_provider_id("github-copilot"),
            Some(OAuthProviderId::GithubCopilot)
        );
        assert_eq!(parse_oauth_provider_id("openai"), None);
    }

    #[test]
    fn parse_chat_flags_extracts_transport_and_cache_retention() {
        let argv = vec![
            "--transport".to_string(),
            "websocket".to_string(),
            "--cache-retention".to_string(),
            "long".to_string(),
        ];
        let flags = parse_chat_flags(&argv).expect("flags parse");
        assert_eq!(flags.transport, Some(Transport::Websocket));
        assert_eq!(flags.cache_retention, Some(CacheRetention::Long));
    }

    #[test]
    fn parse_chat_flags_rejects_unknown_flag() {
        let argv = vec!["--bogus".to_string()];
        assert!(parse_chat_flags(&argv).is_err());
    }

    #[test]
    fn parse_chat_flags_rejects_missing_value() {
        let argv = vec!["--transport".to_string()];
        assert!(parse_chat_flags(&argv).is_err());
    }

    #[test]
    fn parse_chat_flags_rejects_invalid_transport_value() {
        let argv = vec!["--transport".to_string(), "carrier-pigeon".to_string()];
        assert!(parse_chat_flags(&argv).is_err());
    }

    #[test]
    fn humanize_duration_secs_picks_the_largest_unit() {
        assert_eq!(humanize_duration_secs(45), "45s");
        assert_eq!(humanize_duration_secs(120), "2m");
        assert_eq!(humanize_duration_secs(3600), "1h");
        assert_eq!(humanize_duration_secs(7200), "2h");
        assert_eq!(humanize_duration_secs(86_400 * 3), "3d");
    }

    #[test]
    fn expires_in_label_handles_missing_and_past_timestamps() {
        assert_eq!(expires_in_label(None), "");
        assert_eq!(expires_in_label(Some(0)), ", expired");
    }

    #[test]
    fn expires_in_label_includes_remaining_time_for_future_expiry() {
        let two_hours_from_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 2 * 3600 * 1000;
        let label = expires_in_label(Some(two_hours_from_now));
        // Allow slop in case clock advances during the test.
        assert!(
            label == ", expires in 2h" || label == ", expires in 1h",
            "unexpected label: {label}"
        );
    }
}
