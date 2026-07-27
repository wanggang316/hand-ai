//! Core agent loop implementation.
//!
//! Mirrors the behavior contract of `upstream-agent-core/src/agent-loop.ts`:
//!
//! - prompts → assistant turn → tool execution → tool results → next turn …
//! - steering messages drained between turns; follow-up drained at the boundary
//!   where the agent would otherwise stop.
//! - `terminate` early-stop only fires when *every* tool result in the batch
//!   sets `terminate = true`.
//! - `should_stop_after_turn` exits cleanly after `turn_end`.
//! - cancellation is honored at every async boundary via `CancellationToken`.

use crate::error::AgentError;
use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentTool, BeforeToolCallContext, OnUpdate, ShouldStopAfterTurnContext, ToolExecuteCtx,
    ToolExecutionMode, ToolResult, extract_tool_calls,
};
use futures::StreamExt;
use futures::future::join_all;
use model::{
    AssistantMessage, AssistantMessageEvent, Context, Message, StopReason, ToolCall,
    ToolResultMessage,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Sink for agent events. The closure must be cheap and non-blocking.
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Result of running the agent loop.
#[derive(Debug, Clone)]
pub struct AgentLoopResult {
    /// All new messages produced during this loop run.
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Start an agent loop with new prompt messages.
///
/// The prompts are added to the context and `message_*` events are emitted for them.
pub async fn run_agent_loop(
    prompts: Vec<Message>,
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> Result<AgentLoopResult, AgentError> {
    let mut new_messages: Vec<Message> = Vec::with_capacity(prompts.len() + 4);

    emit(AgentEvent::AgentStart);
    emit(AgentEvent::TurnStart);

    for prompt in &prompts {
        context.messages.push(prompt.clone());
        new_messages.push(prompt.clone());
        emit(AgentEvent::MessageStart {
            message: prompt.clone(),
        });
        emit(AgentEvent::MessageEnd {
            message: prompt.clone(),
        });
    }

    run_loop(
        context,
        &mut new_messages,
        tools,
        config,
        client,
        emit,
        cancel,
        /* skip_initial_steering_poll */ false,
        /* first_turn_already_emitted */ true,
    )
    .await?;

    Ok(AgentLoopResult {
        messages: new_messages,
    })
}

/// Continue an agent loop from the current context without adding a new message.
///
/// The last message in context must convert to a user or tool-result message;
/// otherwise the LLM provider will reject the request.
pub async fn run_agent_loop_continue(
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> Result<AgentLoopResult, AgentError> {
    if context.messages.is_empty() {
        return Err(AgentError::InvalidState(
            "no messages in context".to_string(),
        ));
    }

    if let Some(Message::Assistant(_)) = context.messages.last() {
        return Err(AgentError::InvalidState(
            "cannot continue from assistant message".to_string(),
        ));
    }

    let mut new_messages: Vec<Message> = Vec::new();

    emit(AgentEvent::AgentStart);
    emit(AgentEvent::TurnStart);

    run_loop(
        context,
        &mut new_messages,
        tools,
        config,
        client,
        emit,
        cancel,
        false,
        true,
    )
    .await?;

    Ok(AgentLoopResult {
        messages: new_messages,
    })
}

/// Variant of `run_agent_loop` used by `Agent::continue()` when it has already
/// drained steering messages and wants the loop to skip the initial steering poll.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop_with_messages(
    prompts: Vec<Message>,
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
    skip_initial_steering_poll: bool,
) -> Result<AgentLoopResult, AgentError> {
    let mut new_messages: Vec<Message> = Vec::with_capacity(prompts.len() + 4);

    emit(AgentEvent::AgentStart);
    emit(AgentEvent::TurnStart);

    for prompt in &prompts {
        context.messages.push(prompt.clone());
        new_messages.push(prompt.clone());
        emit(AgentEvent::MessageStart {
            message: prompt.clone(),
        });
        emit(AgentEvent::MessageEnd {
            message: prompt.clone(),
        });
    }

    run_loop(
        context,
        &mut new_messages,
        tools,
        config,
        client,
        emit,
        cancel,
        skip_initial_steering_poll,
        true,
    )
    .await?;

    Ok(AgentLoopResult {
        messages: new_messages,
    })
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    context: &mut AgentContext,
    new_messages: &mut Vec<Message>,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
    skip_initial_steering_poll: bool,
    first_turn_already_emitted: bool,
) -> Result<(), AgentError> {
    let mut first_turn = first_turn_already_emitted;

    let mut pending_messages: Vec<Message> = if skip_initial_steering_poll {
        Vec::new()
    } else {
        get_steering_messages(config, cancel).await
    };

    // Outer loop: continues when queued follow-up messages arrive after agent would stop.
    'outer: loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages.
        while has_more_tool_calls || !pending_messages.is_empty() {
            // Note: cancellation is checked inside `stream_assistant_response`,
            // which synthesizes a `stop_reason=Aborted` assistant message and
            // emits MessageStart/MessageEnd for it. The post-turn cancellation
            // check below catches aborts that arrive between turns.
            if first_turn {
                first_turn = false;
            } else {
                emit(AgentEvent::TurnStart);
            }

            // Inject pending (steering or follow-up) messages.
            if !pending_messages.is_empty() {
                for msg in pending_messages.drain(..) {
                    emit(AgentEvent::MessageStart {
                        message: msg.clone(),
                    });
                    emit(AgentEvent::MessageEnd {
                        message: msg.clone(),
                    });
                    context.messages.push(msg.clone());
                    new_messages.push(msg);
                }
            }

            // Stream the assistant response.
            let assistant_msg =
                stream_assistant_response(context, tools, config, client, emit, cancel).await?;
            let assistant_ref = match &assistant_msg {
                Message::Assistant(a) => a,
                _ => unreachable!("stream_assistant_response always returns Assistant"),
            };

            new_messages.push(assistant_msg.clone());

            if matches!(
                assistant_ref.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                emit(AgentEvent::TurnEnd {
                    message: assistant_msg.clone(),
                    tool_results: vec![],
                });
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                return Ok(());
            }

            let tool_calls = extract_tool_calls(assistant_ref);
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();

            if !tool_calls.is_empty() {
                // A `Length` stop means the output was cut off by the token
                // limit, so every tool call in the message may carry truncated
                // arguments. Fail them all instead of executing potentially
                // incomplete calls.
                let batch = if assistant_ref.stop_reason == StopReason::Length {
                    fail_truncated_tool_calls(&tool_calls, emit)
                } else {
                    execute_tool_calls(
                        context,
                        assistant_ref,
                        &tool_calls,
                        tools,
                        config,
                        emit,
                        cancel,
                    )
                    .await
                };

                for result in &batch.messages {
                    let result_msg = Message::ToolResult(result.clone());
                    context.messages.push(result_msg.clone());
                    new_messages.push(result_msg);
                }
                tool_results = batch.messages;
                has_more_tool_calls = !batch.terminate;
            } else {
                has_more_tool_calls = false;
            }

            emit(AgentEvent::TurnEnd {
                message: assistant_msg.clone(),
                tool_results: tool_results.clone(),
            });

            // shouldStopAfterTurn hook → exit cleanly
            if let Some(hook) = &config.should_stop_after_turn {
                let stop_ctx = ShouldStopAfterTurnContext {
                    message: assistant_ref,
                    tool_results: &tool_results,
                    context,
                    new_messages,
                };
                let should_stop = hook(stop_ctx, cancel.clone()).await;
                if should_stop {
                    emit(AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    });
                    return Ok(());
                }
            }

            if cancel.is_cancelled() {
                return finish_with_cancel(emit, new_messages.clone());
            }

            pending_messages = get_steering_messages(config, cancel).await;
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up = get_follow_up_messages(config, cancel).await;
        if !follow_up.is_empty() {
            pending_messages = follow_up;
            continue 'outer;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    });
    Ok(())
}

/// Cancellation is treated as a clean exit at the loop level: we already
/// synthesized an `Assistant{stop_reason: Aborted}` message inside
/// `stream_assistant_response`, so callers can detect aborts by inspecting the
/// last assistant message rather than discriminating on a separate error
/// variant. `Agent::finish_run_outcome` still surfaces `cancel.is_cancelled()`
/// when wrapping a true loop error.
fn finish_with_cancel(emit: &AgentEventSink, messages: Vec<Message>) -> Result<(), AgentError> {
    emit(AgentEvent::AgentEnd { messages });
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming the assistant response
// ---------------------------------------------------------------------------

async fn stream_assistant_response(
    context: &mut AgentContext,
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    client: &model::Client,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> Result<Message, AgentError> {
    // Phase 1: optional context transform (works on Message[]).
    let transformed = if let Some(transform) = &config.transform_context {
        transform(context.messages.clone(), cancel.clone()).await
    } else {
        context.messages.clone()
    };

    // Phase 2: convert to LLM-bound messages.
    let llm_messages = if let Some(convert) = &config.convert_to_llm {
        convert(transformed).await
    } else {
        default_convert_to_llm(transformed)
    };

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

    // Resolve API key dynamically (e.g. for OAuth tokens). Skip the hook
    // when stream_options already carries an explicit api_key — the user
    // has pinned it via `--api-key` and shouldn't have it silently
    // overwritten by a refresh resolver.
    let mut stream_opts = config.stream_options.clone();
    if stream_opts.base.api_key.is_none()
        && let Some(get_api_key) = &config.get_api_key
    {
        let provider_str = config.model.provider.as_str().to_string();
        if let Some(resolved_key) = get_api_key(provider_str).await {
            stream_opts.base.api_key = Some(resolved_key);
        }
    }

    // Single cancellation surface: hand the run's cancel token to the model
    // layer as the stream `signal`. The stream itself then terminates with an
    // aborted terminal event when the caller aborts, so the event loop below
    // is a plain `while let` rather than a second `select!` racing the same
    // stream. Nesting a `select!` over a stream that internally selects on its
    // SSE leaf can intermittently strand a wakeup during long reasoning-token
    // gaps, hanging the turn; a single consumer avoids that shape entirely.
    stream_opts.base.signal = Some(cancel.clone());

    if cancel.is_cancelled() {
        let aborted = synthesize_aborted_message(&config.model, "Aborted before request");
        let msg = Message::Assistant(aborted.clone());
        context.messages.push(msg.clone());
        emit(AgentEvent::MessageStart {
            message: msg.clone(),
        });
        emit(AgentEvent::MessageEnd { message: msg });
        return Ok(Message::Assistant(aborted));
    }

    let mut stream = if let Some(stream_fn) = &config.stream_fn {
        stream_fn(&config.model, llm_context, stream_opts, cancel.clone())
    } else {
        match client.stream_simple(&config.model, llm_context, Some(stream_opts)) {
            Ok(s) => s,
            Err(e) => return Err(AgentError::Client(e)),
        }
    };

    let mut final_message: Option<AssistantMessage> = None;
    let mut emitted_start = false;

    // Plain consumer. Cancellation is honored by the stream itself (via
    // `stream_opts.base.signal`, wired above): on abort it yields a terminal
    // `Error { reason: Aborted, .. }`, which the `Error` arm below turns into a
    // balanced MessageStart/MessageEnd pair and an aborted assistant message.
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
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => {
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
        None => {
            // Stream ended without `Done` or `Error`. Treat as a provider-side
            // failure (like `AssistantMessageEvent::Error`): synthesize an
            // error assistant message in place, replacing the partial that was
            // emitted on `Start`, so subscribers see a balanced
            // `MessageStart`/`MessageEnd` pair and the transcript holds a
            // single closed assistant message.
            let mut failure =
                synthesize_aborted_message(&config.model, "Stream ended without a final message");
            failure.stop_reason = StopReason::Error;
            let msg = Message::Assistant(failure.clone());
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
            Ok(Message::Assistant(failure))
        }
    }
}

fn synthesize_aborted_message(model: &model::Model, reason: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![],
        api: model.api,
        provider: model.provider,
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: model::Usage::default(),
        stop_reason: StopReason::Aborted,
        error_message: Some(reason.into()),
        timestamp: now_ms(),
    }
}

pub(crate) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_convert_to_llm(messages: Vec<Message>) -> Vec<Message> {
    // The default keeps user / assistant / tool-result messages as-is.
    // Custom message types (if introduced via wrappers) should be filtered upstream.
    messages
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

struct ExecutedToolBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// Fail every tool call from an assistant message that stopped with
/// `StopReason::Length`. Streamed tool-call arguments are finalized with a
/// best-effort JSON parser, so a message cut off by the output token limit
/// can carry calls whose arguments parse and validate but are silently
/// incomplete. None of them are safe to run; report each as an error so the
/// model can re-issue the call with complete arguments.
fn fail_truncated_tool_calls(tool_calls: &[&ToolCall], emit: &AgentEventSink) -> ExecutedToolBatch {
    let mut messages = Vec::with_capacity(tool_calls.len());
    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });
        let result = ToolResult::error(format!(
            "Tool call '{}' was not executed: the response hit the output token limit, \
             so its arguments may be truncated. Re-issue the tool call with complete arguments.",
            tool_call.name
        ));
        emit_tool_execution_end(tool_call, &result, true, emit);
        let tr_msg = emit_tool_result_message(tool_call, result, true, emit);
        messages.push(tr_msg);
    }
    ExecutedToolBatch {
        messages,
        terminate: false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[&ToolCall],
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> ExecutedToolBatch {
    // Determine effective execution mode for this batch.
    let any_sequential = tool_calls.iter().any(|tc| {
        tools
            .iter()
            .find(|t| t.name == tc.name)
            .and_then(|t| t.execution_mode)
            == Some(ToolExecutionMode::Sequential)
    });
    let mode = if any_sequential || config.tool_execution == ToolExecutionMode::Sequential {
        ToolExecutionMode::Sequential
    } else {
        ToolExecutionMode::Parallel
    };

    match mode {
        ToolExecutionMode::Sequential => {
            execute_sequential(
                context,
                assistant_message,
                tool_calls,
                tools,
                config,
                emit,
                cancel,
            )
            .await
        }
        ToolExecutionMode::Parallel => {
            execute_parallel(
                context,
                assistant_message,
                tool_calls,
                tools,
                config,
                emit,
                cancel,
            )
            .await
        }
    }
}

async fn execute_sequential(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[&ToolCall],
    tools: &[AgentTool],
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> ExecutedToolBatch {
    let mut messages = Vec::with_capacity(tool_calls.len());
    let mut all_terminate = !tool_calls.is_empty();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, tools, config, cancel).await;

        let (result, is_error) = match preparation {
            ToolCallPreparation::Immediate { result, is_error } => (result, is_error),
            ToolCallPreparation::Prepared { tool, args } => {
                let executed =
                    execute_prepared_tool_call(tool_call, tool, &args, emit, cancel).await;
                finalize_executed_tool_call(
                    context,
                    assistant_message,
                    tool_call,
                    &args,
                    executed,
                    config,
                    cancel,
                )
                .await
            }
        };

        // In sequential mode, completion order == source order, so we emit
        // both the tool_execution_end and the tool-result message inline.
        emit_tool_execution_end(tool_call, &result, is_error, emit);
        if result.terminate != Some(true) {
            all_terminate = false;
        }
        let tr_msg = emit_tool_result_message(tool_call, result, is_error, emit);
        messages.push(tr_msg);
    }

    ExecutedToolBatch {
        messages,
        terminate: all_terminate,
    }
}

async fn execute_parallel<'a>(
    context: &'a AgentContext,
    assistant_message: &'a AssistantMessage,
    tool_calls: &[&'a ToolCall],
    tools: &'a [AgentTool],
    config: &'a AgentLoopConfig,
    emit: &'a AgentEventSink,
    cancel: &'a CancellationToken,
) -> ExecutedToolBatch {
    enum Slot<'b> {
        Immediate {
            result: ToolResult,
            is_error: bool,
        },
        Pending {
            tool: &'b AgentTool,
            args: serde_json::Value,
        },
    }

    // Phase 1: prepare each call sequentially.
    let mut slots: Vec<(&ToolCall, Slot<'a>)> = Vec::with_capacity(tool_calls.len());

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: tool_call.arguments.clone(),
        });

        let preparation =
            prepare_tool_call(context, assistant_message, tool_call, tools, config, cancel).await;

        let slot = match preparation {
            ToolCallPreparation::Immediate { result, is_error } => {
                Slot::Immediate { result, is_error }
            }
            ToolCallPreparation::Prepared { tool, args } => Slot::Pending { tool, args },
        };
        slots.push((*tool_call, slot));
    }

    // Phase 2: build a future per slot. Immediate slots return ready futures;
    // pending slots run concurrently under `join_all`. Each future emits its own
    // `tool_execution_end` so observers see completion order (matches the TS
    // contract). The final `tool_result` message is emitted in source order in
    // Phase 3.
    let futs: Vec<_> = slots
        .iter()
        .map(|(tool_call, slot)| {
            let tc: &ToolCall = tool_call;
            async move {
                let (result, is_error) = match slot {
                    Slot::Immediate { result, is_error } => (result.clone(), *is_error),
                    Slot::Pending { tool, args } => {
                        let executed =
                            execute_prepared_tool_call(tc, tool, args, emit, cancel).await;
                        finalize_executed_tool_call(
                            context,
                            assistant_message,
                            tc,
                            args,
                            executed,
                            config,
                            cancel,
                        )
                        .await
                    }
                };
                emit_tool_execution_end(tc, &result, is_error, emit);
                (result, is_error)
            }
        })
        .collect();

    let outcomes = join_all(futs).await;

    // Phase 3: emit tool-result messages in source order.
    let mut messages = Vec::with_capacity(slots.len());
    let mut all_terminate = !slots.is_empty();

    for ((tool_call, _slot), (result, is_error)) in slots.iter().zip(outcomes) {
        if result.terminate != Some(true) {
            all_terminate = false;
        }
        let tr_msg = emit_tool_result_message(tool_call, result, is_error, emit);
        messages.push(tr_msg);
    }

    ExecutedToolBatch {
        messages,
        terminate: all_terminate,
    }
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
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    tools: &'a [AgentTool],
    config: &AgentLoopConfig,
    cancel: &CancellationToken,
) -> ToolCallPreparation<'a> {
    let Some(tool) = tools.iter().find(|t| t.name == tool_call.name) else {
        return ToolCallPreparation::Immediate {
            result: ToolResult::error(format!("Tool '{}' not found", tool_call.name)),
            is_error: true,
        };
    };

    // Apply prepare_arguments shim, if any.
    let raw_args = tool_call.arguments.clone();
    let prepared_args = match &tool.prepare_arguments {
        Some(prep) => prep(raw_args),
        None => raw_args,
    };

    // JSON-Schema validation, using a per-tool compiled cache.
    if let Err(msg) = validate_tool_args(tool, &prepared_args) {
        return ToolCallPreparation::Immediate {
            result: ToolResult::error(format!("Invalid arguments for tool '{}': {msg}", tool.name)),
            is_error: true,
        };
    }

    // before_tool_call hook.
    if let Some(hook) = &config.before_tool_call {
        let ctx = BeforeToolCallContext {
            assistant_message,
            tool_call,
            args: &prepared_args,
            context,
        };
        if let Some(before_result) = hook(ctx, cancel.clone()).await
            && before_result.block
        {
            let reason = before_result
                .reason
                .unwrap_or_else(|| "Tool execution was blocked".into());
            return ToolCallPreparation::Immediate {
                result: ToolResult::error(reason),
                is_error: true,
            };
        }
    }

    ToolCallPreparation::Prepared {
        tool,
        args: prepared_args,
    }
}

