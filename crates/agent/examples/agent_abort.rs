//! End-to-end smoke test for `Agent::abort_handle()`.
//!
//! Wires up a deterministic mock provider that streams a delayed assistant
//! response, kicks off a `prompt(...)` on a background task, then aborts via
//! the `AbortHandle` while the stream is in flight. The expected event order
//! is logged so the cancellation contract can be eyeballed.
//!
//! Run with:
//!   cargo run -p hand-agent --example agent_abort

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hand_agent::{Agent, AgentEvent};
use model::types::Provider;
use model::{
    Api, ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Client, Context, Cost, InputType, Model, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, Usage,
};

struct SlowProvider {
    delay_ms: u64,
}

impl ApiProvider for SlowProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let delay = self.delay_ms;
        Box::pin(async_stream::stream! {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let msg = AssistantMessage {
                role: "assistant".into(),
                content: vec![AssistantContentBlock::Text(TextContent::new("ok"))],
                api: Api::OpenAICompletions,
                provider: Provider::OpenAI,
                model: "demo".into(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                raw_stop_reason: None,
                error_message: None,
                timestamp: 0,
            };
            yield AssistantMessageEvent::Start { partial: msg.clone() };
            yield AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

fn demo_model() -> Model {
    Model {
        id: "demo".into(),
        name: "Demo".into(),
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        base_url: "https://example.invalid".into(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 8000,
        max_tokens: 1024,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

#[tokio::main]
async fn main() {
    let client = Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(SlowProvider { delay_ms: 500 }),
        Some("demo".into()),
    );

    let mut agent = Agent::new(client, demo_model());

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_for_listener = log.clone();
    let _sub = agent.subscribe(move |event: &AgentEvent, _cancel| {
        let label = match event {
            AgentEvent::AgentStart => "agent_start".to_string(),
            AgentEvent::AgentEnd { .. } => "agent_end".to_string(),
            AgentEvent::TurnStart => "turn_start".to_string(),
            AgentEvent::TurnEnd { .. } => "turn_end".to_string(),
            AgentEvent::MessageStart { .. } => "message_start".to_string(),
            AgentEvent::MessageEnd { .. } => "message_end".to_string(),
            AgentEvent::MessageUpdate { .. } => "message_update".to_string(),
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start".to_string(),
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update".to_string(),
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end".to_string(),
        };
        log_for_listener.lock().unwrap().push(label);
    });

    let abort = agent.abort_handle();

    // Schedule the abort 50ms in — well before the 500ms stream completes.
    let abort_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        abort.abort();
    });

    let outcome = agent.prompt("hello").await;
    abort_task.await.unwrap();

    let last_msg = agent.messages().last().cloned();
    println!("prompt outcome: {:?}", outcome.map(|r| r.messages.len()));
    println!(
        "last message: {:?}",
        last_msg.map(|m| match m {
            model::Message::Assistant(a) => format!("Assistant(stop_reason={:?})", a.stop_reason),
            other => format!("{other:?}"),
        })
    );
    println!("event sequence:");
    for label in log.lock().unwrap().iter() {
        println!("  {label}");
    }
}
