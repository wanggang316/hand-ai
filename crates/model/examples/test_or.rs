//! Minimal repro for the "Working… 不回复" hang.
//!
//! Runs `stream_simple` directly against openrouter/deepseek-v4-flash and
//! prints the first event (or a timeout) so we can pinpoint where in the
//! stack the request stalls.

use futures::StreamExt;
use model::types::{
    Api, AssistantMessageEvent, Cost, InputType, Message, Provider, UserMessage,
};
use model::{ApiProviderRegistry, Context, Model, stream_simple};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let registry = ApiProviderRegistry::new();
    model::providers::register_builtins(&registry);

    // Allow overriding the model id from the command line for quick
    // comparisons between known-good and suspect models.
    let model_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "deepseek/deepseek-v4-flash".to_string());

    let model = Model {
        id: model_id.clone(),
        name: format!("{model_id} (OpenRouter)"),
        api: Api::OpenAICompletions,
        provider: Provider::Openrouter,
        base_url: "https://openrouter.ai/api/v1".to_string(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        compat: None,
        thinking_level_map: None,
    };

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("say hi briefly"))],
        tools: None,
    };

    println!("calling stream_simple…");
    let mut stream = match stream_simple(&registry, &model, context, None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stream_simple failed: {e}");
            return;
        }
    };

    let started = Instant::now();
    let mut total_events = 0u32;
    let mut got_text = false;

    loop {
        match tokio::time::timeout(Duration::from_secs(45), stream.next()).await {
            Ok(Some(ev)) => {
                total_events += 1;
                let tag = match &ev {
                    AssistantMessageEvent::Start { .. } => "Start",
                    AssistantMessageEvent::TextStart { .. } => "TextStart",
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        got_text = true;
                        println!(
                            "[{:>5}ms] TextDelta {:?}",
                            started.elapsed().as_millis(),
                            delta
                        );
                        continue;
                    }
                    AssistantMessageEvent::TextEnd { .. } => "TextEnd",
                    AssistantMessageEvent::ThinkingStart { .. } => "ThinkingStart",
                    AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        println!(
                            "[{:>5}ms] ThinkingDelta {:?}",
                            started.elapsed().as_millis(),
                            delta.chars().take(60).collect::<String>()
                        );
                        continue;
                    }
                    AssistantMessageEvent::ThinkingEnd { .. } => "ThinkingEnd",
                    AssistantMessageEvent::ToolCallStart { .. } => "ToolCallStart",
                    AssistantMessageEvent::ToolCallDelta { .. } => "ToolCallDelta",
                    AssistantMessageEvent::ToolCallEnd { .. } => "ToolCallEnd",
                    AssistantMessageEvent::Done { message, .. } => {
                        println!(
                            "[{:>5}ms] Done — events={} text_seen={} content_blocks={}",
                            started.elapsed().as_millis(),
                            total_events,
                            got_text,
                            message.content.len()
                        );
                        for (i, block) in message.content.iter().enumerate() {
                            match block {
                                model::types::AssistantContentBlock::Text(t) => {
                                    println!("  [{i}] Text: {:?}", t.text);
                                }
                                model::types::AssistantContentBlock::Thinking(t) => {
                                    println!(
                                        "  [{i}] Thinking: {:?}…",
                                        t.thinking.chars().take(80).collect::<String>()
                                    );
                                }
                                other => println!("  [{i}] {:?}", std::mem::discriminant(other)),
                            }
                        }
                        break;
                    }
                    AssistantMessageEvent::Error { error, .. } => {
                        println!(
                            "[{:>5}ms] Error — {}",
                            started.elapsed().as_millis(),
                            error.error_message.as_deref().unwrap_or("(no message)")
                        );
                        break;
                    }
                };
                println!("[{:>5}ms] {}", started.elapsed().as_millis(), tag);
            }
            Ok(None) => {
                println!(
                    "[{:>5}ms] stream ended without Done/Error",
                    started.elapsed().as_millis()
                );
                break;
            }
            Err(_) => {
                println!(
                    "[{:>5}ms] timeout after 45s — events_so_far={} text_seen={}",
                    started.elapsed().as_millis(),
                    total_events,
                    got_text
                );
                break;
            }
        }
    }
}