fn validate_tool_args(tool: &AgentTool, args: &serde_json::Value) -> Result<(), String> {
    let Some(compiled) = tool.compiled_schema()? else {
        return Ok(());
    };
    if let Err(errors) = compiled.validate(args) {
        let messages: Vec<String> = errors
            .map(|e| format!("{} (path: {})", e, e.instance_path))
            .collect();
        return Err(messages.join("; "));
    }
    Ok(())
}

async fn execute_prepared_tool_call(
    tool_call: &ToolCall,
    tool: &AgentTool,
    args: &serde_json::Value,
    emit: &AgentEventSink,
    cancel: &CancellationToken,
) -> (ToolResult, bool) {
    let tool_call_id = tool_call.id.clone();
    let tool_name = tool_call.name.clone();
    let raw_args = tool_call.arguments.clone();

    let on_update_emit = emit.clone();
    let on_update: OnUpdate = Arc::new(move |partial: ToolResult| {
        on_update_emit(AgentEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: raw_args.clone(),
            partial_result: partial,
        });
    });

    let ctx = ToolExecuteCtx {
        tool_call_id: tool_call.id.clone(),
        args: args.clone(),
        cancel: cancel.clone(),
        on_update,
    };

    // Race the tool future against cancellation. Tools constructed with
    // `AgentTool::simple` cannot observe the cancel token directly, so the
    // loop has to enforce the abort contract here; dropping the future
    // releases any resources owned across the await point.
    let exec_future = (tool.execute)(ctx);
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return (
                ToolResult::error(format!(
                    "Tool '{}' aborted by caller",
                    tool_call.name
                )),
                true,
            );
        }
        res = exec_future => res,
    };

    match result {
        Ok(result) => (result, false),
        Err(err) => (ToolResult::error(err.to_string()), true),
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_executed_tool_call(
    context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    args: &serde_json::Value,
    executed: (ToolResult, bool),
    config: &AgentLoopConfig,
    cancel: &CancellationToken,
) -> (ToolResult, bool) {
    let (mut result, mut is_error) = executed;

    if let Some(hook) = &config.after_tool_call {
        let ctx = AfterToolCallContext {
            assistant_message,
            tool_call,
            args,
            result: &result,
            is_error,
            context,
        };
        if let Some(override_result) = hook(ctx, cancel.clone()).await {
            apply_after_override(&mut result, &mut is_error, override_result);
        }
    }

    (result, is_error)
}

fn apply_after_override(
    result: &mut ToolResult,
    is_error: &mut bool,
    override_result: AfterToolCallResult,
) {
    if let Some(content) = override_result.content {
        result.content = content;
    }
    if let Some(details) = override_result.details {
        result.details = Some(details);
    }
    if let Some(terminate) = override_result.terminate {
        result.terminate = Some(terminate);
    }
    if let Some(err) = override_result.is_error {
        *is_error = err;
    }
}

/// Emit `tool_execution_end` for one finalized tool call. Called in completion
/// order (sequential or as each parallel future resolves) so observers can
/// drive per-tool completion UI without waiting on slower siblings.
fn emit_tool_execution_end(
    tool_call: &ToolCall,
    result: &ToolResult,
    is_error: bool,
    emit: &AgentEventSink,
) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        result: result.clone(),
        is_error,
    });
}

