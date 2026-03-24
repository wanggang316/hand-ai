//! Core agent loop implementation.
//!
//! Transforms AgentMessage[] to Message[] only at the LLM call boundary.
//! Handles tool execution, steering messages, and follow-up messages.

use crate::error::AgentError;
use crate::types::{
    AfterToolCallContext, AgentContext, AgentEvent, AgentLoopConfig, AgentTool,
    BeforeToolCallContext, ToolExecutionMode, ToolResult, extract_tool_calls,
};
use futures::StreamExt;
use model::{
    AssistantMessage, AssistantMessageEvent, Context, Message, StopReason, ToolCall,
    ToolResultMessage,
};

/// Sink for agent events (callback pattern).
pub type AgentEventSink = Box<dyn Fn(AgentEvent) + Send + Sync>;

/// Result of running the agent loop.
#[derive(Debug, Clone)]
pub struct AgentLoopResult {
    /// All new messages produced during this loop run.
    pub messages: Vec<Message>,
}

/// Start an agent loop with new prompt messages.
///
/// The prompts are added to the context and events are emitted for them.
pub async fn run_agent_loop(
    prompts: Vec<Message>,
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
) -> Result<AgentLoopResult, AgentError> {
    let mut new_messages: Vec<Message> = prompts.clone();

    // Add prompts to context
    for prompt in &prompts {
        context.messages.push(prompt.clone());
    }

    emit(AgentEvent::AgentStart);
    emit(AgentEvent::TurnStart);

    // Emit message events for prompts
    for prompt in &prompts {
        emit(AgentEvent::MessageStart {
            message: prompt.clone(),
        });
        emit(AgentEvent::MessageEnd {
            message: prompt.clone(),
        });
    }

    run_loop(context, &mut new_messages, tools, config, client, emit).await?;

    Ok(AgentLoopResult {
        messages: new_messages,
    })
}

/// Continue an agent loop from the current context without adding a new message.
///
/// The last message in context must be a user or tool_result message.
pub async fn run_agent_loop_continue(
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
) -> Result<AgentLoopResult, AgentError> {
    if context.messages.is_empty() {
        return Err(AgentError::InvalidState(
            "no messages in context".to_string(),
        ));
    }

    // Check last message is not assistant
    if let Some(Message::Assistant(_)) = context.messages.last() {
        return Err(AgentError::InvalidState(
            "cannot continue from assistant message".to_string(),
        ));
    }

    let mut new_messages: Vec<Message> = Vec::new();

    emit(AgentEvent::AgentStart);
    emit(AgentEvent::TurnStart);

    run_loop(context, &mut new_messages, tools, config, client, emit).await?;

    Ok(AgentLoopResult {
        messages: new_messages,
    })
}

