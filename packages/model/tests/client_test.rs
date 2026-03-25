//! Integration tests for Client
//!
//! These tests mirror the patterns from TypeScript's stream.test.ts:
//! - basic_text_generation: Tests complete() with text response validation
//! - handle_tool_call: Tests tool calling events (ToolCallStart, ToolCallDelta, ToolCallEnd)
//! - handle_streaming: Validates TextStart, TextDelta, TextEnd event sequence
//! - handle_thinking: Tests ThinkingDelta events
//! - multi_turn: Tests multi-turn conversation with tool results

use model::types::Provider;
use model::{
    ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Client, Context,
    Cost, InputType, Message, Model, SimpleStreamOptions, StopReason, StreamOptions, Tool,
    ToolCall, ToolResultContent, ToolResultMessage, Usage, UsageCost, UserMessage,
};
use std::collections::HashMap;

/// Create the calculator tool definition
fn calculator_tool() -> Tool {
    let mut parameters = HashMap::new();
    parameters.insert("type".to_string(), serde_json::json!("object"));

    let mut properties = HashMap::new();
    properties.insert(
        "a".to_string(),
        serde_json::json!({
            "type": "number",
            "description": "First number"
        }),
    );
    properties.insert(
        "b".to_string(),
        serde_json::json!({
            "type": "number",
            "description": "Second number"
        }),
    );
    properties.insert(
        "operation".to_string(),
        serde_json::json!({
            "type": "string",
            "enum": ["add", "subtract", "multiply", "divide"],
            "description": "The operation to perform"
        }),
    );

    parameters.insert("properties".to_string(), serde_json::json!(properties));
    parameters.insert(
        "required".to_string(),
        serde_json::json!(["a", "b", "operation"]),
    );

    Tool {
        name: "calculator".to_string(),
        description: "Perform basic arithmetic operations".to_string(),
        parameters: serde_json::json!(parameters),
    }
}

/// Mock provider that simulates text responses
struct MockTextProvider {
    response_text: String,
}

impl ApiProvider for MockTextProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        let response = self.response_text.clone();
        Box::pin(async_stream::stream! {
            yield AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: response.clone(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&response))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: response.clone(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&response))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&response))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage {
                        input: 10,
                        output: response.len() as u64,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 10 + response.len() as u64,
                        cost: UsageCost::default(),
                    },
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Mock provider that simulates tool calling
struct MockToolProvider;

impl ApiProvider for MockToolProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        let tool_id = "call_12345".to_string();
        let tool_name = "calculator".to_string();
        let args = r#"{"a": 15, "b": 27, "operation": "add"}"#;
        let tool_call = ToolCall::new(
            tool_id.clone(),
            tool_name.clone(),
            serde_json::json!({"a": 15, "b": 27, "operation": "add"}),
        );

        Box::pin(async_stream::stream! {
            // Tool call start
            yield AssistantMessageEvent::ToolCallStart {
                content_index: 0,
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                        tool_id.clone(),
                        tool_name.clone(),
                        serde_json::json!({}),
                    ))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                },
            };

            // Tool call delta (streaming arguments)
            yield AssistantMessageEvent::ToolCallDelta {
                content_index: 0,
                delta: args.to_string(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                        tool_id.clone(),
                        tool_name.clone(),
                        serde_json::json!({"a": 15, "b": 27, "operation": "add"}),
                    ))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                },
            };

            // Tool call end
            yield AssistantMessageEvent::ToolCallEnd {
                content_index: 0,
                tool_call: tool_call.clone(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::ToolCall(tool_call.clone())],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                },
            };

            // Done with tool use
            yield AssistantMessageEvent::Done {
                reason: StopReason::ToolUse,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::ToolCall(tool_call)],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage {
                        input: 20,
                        output: 50,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 70,
                        cost: UsageCost::default(),
                    },
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                },
            };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Mock provider that simulates streaming text with chunks
struct MockStreamingProvider;

impl ApiProvider for MockStreamingProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        let chunks = vec!["1", ", ", "2", ", ", "3"];

        Box::pin(async_stream::stream! {
            yield AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            let mut full_text = String::new();
            for chunk in chunks {
                full_text.push_str(chunk);
                yield AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: chunk.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&full_text))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };
            }

            yield AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: full_text.clone(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&full_text))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new(&full_text))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage {
                        input: 5,
                        output: full_text.len() as u64,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 5 + full_text.len() as u64,
                        cost: UsageCost::default(),
                    },
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Mock provider that simulates thinking/reasoning
struct MockThinkingProvider;