/// Build the `ToolResultMessage` and emit its `MessageStart`/`MessageEnd`.
/// In parallel mode this is always called in source order (after `join_all`).
fn emit_tool_result_message(
    tool_call: &ToolCall,
    result: ToolResult,
    is_error: bool,
    emit: &AgentEventSink,
) -> ToolResultMessage {
    let mut tr_msg =
        ToolResultMessage::new(tool_call.id.clone(), tool_call.name.clone(), result.content);
    tr_msg.is_error = is_error;
    tr_msg.details = result.details;

    let msg = Message::ToolResult(tr_msg.clone());
    emit(AgentEvent::MessageStart {
        message: msg.clone(),
    });
    emit(AgentEvent::MessageEnd { message: msg });

    tr_msg
}

// ---------------------------------------------------------------------------
// Steering / follow-up
// ---------------------------------------------------------------------------

async fn get_steering_messages(
    config: &AgentLoopConfig,
    cancel: &CancellationToken,
) -> Vec<Message> {
    if cancel.is_cancelled() {
        return Vec::new();
    }
    match &config.get_steering_messages {
        Some(hook) => hook().await,
        None => Vec::new(),
    }
}

async fn get_follow_up_messages(
    config: &AgentLoopConfig,
    cancel: &CancellationToken,
) -> Vec<Message> {
    if cancel.is_cancelled() {
        return Vec::new();
    }
    match &config.get_follow_up_messages {
        Some(hook) => hook().await,
        None => Vec::new(),
    }
}