/// Main loop logic shared by `run_agent_loop` and `run_agent_loop_continue`.
async fn run_loop(
    context: &mut AgentContext,
    new_messages: &mut Vec<Message>,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
) -> Result<(), AgentError> {
    let mut first_turn = true;

    // Check for steering messages at start
    let mut pending_messages = get_steering_messages(config).await;

    // Outer loop: continues when queued follow-up messages arrive after agent would stop
    loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages
        while has_more_tool_calls || !pending_messages.is_empty() {
            if !first_turn {
                emit(AgentEvent::TurnStart);
            } else {
                first_turn = false;
            }

            // Process pending messages (inject before next assistant response)
            if !pending_messages.is_empty() {
                for message in pending_messages.drain(..) {
                    emit(AgentEvent::MessageStart {
                        message: message.clone(),
                    });
                    emit(AgentEvent::MessageEnd {
                        message: message.clone(),
                    });
                    context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream assistant response
            let assistant_msg =
                stream_assistant_response(context, tools, config, client, emit).await?;
            let assistant_message_ref = match &assistant_msg {
                Message::Assistant(a) => a,
                _ => unreachable!("stream_assistant_response always returns Assistant"),
            };

            new_messages.push(assistant_msg.clone());

            // Check for error/abort
            if assistant_message_ref.stop_reason == StopReason::Error
                || assistant_message_ref.stop_reason == StopReason::Aborted
            {
                emit(AgentEvent::TurnEnd {
                    message: assistant_msg,
                    tool_results: vec![],
                });
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                return Ok(());
            }

            // Check for tool calls
            let tool_calls = extract_tool_calls(assistant_message_ref);
            has_more_tool_calls = !tool_calls.is_empty();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();

            if has_more_tool_calls {
                let results = execute_tool_calls(
                    context,
                    assistant_message_ref,
                    &tool_calls,
                    tools,
                    config,
                    emit,
                )
                .await;

                for result in &results {
                    let result_msg = Message::ToolResult(result.clone());
                    context.messages.push(result_msg.clone());
                    new_messages.push(result_msg);
                }
                tool_results = results;
            }

            emit(AgentEvent::TurnEnd {
                message: assistant_msg,
                tool_results,
            });

            pending_messages = get_steering_messages(config).await;
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up = get_follow_up_messages(config).await;
        if !follow_up.is_empty() {
            pending_messages = follow_up;
            continue;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    });
    Ok(())
}

/// Stream an assistant response from the LLM.
async fn stream_assistant_response(
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
) -> Result<Message, AgentError> {
    // Convert messages for LLM
    let llm_messages = if let Some(convert) = &config.convert_to_llm {
        convert(&context.messages).await
    } else {
        // Default: pass through all standard messages
        default_convert_to_llm(&context.messages)
    };

    // Build LLM context
    let model_tools: Vec<model::Tool> = tools.iter().map(|t| t.to_model_tool()).collect();

    let llm_context = Context {
        system_prompt: if context.system_prompt.is_empty() {
            None
        } else {
            Some(context.system_prompt.clone())
        },
        messages: llm_messages,
        tools: if model_tools.is_empty() {
            None
        } else {
            Some(model_tools)
        },
    };

    // Stream from client
    let mut stream = client
        .stream_simple(
            &config.model,
            llm_context,
            Some(config.stream_options.clone()),
        )
        .map_err(AgentError::Client)?;

    let mut final_message: Option<AssistantMessage> = None;
    let mut emitted_start = false;

    while let Some(event) = stream.next().await {
        match &event {
            AssistantMessageEvent::Start { partial } => {
                let msg = Message::Assistant(partial.clone());
                context.messages.push(msg.clone());
                emitted_start = true;
                emit(AgentEvent::MessageStart { message: msg });
            }

            AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. } => {
                if emitted_start {
                    let msg = Message::Assistant(partial.clone());
                    // Update the last message in context
                    if let Some(last) = context.messages.last_mut() {
                        *last = msg.clone();
                    }
                    emit(AgentEvent::MessageUpdate {
                        message: msg,
                        assistant_message_event: Box::new(event),
                    });
                }
            }

            AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                if emitted_start {
                    let msg = Message::Assistant(partial.clone());
                    if let Some(last) = context.messages.last_mut() {
                        *last = msg.clone();
                    }
                    emit(AgentEvent::MessageUpdate {
                        message: msg,
                        assistant_message_event: Box::new(event),
                    });
                }
            }

            AssistantMessageEvent::Done { message, .. } => {
                let msg = Message::Assistant(message.clone());
                if emitted_start {
                    if let Some(last) = context.messages.last_mut() {
                        *last = msg.clone();
                    }
                } else {
                    context.messages.push(msg.clone());
                    emit(AgentEvent::MessageStart {
                        message: msg.clone(),
                    });
                }
                emit(AgentEvent::MessageEnd { message: msg });
                final_message = Some(message.clone());
            }

            AssistantMessageEvent::Error { error, .. } => {
                let msg = Message::Assistant(error.clone());
                if emitted_start {
                    if let Some(last) = context.messages.last_mut() {
                        *last = msg.clone();
                    }
                } else {
                    context.messages.push(msg.clone());
                    emit(AgentEvent::MessageStart {
                        message: msg.clone(),
                    });
                }
                emit(AgentEvent::MessageEnd { message: msg });
                final_message = Some(error.clone());
            }
        }
    }

    match final_message {
        Some(msg) => Ok(Message::Assistant(msg)),
        None => Err(AgentError::LoopEndedWithoutResult),
    }
}

/// Default message conversion: pass through user/assistant/tool_result.
fn default_convert_to_llm(messages: &[Message]) -> Vec<Message> {
    messages.to_vec()
}

/// Execute tool calls from an assistant message.
async fn execute_tool_calls(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[&ToolCall],
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
) -> Vec<ToolResultMessage> {
    match config.tool_execution {
        ToolExecutionMode::Sequential => {
            execute_tool_calls_sequential(
                context,
                assistant_message,
                tool_calls,
                tools,
                config,
                emit,
            )
            .await
        }
        ToolExecutionMode::Parallel => {
            execute_tool_calls_parallel(context, assistant_message, tool_calls, tools, config, emit)
                .await
        }
    }
}

/// Execute tool calls sequentially.
async fn execute_tool_calls_sequential(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[&ToolCall],
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
) -> Vec<ToolResultMessage> {
    let mut results = Vec::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, tools, config).await;
        match preparation {
            ToolCallPreparation::Immediate { result, is_error } => {
                results.push(emit_tool_call_outcome(tool_call, result, is_error, emit));
            }
            ToolCallPreparation::Prepared { tool, args, .. } => {
                let executed = execute_prepared_tool_call(tool_call, tool, &args).await;
                let finalized = finalize_executed_tool_call(
                    context,
                    assistant_message,
                    tool_call,
                    &args,
                    executed,
                    config,
                )
                .await;
                results.push(emit_tool_call_outcome(
                    tool_call,
                    finalized.0,
                    finalized.1,
                    emit,
                ));
            }
        }
    }

    results
}