impl ApiProvider for MockThinkingProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        Box::pin(async_stream::stream! {
            // Thinking start
            yield AssistantMessageEvent::ThinkingStart {
                content_index: 1,
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![] ,
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            // Thinking deltas
            let thinking_chunks = vec!["Let me", " think", " about", " this..."];
            let mut full_thinking = String::new();

            for chunk in thinking_chunks {
                full_thinking.push_str(chunk);
                yield AssistantMessageEvent::ThinkingDelta {
                    content_index: 1,
                    delta: chunk.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![AssistantContentBlock::Thinking(model::ThinkingContent::new(&full_thinking))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };
            }

            // Thinking end
            yield AssistantMessageEvent::ThinkingEnd {
                content_index: 1,
                content: full_thinking.clone(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![AssistantContentBlock::Thinking(model::ThinkingContent::new(&full_thinking))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            // Text response
            yield AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "The answer is 42".to_string(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new("The answer is 42"))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::TextEnd {
                content_index: 0,
                content: "The answer is 42".to_string(),
                partial: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![model::AssistantContentBlock::Text(model::TextContent::new("The answer is 42"))],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };

            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![
                        model::AssistantContentBlock::Text(model::TextContent::new("The answer is 42")),
                        AssistantContentBlock::Thinking(model::ThinkingContent::new("Let me think about this...")),
                    ],
                    api: model::Api::OpenAICompletions,
                    provider: Provider::OpenAI,
                    model: "test".to_string(),
                    usage: Usage {
                        input: 15,
                        output: 20,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 35,
                        cost: UsageCost::default(),
                    },
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                },
            };
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Mock provider for multi-turn conversation testing
struct MockMultiTurnProvider {
    turn: std::sync::atomic::AtomicUsize,
}

impl MockMultiTurnProvider {
    fn new() -> Self {
        Self {
            turn: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl ApiProvider for MockMultiTurnProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        let turn = self.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        Box::pin(async_stream::stream! {
            if turn == 0 {
                // First turn: return tool calls for 42 * 17 and 453 + 434
                let tool_id1 = "call_1".to_string();
                let tool_id2 = "call_2".to_string();

                let tc1 = ToolCall::new(
                    tool_id1.clone(),
                    "calculator",
                    serde_json::json!({"a": 42, "b": 17, "operation": "multiply"}),
                );
                let tc2 = ToolCall::new(
                    tool_id2.clone(),
                    "calculator",
                    serde_json::json!({"a": 453, "b": 434, "operation": "add"}),
                );

                // First tool call start
                yield AssistantMessageEvent::ToolCallStart {
                    content_index: 0,
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                            tool_id1.clone(),
                            "calculator",
                            serde_json::json!({}),
                        ))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                let args1 = r#"{"a": 42, "b": 17, "operation": "multiply"}"#;
                yield AssistantMessageEvent::ToolCallDelta {
                    content_index: 0,
                    delta: args1.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![AssistantContentBlock::ToolCall(tc1.clone())],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                yield AssistantMessageEvent::ToolCallEnd {
                    content_index: 0,
                    tool_call: tc1.clone(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![AssistantContentBlock::ToolCall(tc1.clone())],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                // Second tool call
                yield AssistantMessageEvent::ToolCallStart {
                    content_index: 1,
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![
                            AssistantContentBlock::ToolCall(tc1.clone()),
                            AssistantContentBlock::ToolCall(ToolCall::new(
                                tool_id2.clone(),
                                "calculator",
                                serde_json::json!({}),
                            )),
                        ],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                let args2 = r#"{"a": 453, "b": 434, "operation": "add"}"#;
                yield AssistantMessageEvent::ToolCallDelta {
                    content_index: 1,
                    delta: args2.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![
                            AssistantContentBlock::ToolCall(tc1.clone()),
                            AssistantContentBlock::ToolCall(tc2.clone()),
                        ],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                yield AssistantMessageEvent::ToolCallEnd {
                    content_index: 1,
                    tool_call: tc2.clone(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![
                            AssistantContentBlock::ToolCall(tc1.clone()),
                            AssistantContentBlock::ToolCall(tc2.clone()),
                        ],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                yield AssistantMessageEvent::Done {
                    reason: StopReason::ToolUse,
                    message: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![
                            AssistantContentBlock::ToolCall(tc1),
                            AssistantContentBlock::ToolCall(tc2),
                        ],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage {
                            input: 30,
                            output: 60,
                            cache_read: 0,
                            cache_write: 0,
                            total_tokens: 90,
                            cost: UsageCost::default(),
                        },
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    },
                };
            } else {
                // Second turn: final answer with 714 and 887
                yield AssistantMessageEvent::TextStart {
                    content_index: 0,
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                let response = "The results are: 42 * 17 = 714 and 453 + 434 = 887.";
                yield AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: response.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![model::AssistantContentBlock::Text(model::TextContent::new(response))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                yield AssistantMessageEvent::TextEnd {
                    content_index: 0,
                    content: response.to_string(),
                    partial: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![model::AssistantContentBlock::Text(model::TextContent::new(response))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };

                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: AssistantMessage {
                        role: "assistant".to_string(),
                        content: vec![model::AssistantContentBlock::Text(model::TextContent::new(response))],
                        api: model::Api::OpenAICompletions,
                        provider: Provider::OpenAI,
                        model: "test".to_string(),
                        usage: Usage {
                            input: 40,
                            output: response.len() as u64,
                            cache_read: 0,
                            cache_write: 0,
                            total_tokens: 40 + response.len() as u64,
                            cost: UsageCost::default(),
                        },
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    },
                };
            }
        })
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> model::AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

/// Helper to create a test model
fn create_test_model(api: model::Api) -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        provider: Provider::OpenAI,
        api,
        base_url: "https://test.example.com".to_string(),
        reasoning: true,
        input: vec![InputType::Text],
        context_window: 8192,
        max_tokens: 4096,
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        headers: None,
        compat: None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_basic_text_generation() {
    let client = Client::new();

    // Register mock provider
    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockTextProvider {
            response_text: "Hello test successful".to_string(),
        }),
        Some("test".to_string()),
    );

    let test_model = create_test_model(model::Api::OpenAICompletions);

    // First request
    let context = Context {
        system_prompt: Some("You are a helpful assistant. Be concise.".to_string()),
        messages: vec![Message::User(UserMessage::new_text(
            "Reply with exactly: 'Hello test successful'",
        ))],
        tools: None,
    };

    let message = client
        .complete_simple(&test_model, context.clone(), None)
        .await
        .expect("Complete should succeed");

    assert_eq!(message.role, "assistant");
    assert!(!message.content.is_empty());
    assert!(message.usage.input > 0);
    assert!(message.usage.output > 0);
    assert_eq!(message.stop_reason, StopReason::Stop);

    let text_content: String = message
        .content
        .iter()
        .filter_map(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert!(text_content.contains("Hello test successful"));

    // Second request (multi-turn)
    let mut context2 = context;
    context2.messages.push(Message::Assistant(message));
    context2.messages.push(Message::User(UserMessage::new_text(
        "Now say 'Goodbye test successful'",
    )));

    // Re-register for second call
    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockTextProvider {
            response_text: "Goodbye test successful".to_string(),
        }),
        Some("test".to_string()),
    );

    let message2 = client
        .complete_simple(&test_model, context2, None)
        .await
        .expect("Second complete should succeed");

    let text_content2: String = message2
        .content
        .iter()
        .filter_map(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert!(text_content2.contains("Goodbye test successful"));
}

#[tokio::test]
async fn test_handle_tool_call() {
    let client = Client::new();

    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockToolProvider),
        Some("test".to_string()),
    );

    let test_model = create_test_model(model::Api::OpenAICompletions);

    let context = Context {
        system_prompt: Some("You are a helpful assistant that uses tools when asked.".to_string()),
        messages: vec![Message::User(UserMessage::new_text(
            "Calculate 15 + 27 using the calculator tool.",
        ))],
        tools: Some(vec![calculator_tool()]),
    };

    let mut stream = client
        .stream_simple(&test_model, context, None)
        .expect("Stream should start");

    use futures::StreamExt;

    let mut has_tool_start = false;
    let mut has_tool_delta = false;
    let mut has_tool_end = false;
    let mut accumulated_args = String::new();
    let mut content_index: u32 = 0;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::ToolCallStart {
                content_index: idx,
                partial,
            } => {
                has_tool_start = true;
                content_index = idx;
                if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get(idx as usize)
                {
                    assert_eq!(tc.name, "calculator");
                    assert!(!tc.id.is_empty());
                }
            }
            AssistantMessageEvent::ToolCallDelta {
                content_index: idx,
                delta,
                partial,
            } => {
                has_tool_delta = true;
                assert_eq!(idx, content_index);
                accumulated_args.push_str(&delta);
                if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get(idx as usize)
                {
                    assert_eq!(tc.name, "calculator");
                    // Arguments should be partially populated
                    assert!(!tc.arguments.is_null());
                }
            }
            AssistantMessageEvent::ToolCallEnd {
                content_index: idx,
                tool_call,
                ..
            } => {
                has_tool_end = true;
                assert_eq!(idx, content_index);
                assert_eq!(tool_call.name, "calculator");
                // Verify accumulated args can be parsed
                let parsed: serde_json::Value =
                    serde_json::from_str(&accumulated_args).expect("Args should be valid JSON");
                assert_eq!(parsed["a"], 15);
                assert_eq!(parsed["b"], 27);
                assert_eq!(parsed["operation"], "add");
            }
            AssistantMessageEvent::Done { message, .. } => {
                assert_eq!(message.stop_reason, StopReason::ToolUse);
                let has_tool_call = message
                    .content
                    .iter()
                    .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)));
                assert!(has_tool_call, "Response should contain a tool call");
            }
            _ => {}
        }
    }

    assert!(has_tool_start, "Should receive ToolCallStart event");
    assert!(has_tool_delta, "Should receive ToolCallDelta event");
    assert!(has_tool_end, "Should receive ToolCallEnd event");
}

#[tokio::test]
async fn test_handle_streaming() {
    let client = Client::new();

    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockStreamingProvider),
        Some("test".to_string()),
    );

    let test_model = create_test_model(model::Api::OpenAICompletions);

    let context = Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage::new_text("Count from 1 to 3"))],
        tools: None,
    };

    let mut stream = client
        .stream_simple(&test_model, context, None)
        .expect("Stream should start");

    use futures::StreamExt;

    let mut text_started = false;
    let mut text_chunks = String::new();
    let mut text_completed = false;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextStart { .. } => {
                text_started = true;
            }
            AssistantMessageEvent::TextDelta { delta, .. } => {
                text_chunks.push_str(&delta);
            }
            AssistantMessageEvent::TextEnd { .. } => {
                text_completed = true;
            }
            AssistantMessageEvent::Done { message, .. } => {
                let has_text = message
                    .content
                    .iter()
                    .any(|b| matches!(b, AssistantContentBlock::Text(_)));
                assert!(has_text, "Response should contain text");
            }
            _ => {}
        }
    }

    assert!(text_started, "Should receive TextStart event");
    assert!(!text_chunks.is_empty(), "Should receive text chunks");
    assert_eq!(text_chunks, "1, 2, 3");
    assert!(text_completed, "Should receive TextEnd event");
}

