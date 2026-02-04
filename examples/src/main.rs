//! Example: Test Client functionality
//!
//! Run with:
//!   OPENAI_API_KEY=your_key cargo run --example client_test

use model::{Client, Context, Message, SimpleStreamOptions, UserMessage, get_model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create client (automatically registers built-in providers)
    let client = Client::new();

    // Get a model
    let model =
        get_model("openrouter", "openai/gpt-5-mini").ok_or("Model not found: openai/gpt-5-mini")?;

    println!("Testing model: {} ({})", model.name, model.id);
    println!("API: {:?}", model.api);
    println!();

    // Create a simple context
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage::new_text(
            "What is 2 + 2? Answer in one word.",
        ))],
        tools: None,
    };

    // Test stream_simple
    println!("=== Testing stream_simple ===");

    let options = SimpleStreamOptions::default();

    match client.stream_simple(&model, context.clone(), Some(options)) {
        Ok(mut stream) => {
            use futures::StreamExt;

            let mut full_response = String::new();

            while let Some(event) = stream.next().await {
                match event {
                    model::AssistantMessageEvent::TextDelta { delta, .. } => {
                        print!("{delta}");
                        full_response.push_str(&delta);
                    }
                    model::AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        eprint!("[Thinking: {delta}]");
                    }
                    model::AssistantMessageEvent::Done { message, .. } => {
                        println!("\n\n[Done]");
                        println!("Stop reason: {:?}", message.stop_reason);
                        println!(
                            "Usage: input={}, output={}, total={}",
                            message.usage.input, message.usage.output, message.usage.total_tokens
                        );
                    }
                    model::AssistantMessageEvent::Error { error, .. } => {
                        eprintln!("\n[Error] {:?}", error.error_message);
                    }
                    _ => {}
                }
            }

            println!("\nFull response: {full_response}");
        }
        Err(e) => {
            eprintln!("Failed to start stream: {e}");
            return Err(e.into());
        }
    }

    // Test complete_simple
    println!("\n=== Testing complete_simple ===");

    match client.complete_simple(&model, context, None).await {
        Ok(message) => {
            println!("Response: {:?}", message.content);
            println!("Stop reason: {:?}", message.stop_reason);
        }
        Err(e) => {
            eprintln!("Failed to complete: {e}");
        }
    }

    Ok(())
}