/// Execute tool calls in parallel.
async fn execute_tool_calls_parallel(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[&ToolCall],
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
) -> Vec<ToolResultMessage> {
    let mut results = Vec::new();

    struct PreparedCall<'a> {
        tool_call: &'a ToolCall,
        tool: &'a AgentTool,
        args: serde_json::Value,
    }

    let mut runnable_calls: Vec<PreparedCall<'_>> = Vec::new();

    // Prepare sequentially
    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, tools, config).await;
        match preparation {
            ToolCallPreparation::Immediate { result, is_error } => {
                results.push(emit_tool_call_outcome(tool_call, result, is_error, emit));
            }
            ToolCallPreparation::Prepared { tool, args, .. } => {
                runnable_calls.push(PreparedCall {
                    tool_call,
                    tool,
                    args,
                });
            }
        }
    }

    // Await in source order (matching TS parallel behavior where results
    // are collected in source order after concurrent preparation)
    for call in &runnable_calls {
        let executed = execute_prepared_tool_call(call.tool_call, call.tool, &call.args).await;
        let finalized = finalize_executed_tool_call(
            context,
            assistant_message,
            call.tool_call,
            &call.args,
            executed,
            config,
        )
        .await;
        results.push(emit_tool_call_outcome(
            call.tool_call,
            finalized.0,
            finalized.1,
            emit,
        ));
    }

    results
}

enum ToolCallPreparation<'a> {
    Immediate {
        result: ToolResult,
        is_error: bool,
    },
    Prepared {
        tool: &'a AgentTool,
        args: serde_json::Value,
    },
}

async fn prepare_tool_call<'a>(
    _context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    tools: &'a [AgentTool],
    config: &AgentLoopConfig,
) -> ToolCallPreparation<'a> {
    // Find tool
    let tool = match tools.iter().find(|t| t.name == tool_call.name) {
        Some(t) => t,
        None => {
            return ToolCallPreparation::Immediate {
                result: ToolResult::error(format!("Tool {} not found", tool_call.name)),
                is_error: true,
            };
        }
    };

    let args = tool_call.arguments.clone();

    // Run before_tool_call hook
    if let Some(hook) = &config.before_tool_call {
        let ctx = BeforeToolCallContext {
            assistant_message,
            tool_call,
            args: &args,
        };
        if let Some(before_result) = hook(ctx).await
            && before_result.block {
                let reason = before_result
                    .reason
                    .unwrap_or_else(|| "Tool execution was blocked".to_string());
                return ToolCallPreparation::Immediate {
                    result: ToolResult::error(reason),
                    is_error: true,
                };
            }
    }

    ToolCallPreparation::Prepared { tool, args }
}

async fn execute_prepared_tool_call(
    tool_call: &ToolCall,
    tool: &AgentTool,
    args: &serde_json::Value,
) -> (ToolResult, bool) {
    let result = (tool.execute)(tool_call.id.clone(), args.clone()).await;
    (result, false)
}

async fn finalize_executed_tool_call(
    _context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    args: &serde_json::Value,
    executed: (ToolResult, bool),
    config: &AgentLoopConfig,
) -> (ToolResult, bool) {
    let (mut result, mut is_error) = executed;

    if let Some(hook) = &config.after_tool_call {
        let ctx = AfterToolCallContext {
            assistant_message,
            tool_call,
            args,
            result: &result,
            is_error,
        };
        if let Some(after_result) = hook(ctx).await {
            if let Some(content) = after_result.content {
                result.content = content;
            }
            if let Some(details) = after_result.details {
                result.details = Some(details);
            }
            if let Some(err) = after_result.is_error {
                is_error = err;
            }
        }
    }

    (result, is_error)
}

fn emit_tool_call_outcome(
    tool_call: &ToolCall,
    result: ToolResult,
    is_error: bool,
    emit: &AgentEventSink,
) -> ToolResultMessage {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        result: result.clone(),
        is_error,
    });

    let tool_result_message =
        ToolResultMessage::new(tool_call.id.clone(), tool_call.name.clone(), result.content);

    // Set is_error and details
    let mut trm = tool_result_message;
    trm.is_error = is_error;
    trm.details = result.details;

    let msg = Message::ToolResult(trm.clone());
    emit(AgentEvent::MessageStart {
        message: msg.clone(),
    });
    emit(AgentEvent::MessageEnd { message: msg });

    trm
}

/// Get steering messages from config hook.
async fn get_steering_messages(config: &AgentLoopConfig) -> Vec<Message> {
    match &config.get_steering_messages {
        Some(hook) => hook().await,
        None => Vec::new(),
    }
}

/// Get follow-up messages from config hook.
async fn get_follow_up_messages(config: &AgentLoopConfig) -> Vec<Message> {
    match &config.get_follow_up_messages {
        Some(hook) => hook().await,
        None => Vec::new(),
    }
}