#[tokio::test]
async fn test_handle_thinking() {
    let client = Client::new();

    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockThinkingProvider),
        Some("test".to_string()),
    );

    let test_model = create_test_model(model::Api::OpenAICompletions);

    let context = Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage::new_text(
            "Think long and hard about the answer.",
        ))],
        tools: None,
    };

    let mut stream = client
        .stream_simple(&test_model, context, None)
        .expect("Stream should start");

    use futures::StreamExt;

    let mut thinking_started = false;
    let mut thinking_chunks = String::new();
    let mut thinking_completed = false;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::ThinkingStart { .. } => {
                thinking_started = true;
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                thinking_chunks.push_str(&delta);
            }
            AssistantMessageEvent::ThinkingEnd { .. } => {
                thinking_completed = true;
            }
            AssistantMessageEvent::Done { message, .. } => {
                assert_eq!(message.stop_reason, StopReason::Stop);
                let has_thinking = message
                    .content
                    .iter()
                    .any(|b| matches!(b, AssistantContentBlock::Thinking(_)));
                assert!(has_thinking, "Response should contain thinking content");
            }
            _ => {}
        }
    }

    assert!(thinking_started, "Should receive ThinkingStart event");
    assert!(
        !thinking_chunks.is_empty(),
        "Should receive thinking chunks"
    );
    assert_eq!(thinking_chunks, "Let me think about this...");
    assert!(thinking_completed, "Should receive ThinkingEnd event");
}

