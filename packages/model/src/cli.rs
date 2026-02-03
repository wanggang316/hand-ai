//! CLI tool for the model package.
//!
//! Provides commands to list providers, models, check API keys, and more.

use std::env;

use crate::{
    get_env_api_key_by_str, get_model, get_models, get_provider_keys,
    get_providers, models, Compat,
};

/// Print help message.
fn print_help() {
    println!(
        r#"Usage: cargo run --bin model-cli <command> [args]

Commands:
  list-providers              List all available providers
  list-models [provider]      List models for a provider (or all if not specified)
  check-keys                  Check API key configuration status
  model-info <provider> <id>  Show details for a specific model
  help, --help, -h            Show this help message

Examples:
  cargo run --bin model-cli list-providers
  cargo run --bin model-cli list-models openai
  cargo run --bin model-cli check-keys
  cargo run --bin model-cli model-info openai gpt-4o
"#
    );
}

/// List all available providers.
fn list_providers() {
    println!("Available providers:\n");

    let providers = get_providers();
    let provider_keys = get_provider_keys();

    if providers.is_empty() {
        println!("No providers found.");
        return;
    }

    for provider_key in provider_keys {
        let has_key = get_env_api_key_by_str(&provider_key).is_some();
        let status = if has_key { "✓" } else { "✗" };
        println!("  {} {} {}", status, provider_key.chars().take(25).collect::<String>().pad_to_width(25),
                 if has_key { "(configured)" } else { "" });
    }

    println!("\n✓ = API key configured");
    println!("✗ = API key not configured");
}

/// List models for a specific provider or all providers.
fn list_models(provider: Option<&str>) {
    match provider {
        Some(p) => {
            println!("Models for provider '{}':\n", p);
            let models = get_models(p);
            if models.is_empty() {
                println!("No models found for provider '{}'.", p);
                return;
            }

            for model in models {
                println!("  {} {}",
                         model.id.chars().take(40).collect::<String>().pad_to_width(40),
                         model.name);
                println!("    API: {:?}, Context: {}, Max tokens: {}",
                         model.api, model.context_window, model.max_tokens);
                println!("    Cost: ${:.4}/1M input, ${:.4}/1M output",
                         model.cost.input, model.cost.output);
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
                            println!("{}:", provider_key);
                            let mut model_ids: Vec<_> = provider_models.keys().collect();
                            model_ids.sort();
                            for model_id in model_ids {
                                if let Some(model) = provider_models.get(model_id) {
                                    println!("  {} {}",
                                             model.id.chars().take(40).collect::<String>().pad_to_width(40),
                                             model.name);
                                }
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error loading models: {}", e);
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
                    ("✓", format!("{}...{}", &key[..8], &key[key.len()-4..]))
                } else {
                    ("✓", "Configured".to_string())
                }
            }
            None => ("✗", "Not configured".to_string()),
        };

        println!("  {} {} {}",
                 status,
                 provider_key.chars().take(25).collect::<String>().pad_to_width(25),
                 details);
    }

    println!("\n{}/{} providers configured", configured_count, total_count);
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
                        println!("  OpenAI Completions: {:?}", oai_compat);
                    }
                    Compat::OpenAIResponses(oai_resp_compat) => {
                        println!("  OpenAI Responses: {:?}", oai_resp_compat);
                    }
                }
            }

            if let Some(headers) = &model.headers {
                println!("\nCustom Headers:");
                for (key, value) in headers {
                    println!("  {}: {}", key, value);
                }
            }
        }
        None => {
            eprintln!("Model not found: {} / {}", provider, model_id);
            eprintln!("Use 'list-models {}' to see available models.", provider);
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

/// Main CLI entry point.
pub fn main() {
    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(|s| s.as_str());

    match command {
        Some("list-providers") => list_providers(),
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
        Some("help") | Some("--help") | Some("-h") | None => print_help(),
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!("Use 'help' for usage information.");
            std::process::exit(1);
        }
    }
}
