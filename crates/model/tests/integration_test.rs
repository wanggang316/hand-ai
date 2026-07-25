//! Integration tests that call real APIs
//!
//! These tests are only run when the corresponding API key is set.
//! Use `cargo test -p model --test integration_test -- --ignored` to run them.

use model::{
    Client, Context, Message, SimpleStreamOptions, UserMessage, get_env_api_key, get_model,
};
use std::env;

/// Check if we should run integration tests
fn should_run_integration_tests() -> bool {
    env::var("RUN_INTEGRATION_TESTS").is_ok()
}

/// Get Groq API key if available (uses OpenAICompletions API)
fn get_groq_key() -> Option<String> {
    get_env_api_key(&model::types::Provider::Groq)
}

#[tokio::test]
#[ignore = "requires GROQ_API_KEY environment variable"]
async fn test_groq_chat_completion() {
    if !should_run_integration_tests() {
        println!("Skipping integration test. Set RUN_INTEGRATION_TESTS=1 to enable.");
        return;
    }

    let api_key = match get_groq_key() {
        Some(key) => key,
        None => {
            println!("Skipping: GROQ_API_KEY not set");
            return;
        }
    };

    println!("Testing Groq chat completion...");

    // Create client
    let client = Client::new();

    // Get model (Groq uses OpenAICompletions API which is registered by default)
    let model = get_model("groq", "llama-3.1-8b-instant").expect("Model not found: groq/llama-3.1-8b-instant");

    // Create context
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage::new_text(
            "What is 2 + 2? Answer with just the number.",
        ))],
        tools: None,
    };

    // Build options with API key
    let mut options = SimpleStreamOptions::default();
    options.base.api_key = Some(api_key);

    // Test streaming
    let mut stream = client
        .stream_simple(&model, context.clone(), Some(options.clone()))
        .expect("Failed to start stream");

    use futures::StreamExt;

    let mut full_response = String::new();
    let mut got_start = false;
    let mut got_done = false;

    while let Some(event) = stream.next().await {
        match event {
            model::AssistantMessageEvent::Start { .. } => {
                got_start = true;
                println!("[Stream started]");
            }
            model::AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{delta}");
                full_response.push_str(&delta);
            }
            model::AssistantMessageEvent::TextEnd { content, .. } => {
                println!("\n[Text end: {content}]");
            }
            model::AssistantMessageEvent::Done { message, .. } => {
                got_done = true;
                println!("\n[Done]");
                println!("Stop reason: {:?}", message.stop_reason);
                println!(
                    "Usage: input={}, output={}, total={}",
                    message.usage.input, message.usage.output, message.usage.total_tokens
                );
                println!("Cost: ${:.6}", message.usage.cost.total);
            }
            model::AssistantMessageEvent::Error { error, .. } => {
                panic!("Stream error: {:?}", error.error_message);
            }
            _ => {}
        }
    }

    assert!(got_start, "Should receive Start event");
    assert!(got_done, "Should receive Done event");
    // Just check we got some response (Groq might answer differently)
    assert!(
        !full_response.is_empty(),
        "Response should not be empty, got: {}",
        full_response
    );
}

#[tokio::test]
#[ignore = "requires GROQ_API_KEY environment variable"]
async fn test_groq_complete_simple() {
    if !should_run_integration_tests() {
        println!("Skipping integration test. Set RUN_INTEGRATION_TESTS=1 to enable.");
        return;
    }

    let api_key = match get_groq_key() {
        Some(key) => key,
        None => {
            println!("Skipping: GROQ_API_KEY not set");
            return;
        }
    };

    println!("Testing Groq complete_simple...");

    let client = Client::new();

    let model = get_model("groq", "llama-3.1-8b-instant").expect("Model not found: groq/llama-3.1-8b-instant");

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text(
            "Say 'Hello from Groq' and nothing else.",
        ))],
        tools: None,
    };

    let mut options = SimpleStreamOptions::default();
    options.base.api_key = Some(api_key);

    let message = client
        .complete_simple(&model, context, Some(options))
        .await
        .expect("Complete failed");

    println!("Response: {:?}", message.content);
    println!("Stop reason: {:?}", message.stop_reason);
    println!("Usage: {:?}", message.usage);

    assert!(
        message.stop_reason != model::StopReason::Error,
        "Should not error"
    );
}

#[tokio::test]
async fn test_client_stream_returns_error_without_api_key() {
    // This test always runs - it tests that stream returns error event when API key is missing
    println!("Testing stream error without API key...");

    let client = Client::new();

    // Use Groq model (uses OpenAICompletions which is registered by default)
    let model = get_model("groq", "llama-3.1-8b-instant").expect("Model not found");

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    // Stream starts successfully (provider is registered)
    let mut stream = client
        .stream_simple(&model, context, None)
        .expect("Stream should start");

    use futures::StreamExt;

    // But the first event should be an error (no API key)
    let mut got_error = false;
    while let Some(event) = stream.next().await {
        if let model::AssistantMessageEvent::Error { error, .. } = event {
            println!("Got expected error: {:?}", error.error_message);
            got_error = true;
            break;
        }
    }

    assert!(
        got_error,
        "Should receive Error event when API key is missing"
    );
}

#[tokio::test]
async fn test_provider_not_found_error() {
    // This test always runs - no API needed
    let client = Client::new();

    // Clear all providers to test "not found" behavior
    client.registry.clear();

    // Create a model for an API without a registered provider
    let model = model::types::Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        provider: model::types::Provider::AmazonBedrock,
        api: model::types::Api::BedrockConverseStream,
        base_url: "https://test.example.com".to_string(),
        reasoning: false,
        input: vec![model::types::InputType::Text],
        context_window: 8192,
        max_tokens: 4096,
        cost: model::types::Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        headers: None,
        compat: None,
        thinking_level_map: None,
    };

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let result = client.stream_simple(&model, context, None);

    match result {
        Err(model::ClientError::ProviderNotFound { api, .. }) => {
            assert_eq!(api, model::types::Api::BedrockConverseStream);
            println!("Got expected ProviderNotFound error");
        }
        _ => panic!("Expected ProviderNotFound error"),
    }
}