#[tokio::test]
async fn test_multi_turn() {
    let client = Client::new();

    client.registry.register(
        model::Api::OpenAICompletions,
        Box::new(MockMultiTurnProvider::new()),
        Some("test".to_string()),
    );

    let test_model = create_test_model(model::Api::OpenAICompletions);

    let mut context = Context {
        system_prompt: Some(
            "You are a helpful assistant that can use tools to answer questions.".to_string(),
        ),
        messages: vec![Message::User(UserMessage::new_text(
            "Think about this briefly, then calculate 42 * 17 and 453 + 434 using the calculator tool.",
        ))],
        tools: Some(vec![calculator_tool()]),
    };

    let mut all_text_content = String::new();
    let mut has_seen_tool_calls = false;
    let max_turns = 5;

    for turn in 0..max_turns {
        let message = client
            .complete_simple(&test_model, context.clone(), None)
            .await
            .expect(&format!("Complete should succeed on turn {}", turn));

        // Add assistant response to context
        context.messages.push(Message::Assistant(message.clone()));

        // Process content blocks
        let mut tool_results: Vec<ToolResultMessage> = vec![];

        for block in &message.content {
            match block {
                AssistantContentBlock::Text(t) => {
                    all_text_content.push_str(&t.text);
                }
                AssistantContentBlock::Thinking(_) => {
                    // Thinking content observed
                }
                AssistantContentBlock::ToolCall(tc) => {
                    has_seen_tool_calls = true;
                    assert_eq!(tc.name, "calculator");
                    assert!(!tc.id.is_empty());

                    let args = &tc.arguments;
                    let a = args["a"].as_f64().unwrap_or(0.0) as i64;
                    let b = args["b"].as_f64().unwrap_or(0.0) as i64;
                    let operation = args["operation"].as_str().unwrap_or("");

                    let result = match operation {
                        "multiply" => a * b,
                        "add" => a + b,
                        _ => 0,
                    };

                    tool_results.push(ToolResultMessage {
                        role: "tool".to_string(),
                        tool_call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        content: vec![ToolResultContent::Text(model::TextContent::new(
                            result.to_string(),
                        ))],
                        details: None,
                        is_error: false,
                        timestamp: 0,
                    });
                }
            }
        }

        // Add tool results to context
        for result in tool_results {
            context.messages.push(Message::ToolResult(result));
        }

        assert_ne!(message.stop_reason, StopReason::Error, "Should not error");

        if message.stop_reason == StopReason::Stop {
            break;
        }
    }

    assert!(has_seen_tool_calls, "Should have seen tool calls");
    assert!(!all_text_content.is_empty(), "Should have text content");
    assert!(
        all_text_content.contains("714"),
        "Should mention 714 (42*17)"
    );
    assert!(
        all_text_content.contains("887"),
        "Should mention 887 (453+434)"
    );
}

#[tokio::test]
async fn test_client_provider_not_found() {
    let client = Client::new();

    // Clear all providers to test "not found" behavior
    client.registry.clear();

    let test_model = create_test_model(model::Api::BedrockConverseStream);

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    // Should return error because provider was unregistered
    let result = client.stream_simple(&test_model, context, None);
    assert!(result.is_err());

    match result {
        Err(model::ClientError::ProviderNotFound { api, .. }) => {
            assert_eq!(api, model::Api::BedrockConverseStream);
        }
        _ => panic!("Expected ProviderNotFound error"),
    }
}

#[tokio::test]
async fn test_complete_simple_returns_error_without_api_key() {
    let client = Client::new();

    // Clear providers to test "not found" behavior
    client.registry.clear();

    let unknown_model = Model {
        api: model::Api::BedrockConverseStream,
        ..create_test_model(model::Api::BedrockConverseStream)
    };

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new_text("Hello"))],
        tools: None,
    };

    let result = client.complete_simple(&unknown_model, context, None).await;
    assert!(result.is_err(), "Should error when provider not found");
}

#[test]
fn test_client_registers_anthropic_provider_by_default() {
    let client = Client::new();

    assert!(client.registry.has(&model::Api::AnthropicMessages));
    assert!(client.registry.has(&model::Api::OpenAICompletions));
}
