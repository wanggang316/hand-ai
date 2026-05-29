//! RPC dispatcher: bridges JSONL stdin/stdout to an [`AgentSession`].
//!
//! Reads [`RpcCommand`] frames from a JSONL input stream, routes each to a
//! handler on an owned [`AgentSession`], and forwards the session's events
//! back out to the JSONL output stream. The dispatcher exits cleanly when
//! the input stream ends.
//!
//! # Concurrency model
//!
//! Mostly single-task. Commands are processed sequentially on the
//! dispatcher's task; while a `prompt` is driving a turn through the
//! agent loop, the dispatcher does not pull the next command. Events
//! stream out in real time because they are emitted by the session's
//! subscribe callback, which forwards through an `mpsc` channel to a
//! separate writer task.
//!
//! `bash` is one exception: it races [`AgentSession::run_bash`]
//! against further input frames so an `abort_bash` arriving mid-flight
//! can cancel the executor (see [`AgentSession::abort_bash`]).
//! `AbortBash` is dispatched inline during the race — it only borrows
//! `&session`, same as `run_bash`, so the two coexist without the rest
//! of the dispatcher needing to be made multitasking. Other commands
//! arriving during a `bash` are deferred and processed through the
//! normal path once `run_bash` returns.
//!
//! `prompt` is the second exception: while `send_message` is in flight
//! (which exclusively borrows `&mut session`), the dispatcher continues
//! to read further frames and services `steer` / `follow_up` inline by
//! pushing onto queue handles cloned from the session BEFORE the prompt
//! starts. The agent loop drains those queues at the next turn boundary
//! via the `get_steering_messages` / `get_follow_up_messages` callbacks
//! wired in [`AgentSession::send_message`]. `abort` and `abort_retry`
//! are also dispatched inline — both call [`AgentSession::abort`], which
//! only borrows `&self` and flips the cancellation token, making them
//! symmetric with `abort_bash` during a `bash` race. `get_state` /
//! `get_messages` remain deferred until the prompt completes — wiring
//! those inline requires `Arc`-based shared state for the read paths,
//! which is a follow-up.
//!
//! The TS port (`upstream coding-agent/src/modes/rpc/rpc-mode.ts`) is fully
//! multitasking: it parks the in-flight prompt as a Promise and
//! continues reading commands so every command type can interrupt a
//! turn. The Rust port is incrementally getting there, one in-flight
//! command-class at a time.

use crate::core::agent_session::{AgentSession, AgentSessionEvent, build_user_message};
use crate::rpc::jsonl::{JsonlReadError, read_jsonl, write_jsonl};
use crate::rpc::types::{
    BashRpcData, CloneData, CommandsData, ExportHtmlData, ForkData, ForkMessagesData,
    LastAssistantTextData, MessagesData, NewSessionData, RpcCommand, RpcResponse, RpcResponseBody,
    RpcResultEmpty, RpcResultWithData, RpcSessionState, RpcSlashCommand, SlashCommandSource,
    SwitchSessionData,
};
use futures::StreamExt;
use hand_agent::types::AgentEvent;
use model::Message;
use model::types::ThinkingLevel;
use serde::Serialize;
use std::io;
use std::path::PathBuf;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::mpsc;

/// Errors the RPC dispatcher can surface to its caller.
///
/// Per-command errors (parse failures, unknown variants) are reported as
/// JSONL `RpcResponse` failures and do not terminate the loop; this enum
/// only fires for errors that prevent the dispatcher from making any more
/// progress (e.g. the writer task panicking).
#[derive(Debug, thiserror::Error)]
pub enum RpcServerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("session error: {0}")]
    Session(#[from] crate::core::error::CodingAgentError),
}

/// Wire envelope for an outbound session event.
///
/// The dispatcher emits `{type: "event", event: <WireSessionEvent>}` per
/// frame. The exact event payload shape (`agent` / `compaction_start` /
/// `compaction_end` / `error`) lives in [`WireSessionEvent`] below.
/// The TS impl uses the same `{type: "event", ...}` discriminator, so
/// this is a starting point that matches the spirit of the wire while
/// staying minimal until the full event-format port lands.
#[derive(Debug, Clone, Serialize)]
struct EventEnvelope {
    #[serde(rename = "type")]
    envelope_type: EventTag,
    event: WireSessionEvent,
}

#[derive(Debug, Clone, Copy, Serialize)]
enum EventTag {
    #[serde(rename = "event")]
    Event,
}

/// Serializable mirror of [`AgentSessionEvent`]. Kept private here so the
/// wire shape lives next to the dispatcher; promotion to a public type
/// can happen when more event consumers appear.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireSessionEvent {
    // `AgentEvent` is large after the origin/main merge — clippy's
    // `large_enum_variant` lint flags the unboxed form. Box to keep the
    // enum size small; `serde` serializes Box<T> identically to T so
    // the JSONL wire shape is unchanged.
    Agent(Box<AgentEvent>),
    CompactionStart,
    CompactionEnd {
        summary: String,
    },
    Error {
        message: String,
    },
    /// Session metadata changed (currently the display name). RPC
    /// clients listen on this so a UI rendering the session list can
    /// refresh after a `/name` command without polling.
    SessionInfoChanged {
        name: Option<String>,
    },
}

impl From<AgentSessionEvent> for WireSessionEvent {
    fn from(event: AgentSessionEvent) -> Self {
        match event {
            AgentSessionEvent::Agent(e) => WireSessionEvent::Agent(e),
            AgentSessionEvent::CompactionStart => WireSessionEvent::CompactionStart,
            AgentSessionEvent::CompactionEnd { summary } => {
                WireSessionEvent::CompactionEnd { summary }
            }
            AgentSessionEvent::Error(message) => WireSessionEvent::Error { message },
            AgentSessionEvent::SessionInfoChanged { name } => {
                WireSessionEvent::SessionInfoChanged { name }
            }
        }
    }
}

/// One outbound JSONL frame. Both responses and events are funneled through
/// a single channel so writes are serialized — interleaving on stdout would
/// produce undecodable JSONL.
enum Outbound {
    Response(Box<RpcResponse>),
    Event(Box<EventEnvelope>),
}

/// Run the RPC dispatcher until the input stream ends.
///
/// Per-command errors (parse failures, malformed JSON, unknown commands)
/// are reported via JSONL failure responses and do NOT terminate the loop.
/// On clean EOF the function returns `Ok(())`.
pub async fn run_rpc_server<R, W>(
    reader: R,
    writer: W,
    mut session: AgentSession,
) -> Result<(), RpcServerError>
where
    R: AsyncBufRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    // Outbound channel: dispatcher and event listener both push frames;
    // the writer task drains it. Unbounded because back-pressure from
    // stdout would otherwise deadlock the synchronous `subscribe`
    // callback (which is invoked from the agent loop).
    let (tx, mut rx) = mpsc::unbounded_channel::<Outbound>();

    // Subscribe the session to the outbound channel before any commands
    // can drive a turn.
    let event_tx = tx.clone();
    session.subscribe(move |event| {
        let envelope = EventEnvelope {
            envelope_type: EventTag::Event,
            event: event.into(),
        };
        // Best-effort: if the writer task is gone the receiver is closed
        // and we drop the event. The dispatcher loop will exit shortly
        // after on its own.
        let _ = event_tx.send(Outbound::Event(Box::new(envelope)));
    });

    // Spawn the writer task. It owns the writer and serializes every
    // frame through the same `write_jsonl` helper used for inbound
    // framing.
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(out) = rx.recv().await {
            // Writer error is fatal: drop further frames and bail out.
            // The dispatcher will notice on the next send.
            match out {
                Outbound::Response(resp) => write_jsonl(&mut writer, &*resp).await?,
                Outbound::Event(evt) => write_jsonl(&mut writer, &*evt).await?,
            }
        }
        Ok::<_, io::Error>(())
    });

    // Drive the inbound stream. Each iteration handles one parse result.
    let mut stream = Box::pin(read_jsonl::<R, RpcCommand>(reader));
    // Commands that arrived while a `bash` was in flight and weren't
    // serviceable inline (i.e. anything other than `AbortBash`). Drained
    // through the normal dispatch path once `run_bash` returns. Empty
    // for the common case (no `bash` ever ran).
    let mut deferred: Vec<RpcCommand> = Vec::new();
    loop {
        // Service any commands deferred while a previous `bash` was
        // running before pulling the next frame off the wire.
        let item_opt = if let Some(cmd) = deferred.pop() {
            Some(Ok(cmd))
        } else {
            stream.next().await
        };
        let Some(item) = item_opt else {
            break;
        };
        match item {
            // Special case: `bash` is the only long-running command on
            // the dispatcher hot path that must remain interruptible by
            // a follow-up `abort_bash` arriving while it's in flight.
            // We race the executor future against further input frames
            // — `abort_bash` is dispatched inline (it only borrows
            // `&session`, same as `run_bash`), everything else is
            // queued for after-bash dispatch. This keeps the rest of
            // the dispatcher single-task while satisfying the
            // `AbortBash` interrupt semantics that motivated
            // `bash_cancel`.
            Ok(RpcCommand::Bash { id, command }) => {
                let mut io_fatal: Option<io::Error> = None;
                // Inner block scopes the `&session` borrow held by
                // `bash_fut` so we can `drop(session)` later if a
                // fatal reader I/O error occurred.
                let outcome = {
                    let bash_fut = session.run_bash(&command, 120);
                    tokio::pin!(bash_fut);
                    loop {
                        tokio::select! {
                            biased;
                            res = &mut bash_fut => break res,
                            next = stream.next() => match next {
                                Some(Ok(RpcCommand::AbortBash { id: aid })) => {
                                    let _ = session.abort_bash();
                                    let resp = RpcResponse::new(
                                        aid,
                                        RpcResponseBody::AbortBash(RpcResultEmpty::ok()),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        // Writer dropped — finish the
                                        // bash future so `kill_on_drop`
                                        // reaps the child, then bail.
                                        break bash_fut.await;
                                    }
                                }
                                Some(Ok(other)) => {
                                    deferred.insert(0, other);
                                }
                                Some(Err(JsonlReadError::Parse { source, .. })) => {
                                    let resp = RpcResponse::new(
                                        None,
                                        RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                                            "invalid JSON: {source}"
                                        ))),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break bash_fut.await;
                                    }
                                }
                                Some(Err(JsonlReadError::Utf8(e))) => {
                                    let resp = RpcResponse::new(
                                        None,
                                        RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                                            "invalid UTF-8 in command frame: {e}"
                                        ))),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break bash_fut.await;
                                    }
                                }
                                Some(Err(JsonlReadError::Io(e))) => {
                                    // Reader I/O is fatal — finish the
                                    // bash future so its child is reaped
                                    // via `kill_on_drop` (the bash
                                    // response itself is dropped on the
                                    // floor — the writer's about to go
                                    // away anyway). Stash the io error
                                    // and break the inner loop so the
                                    // borrow on `session` ends before we
                                    // drop it.
                                    io_fatal = Some(e);
                                    break bash_fut.await;
                                }
                                None => {
                                    // Stream EOF: still need to deliver
                                    // the bash response, so just await
                                    // the future to completion.
                                    break bash_fut.await;
                                }
                            }
                        }
                    }
                };
                // Inner-loop reader I/O propagates here so the
                // borrow on `session` (held by `bash_fut`) has
                // already ended.
                if let Some(e) = io_fatal {
                    drop(tx);
                    drop(session);
                    let _ = writer_task.await;
                    return Err(RpcServerError::Io(e));
                }
                let response = match outcome {
                    Ok(outcome) => {
                        let (stdout, stderr) = if outcome.aborted {
                            (String::new(), outcome.result.output)
                        } else {
                            (outcome.result.output, String::new())
                        };
                        RpcResponse::new(
                            id,
                            RpcResponseBody::Bash(RpcResultWithData::ok(BashRpcData {
                                stdout,
                                stderr,
                                exit_code: outcome.result.exit_code,
                                truncated: outcome.result.truncated,
                            })),
                        )
                    }
                    Err(e) => RpcResponse::new(
                        id,
                        RpcResponseBody::Bash(RpcResultWithData::<BashRpcData>::err(format!(
                            "bash failed: {e}"
                        ))),
                    ),
                };
                if tx.send(Outbound::Response(Box::new(response))).is_err() {
                    break;
                }
            }
            // Special case: `prompt` drives `send_message`, which holds
            // `&mut session` for the duration of the agent loop turn. To
            // service `steer` / `follow_up` arriving mid-stream we clone
            // the session's queue handles BEFORE starting the prompt and
            // push to them inline during the race. Everything else is
            // deferred (no `&self` access path is available while
            // `send_message` borrows mutably). See module docs.
            Ok(RpcCommand::Prompt {
                id,
                message,
                images,
                ..
            }) => {
                let steering_q = session.steering_queue_handle();
                let follow_up_q = session.follow_up_queue_handle();
                let cancel_handle = session.cancel_handle();
                let mut io_fatal: Option<io::Error> = None;
                let result = {
                    let prompt_fut = session.send_message_with_images(&message, images);
                    tokio::pin!(prompt_fut);
                    loop {
                        tokio::select! {
                            biased;
                            res = &mut prompt_fut => break res,
                            next = stream.next() => match next {
                                Some(Ok(RpcCommand::Steer { id: sid, message: smsg, images })) => {
                                    let user = Message::User(build_user_message(&smsg, images));
                                    steering_q.lock().unwrap().push(user);
                                    let resp = RpcResponse::new(
                                        sid,
                                        RpcResponseBody::Steer(RpcResultEmpty::ok()),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Ok(RpcCommand::FollowUp { id: fid, message: fmsg, images })) => {
                                    let user = Message::User(build_user_message(&fmsg, images));
                                    follow_up_q.lock().unwrap().push(user);
                                    let resp = RpcResponse::new(
                                        fid,
                                        RpcResponseBody::FollowUp(RpcResultEmpty::ok()),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Ok(RpcCommand::Abort { id: aid })) => {
                                    // `session.abort()` itself is
                                    // unreachable mid-prompt because
                                    // `send_message` exclusively borrows
                                    // `&mut session`. We pre-cloned the
                                    // cancel-token handle for exactly
                                    // this — flipping it has identical
                                    // semantics to `session.abort()` and
                                    // unwinds the agent loop at its next
                                    // await point. Symmetric with
                                    // `abort_bash` during a `bash` race.
                                    {
                                        let token = cancel_handle.lock().unwrap();
                                        if !token.is_cancelled() {
                                            token.cancel();
                                        }
                                    }
                                    let resp = RpcResponse::new(
                                        aid,
                                        RpcResponseBody::Abort(RpcResultEmpty::ok()),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Ok(RpcCommand::AbortRetry { id: aid })) => {
                                    // Same primitive as `Abort` — both
                                    // map to cancelling the turn token
                                    // because there's no dedicated
                                    // retry-only cancel hook yet (see
                                    // the top-level `AbortRetry`
                                    // handler).
                                    {
                                        let token = cancel_handle.lock().unwrap();
                                        if !token.is_cancelled() {
                                            token.cancel();
                                        }
                                    }
                                    let resp = RpcResponse::new(
                                        aid,
                                        RpcResponseBody::AbortRetry(RpcResultEmpty::ok()),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Ok(other)) => {
                                    // Defer everything else until the
                                    // prompt completes — `&mut session`
                                    // is unreachable from here.
                                    deferred.insert(0, other);
                                }
                                Some(Err(JsonlReadError::Parse { source, .. })) => {
                                    let resp = RpcResponse::new(
                                        None,
                                        RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                                            "invalid JSON: {source}"
                                        ))),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Err(JsonlReadError::Utf8(e))) => {
                                    let resp = RpcResponse::new(
                                        None,
                                        RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                                            "invalid UTF-8 in command frame: {e}"
                                        ))),
                                    );
                                    if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                                        break prompt_fut.await;
                                    }
                                }
                                Some(Err(JsonlReadError::Io(e))) => {
                                    // Reader I/O is fatal. Finish the
                                    // prompt future first so the
                                    // borrow on `session` ends before
                                    // we drop it below.
                                    io_fatal = Some(e);
                                    break prompt_fut.await;
                                }
                                None => {
                                    // EOF: still deliver the prompt
                                    // response, then exit normally.
                                    break prompt_fut.await;
                                }
                            }
                        }
                    }
                };
                if let Some(e) = io_fatal {
                    drop(tx);
                    drop(session);
                    let _ = writer_task.await;
                    return Err(RpcServerError::Io(e));
                }
                let response = match result {
                    Ok(_) => RpcResponse::new(id, RpcResponseBody::Prompt(RpcResultEmpty::ok())),
                    Err(e) => RpcResponse::new(
                        id,
                        RpcResponseBody::Prompt(RpcResultEmpty::err(format!("prompt failed: {e}"))),
                    ),
                };
                if tx.send(Outbound::Response(Box::new(response))).is_err() {
                    break;
                }
            }
            Ok(cmd) => {
                let response = handle_command(&mut session, cmd).await;
                if tx.send(Outbound::Response(Box::new(response))).is_err() {
                    // Writer dropped; nothing more we can do.
                    break;
                }
            }
            Err(JsonlReadError::Parse { source, .. }) => {
                // Parse failed before we could read the command kind, so
                // we cannot attach a typed `RpcResponseBody::<X>` body
                // truthfully. Emit `Invalid` (command: "invalid") so the
                // wire shape stays valid JSON and the discriminator
                // distinguishes a parse rejection from a prompt failure.
                let resp = RpcResponse::new(
                    None,
                    RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                        "invalid JSON: {source}"
                    ))),
                );
                if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                    break;
                }
            }
            Err(JsonlReadError::Utf8(e)) => {
                // Same treatment as a parse error — see above.
                let resp = RpcResponse::new(
                    None,
                    RpcResponseBody::Invalid(RpcResultEmpty::err(format!(
                        "invalid UTF-8 in command frame: {e}"
                    ))),
                );
                if tx.send(Outbound::Response(Box::new(resp))).is_err() {
                    break;
                }
            }
            Err(JsonlReadError::Io(e)) => {
                // I/O on the reader is fatal for the dispatcher.
                // Drop the session first so the cloned sender held by
                // its subscribe closure is released; without that the
                // writer task hangs (see EOF branch below).
                drop(tx);
                drop(session);
                let _ = writer_task.await;
                return Err(RpcServerError::Io(e));
            }
        }
    }

    // EOF: drop our outbound sender, then drop the session so its
    // `event_listeners` Arc is released along with the cloned sender
    // captured in the subscribe closure. Without dropping the session
    // first, the writer task would hang waiting for that last sender to
    // close. Then await the writer task to flush any pending frames.
    drop(tx);
    drop(session);
    match writer_task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(RpcServerError::Io(e)),
        Err(join_err) => Err(RpcServerError::Io(io::Error::other(format!(
            "writer task panicked: {join_err}"
        )))),
    }
}

/// Dispatch a single command on the owned `AgentSession`.
///
/// Returns the [`RpcResponse`] to write back. For `prompt`, this awaits
/// the full agent turn (single-task model — see module docs); events
/// stream out via the subscribe channel during the await.
async fn handle_command(session: &mut AgentSession, cmd: RpcCommand) -> RpcResponse {
    match cmd {
        RpcCommand::Prompt {
            id,
            message,
            images,
            ..
        } => {
            // Per the brief: emit success once the prompt is queued. In
            // this single-task model the await runs the whole turn
            // synchronously before this match returns, so by the time we
            // reply the turn has already completed. Events arrive on the
            // outbound channel before the response — the writer
            // serializes all frames in send order, so consumers see the
            // event stream first and the success last.
            match session.send_message_with_images(&message, images).await {
                Ok(_) => RpcResponse::new(id, RpcResponseBody::Prompt(RpcResultEmpty::ok())),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::Prompt(RpcResultEmpty::err(format!("prompt failed: {e}"))),
                ),
            }
        }

        RpcCommand::NewSession { id, .. } => {
            // Reset the session in place. `reset_session` clears the
            // conversation state but preserves the model, client, tools,
            // extensions, AND — crucially for this handler —
            // `event_listeners`. The dispatcher subscribed once at startup
            // (see `run_rpc_server`); a wholesale `*session = new` would
            // drop that subscription and post-reset events would no
            // longer reach the client. See C1 in T1.3 for the original
            // regression and the test below for the guard.
            //
            // NOTE: this path does not currently honor `parent_session`
            // (the `new_session` parent-fork variant). Forking from a
            // specific message is exposed via the dedicated `fork` /
            // `clone` commands instead.
            match session.reset_session() {
                Ok(()) => RpcResponse::new(
                    id,
                    RpcResponseBody::NewSession(RpcResultWithData::ok(NewSessionData {
                        cancelled: false,
                    })),
                ),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::NewSession(RpcResultWithData::err(format!(
                        "reset failed: {e}"
                    ))),
                ),
            }
        }

        RpcCommand::GetState { id } => {
            let state = build_session_state(session);
            RpcResponse::new(id, RpcResponseBody::GetState(RpcResultWithData::ok(state)))
        }

        RpcCommand::GetMessages { id } => {
            // `messages` are typed as opaque JSON in the wire protocol
            // (see the TODO on `MessagesData`). Round-trip each through
            // serde_json::to_value so the envelope is faithful even
            // though the inner shape is not yet a typed
            // `AgentMessage` port.
            let messages = session
                .messages()
                .iter()
                .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>();
            RpcResponse::new(
                id,
                RpcResponseBody::GetMessages(RpcResultWithData::ok(MessagesData { messages })),
            )
        }

        RpcCommand::SetMessages { id, messages } => {
            // Restore a persisted transcript into this session's context.
            // Entries that don't deserialize into a `Message` (e.g. UI-only
            // roles the browser layered on) are skipped rather than failing.
            let restored: Vec<Message> = messages
                .into_iter()
                .filter_map(|v| serde_json::from_value::<Message>(v).ok())
                .collect();
            session.set_messages(restored);
            RpcResponse::new(id, RpcResponseBody::SetMessages(RpcResultEmpty::ok()))
        }

        RpcCommand::Abort { id } => {
            // Cancel the in-flight turn (if any). Idempotent: returning
            // `success: true` for both "cancelled a running turn" and
            // "nothing to cancel" matches the TS reference, which
            // treats abort as a fire-and-forget signal.
            let _ = session.abort();
            RpcResponse::new(id, RpcResponseBody::Abort(RpcResultEmpty::ok()))
        }
        RpcCommand::Steer {
            id,
            message,
            images,
        } => {
            // Enqueue + ack regardless of whether a prompt is in flight.
            // The agent loop drains the queue at the next turn boundary
            // via `get_steering_messages`; if no prompt is running the
            // message just waits there until the next `send_message`.
            session.enqueue_steer(&message, images);
            RpcResponse::new(id, RpcResponseBody::Steer(RpcResultEmpty::ok()))
        }
        RpcCommand::FollowUp {
            id,
            message,
            images,
        } => {
            // Same enqueue-and-ack semantics as Steer; the follow-up
            // queue is drained between turns by `get_follow_up_messages`.
            session.enqueue_follow_up(&message, images);
            RpcResponse::new(id, RpcResponseBody::FollowUp(RpcResultEmpty::ok()))
        }
        RpcCommand::SetModel {
            id,
            provider,
            model_id,
        } => match session.model_registry().find(&provider, &model_id) {
            Some(model) => {
                let model = model.clone();
                let value = match serde_json::to_value(&model) {
                    Ok(v) => v,
                    Err(e) => {
                        return RpcResponse::new(
                            id,
                            RpcResponseBody::SetModel(RpcResultWithData::err(format!(
                                "failed to serialize model: {e}"
                            ))),
                        );
                    }
                };
                session.set_model(model);
                RpcResponse::new(id, RpcResponseBody::SetModel(RpcResultWithData::ok(value)))
            }
            None => RpcResponse::new(
                id,
                RpcResponseBody::SetModel(RpcResultWithData::err(format!(
                    "model not found: {provider}/{model_id}"
                ))),
            ),
        },

        RpcCommand::CycleModel { id } => {
            let next = session.model_registry().next(session.model()).cloned();
            match next {
                Some(model) => {
                    let value = match serde_json::to_value(&model) {
                        Ok(v) => v,
                        Err(e) => {
                            return RpcResponse::new(
                                id,
                                RpcResponseBody::CycleModel(RpcResultWithData::err(format!(
                                    "failed to serialize model: {e}"
                                ))),
                            );
                        }
                    };
                    let thinking_level = session
                        .stream_options()
                        .reasoning
                        .unwrap_or(ThinkingLevel::Medium);
                    session.set_model(model);
                    RpcResponse::new(
                        id,
                        RpcResponseBody::CycleModel(RpcResultWithData::ok(Some(
                            crate::rpc::types::CycleModelData {
                                model: value,
                                thinking_level,
                                // `is_scoped` is a TS concept tracking whether
                                // the model came from a scoped (per-cwd /
                                // per-project) override. We have no such
                                // override surface yet; report `false` until
                                // the settings port lands.
                                is_scoped: false,
                            },
                        ))),
                    )
                }
                None => {
                    RpcResponse::new(id, RpcResponseBody::CycleModel(RpcResultWithData::ok(None)))
                }
            }
        }

        RpcCommand::GetAvailableModels { id } => {
            // Each `Model` is `Serialize`; round-trip via `serde_json::to_value`
            // because `AvailableModelsData::models` is opaque JSON in the wire
            // protocol (see TODO on the type).
            let models = session
                .model_registry()
                .all()
                .iter()
                .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>();
            RpcResponse::new(
                id,
                RpcResponseBody::GetAvailableModels(RpcResultWithData::ok(
                    crate::rpc::types::AvailableModelsData { models },
                )),
            )
        }
        RpcCommand::SetThinkingLevel { id, level } => {
            let mut opts = session.stream_options().clone();
            opts.reasoning = Some(level);
            session.set_stream_options(opts);
            RpcResponse::new(id, RpcResponseBody::SetThinkingLevel(RpcResultEmpty::ok()))
        }
        RpcCommand::CycleThinkingLevel { id } => {
            // Cycle order matches the TS reference's full ladder:
            // minimal → low → medium → high → xhigh → minimal.
            // Treat "unset" as Medium so cycling from the implicit
            // default lands somewhere predictable (High).
            let current = session
                .stream_options()
                .reasoning
                .unwrap_or(ThinkingLevel::Medium);
            let next = match current {
                ThinkingLevel::Minimal => ThinkingLevel::Low,
                ThinkingLevel::Low => ThinkingLevel::Medium,
                ThinkingLevel::Medium => ThinkingLevel::High,
                ThinkingLevel::High => ThinkingLevel::Xhigh,
                ThinkingLevel::Xhigh => ThinkingLevel::Minimal,
            };
            let mut opts = session.stream_options().clone();
            opts.reasoning = Some(next);
            session.set_stream_options(opts);
            RpcResponse::new(
                id,
                RpcResponseBody::CycleThinkingLevel(RpcResultWithData::ok(Some(
                    crate::rpc::types::CycleThinkingLevelData { level: next },
                ))),
            )
        }
        RpcCommand::SetSteeringMode { id, mode } => {
            session.set_steering_mode(mode);
            RpcResponse::new(id, RpcResponseBody::SetSteeringMode(RpcResultEmpty::ok()))
        }
        RpcCommand::SetFollowUpMode { id, mode } => {
            session.set_follow_up_mode(mode);
            RpcResponse::new(id, RpcResponseBody::SetFollowUpMode(RpcResultEmpty::ok()))
        }
        RpcCommand::Compact {
            id,
            custom_instructions: _,
        } => {
            // `custom_instructions` from the wire is currently dropped:
            // `compaction::build_compaction_prompt` does not yet accept
            // a per-call instruction string. The TS reference threads it
            // through; restoring that requires a small helper change in
            // core/compaction.rs (TODO).
            match session.compact().await {
                Ok(summary) => RpcResponse::new(
                    id,
                    RpcResponseBody::Compact(RpcResultWithData::ok(serde_json::json!({
                        "summary": summary,
                    }))),
                ),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::Compact(RpcResultWithData::<serde_json::Value>::err(format!(
                        "compaction failed: {e}"
                    ))),
                ),
            }
        }
        RpcCommand::SetAutoCompaction { id, enabled } => {
            session.set_auto_compaction(enabled);
            RpcResponse::new(id, RpcResponseBody::SetAutoCompaction(RpcResultEmpty::ok()))
        }
        RpcCommand::SetAutoRetry { id, enabled } => {
            session.set_auto_retry(enabled);
            RpcResponse::new(id, RpcResponseBody::SetAutoRetry(RpcResultEmpty::ok()))
        }
        RpcCommand::AbortRetry { id } => {
            // Without dedicated retry-state tracking, abort_retry maps
            // to abort: cancelling the cancellation token unwinds the
            // current retry-with-backoff sleep along with the rest of
            // the turn. A finer-grained "cancel just the backoff, let
            // the loop surface the underlying error" can be added once
            // hand-agent exposes a retry-only cancel hook.
            let _ = session.abort();
            RpcResponse::new(id, RpcResponseBody::AbortRetry(RpcResultEmpty::ok()))
        }
        RpcCommand::Bash { id, .. } => {
            // `Bash` is intercepted by the dispatcher loop (see
            // `run_rpc_server`) so it can race the executor future
            // against further input frames for in-flight `abort_bash`.
            // Reaching this arm would mean a routing bug; emit a
            // structured error rather than panicking.
            RpcResponse::new(
                id,
                RpcResponseBody::Bash(RpcResultWithData::<BashRpcData>::err(
                    "internal: bash command must be intercepted by dispatcher loop".to_string(),
                )),
            )
        }
        RpcCommand::AbortBash { id } => {
            // Idempotent — matches `abort` semantics. Returns success
            // even when no bash is running, mirroring how the TS
            // reference treats abort_bash as a fire-and-forget signal.
            let _ = session.abort_bash();
            RpcResponse::new(id, RpcResponseBody::AbortBash(RpcResultEmpty::ok()))
        }
        RpcCommand::GetSessionStats { id } => {
            // `GetSessionStats` is typed as opaque JSON in the wire
            // protocol pending the typed `SessionStats` port. Compose
            // the same fields the TS reference emits — id / name /
            // message count / model id / provider / cwd — so consumers
            // see a stable shape today.
            let stats = serde_json::json!({
                "sessionId": session.session_id(),
                "sessionName": session.label(),
                "messageCount": session.message_count(),
                "modelId": session.model().id,
                "provider": session.model().provider,
                "cwd": session.cwd().to_string_lossy(),
            });
            RpcResponse::new(
                id,
                RpcResponseBody::GetSessionStats(RpcResultWithData::ok(stats)),
            )
        }
        RpcCommand::ExportHtml { id, output_path } => {
            // Default output: "<session_id>.html" under the session cwd.
            let path = output_path
                .map(PathBuf::from)
                .unwrap_or_else(|| session.cwd().join(format!("{}.html", session.session_id())));
            match crate::core::export::export_to_html(
                session.messages(),
                session.session_id(),
                &session.model().id,
                &path,
            ) {
                Ok(()) => RpcResponse::new(
                    id,
                    RpcResponseBody::ExportHtml(RpcResultWithData::ok(ExportHtmlData {
                        path: path.to_string_lossy().into_owned(),
                    })),
                ),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::ExportHtml(RpcResultWithData::<ExportHtmlData>::err(format!(
                        "export failed: {e}"
                    ))),
                ),
            }
        }
        RpcCommand::SwitchSession { id, session_path } => {
            match session.switch_session(std::path::Path::new(&session_path)) {
                Ok(()) => RpcResponse::new(
                    id,
                    RpcResponseBody::SwitchSession(RpcResultWithData::ok(SwitchSessionData {
                        cancelled: false,
                    })),
                ),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::SwitchSession(RpcResultWithData::<SwitchSessionData>::err(
                        format!("switch_session failed: {e}"),
                    )),
                ),
            }
        }
        RpcCommand::Fork { id, entry_id } => match session.fork(&entry_id) {
            Ok(text) => RpcResponse::new(
                id,
                RpcResponseBody::Fork(RpcResultWithData::ok(ForkData {
                    text,
                    cancelled: false,
                })),
            ),
            Err(e) => RpcResponse::new(
                id,
                RpcResponseBody::Fork(RpcResultWithData::<ForkData>::err(format!(
                    "fork failed: {e}"
                ))),
            ),
        },
        RpcCommand::Clone { id } => match session.clone_session() {
            Ok(()) => RpcResponse::new(
                id,
                RpcResponseBody::Clone(RpcResultWithData::ok(CloneData { cancelled: false })),
            ),
            Err(e) => RpcResponse::new(
                id,
                RpcResponseBody::Clone(RpcResultWithData::<CloneData>::err(format!(
                    "clone failed: {e}"
                ))),
            ),
        },
        RpcCommand::GetForkMessages { id } => {
            let messages = session.fork_messages();
            RpcResponse::new(
                id,
                RpcResponseBody::GetForkMessages(RpcResultWithData::ok(ForkMessagesData {
                    messages,
                })),
            )
        }
        RpcCommand::GetLastAssistantText { id } => {
            // Walk messages in reverse, find the last assistant message with a
            // text block, and return its concatenated text. If no assistant
            // text exists yet, return an empty string (parity with TS).
            let last_text = session
                .messages()
                .iter()
                .rev()
                .find_map(|m| match m {
                    model::Message::Assistant(a) => {
                        let combined: String = a
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                model::types::AssistantContentBlock::Text(t) => {
                                    Some(t.text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        if combined.is_empty() {
                            None
                        } else {
                            Some(combined)
                        }
                    }
                    _ => None,
                })
                .unwrap_or_default();
            RpcResponse::new(
                id,
                RpcResponseBody::GetLastAssistantText(RpcResultWithData::ok(
                    LastAssistantTextData {
                        text: if last_text.is_empty() {
                            None
                        } else {
                            Some(last_text)
                        },
                    },
                )),
            )
        }
        RpcCommand::SetSessionName { id, name } => match session.set_label(&name) {
            Ok(()) => RpcResponse::new(id, RpcResponseBody::SetSessionName(RpcResultEmpty::ok())),
            Err(e) => RpcResponse::new(
                id,
                RpcResponseBody::SetSessionName(RpcResultEmpty::err(format!(
                    "failed to set session name: {e}"
                ))),
            ),
        },
        RpcCommand::GetCommands { id } => {
            // Built-in commands shadow extension commands of the same
            // name (handled by `SlashCommandRegistry::resolve`); the
            // wire response listing simply tags them by source so the
            // client can filter or render accordingly.
            let mut commands: Vec<RpcSlashCommand> = builtin_command_specs();
            for (spec, _ext) in session.collected_slash_commands() {
                commands.push(RpcSlashCommand {
                    name: spec.name.clone(),
                    description: Some(spec.description.clone()),
                    source: SlashCommandSource::Extension,
                    source_info: serde_json::Value::Null,
                });
            }
            RpcResponse::new(
                id,
                RpcResponseBody::GetCommands(RpcResultWithData::ok(CommandsData { commands })),
            )
        }
    }
}

/// Built-in slash command specs. Built from a fresh
/// [`crate::core::slash_commands::SlashCommandRegistry::new`] so the
/// surface here tracks the dispatcher's actual built-in set.
fn builtin_command_specs() -> Vec<RpcSlashCommand> {
    let registry = crate::core::slash_commands::SlashCommandRegistry::new();
    registry
        .commands()
        .iter()
        .map(|c| RpcSlashCommand {
            name: c.name.clone(),
            description: Some(c.description.clone()),
            source: SlashCommandSource::Builtin,
            source_info: serde_json::Value::Null,
        })
        .collect()
}

/// Build a snapshot of the session for `get_state`.
///
/// Pulls runtime state directly from `AgentSession` accessors.
/// `pending_message_count` is the sum of the steer + follow-up queue
/// lengths.
fn build_session_state(session: &AgentSession) -> RpcSessionState {
    let model_value = serde_json::to_value(session.model()).ok();
    let thinking_level = session
        .stream_options()
        .reasoning
        .unwrap_or(ThinkingLevel::Medium);
    let session_file = session
        .session_file()
        .and_then(|p| p.to_str().map(String::from));
    RpcSessionState {
        model: model_value,
        thinking_level,
        is_streaming: session.is_streaming(),
        is_compacting: session.is_compacting(),
        steering_mode: session.steering_mode(),
        follow_up_mode: session.follow_up_mode(),
        session_file,
        session_id: session.session_id().to_string(),
        session_name: session.label().map(str::to_string),
        auto_compaction_enabled: session.auto_compaction_enabled(),
        message_count: session.message_count() as u64,
        pending_message_count: session.pending_message_count(),
    }
}

#[cfg(test)]
mod tests {
    //! Dispatcher unit tests.
    //!
    //! These tests use `tokio::io::duplex` to wire the dispatcher's reader
    //! and writer to in-memory streams. Mock providers come from
    //! `tests/common/mod.rs` (T0.3); we re-implement the smallest piece
    //! we need (a text-only provider) inline because Cargo does not let
    //! `#[cfg(test)]` modules in `src/` import from `tests/`.
    use super::*;
    use crate::core::agent_session::AgentSession;
    use futures::StreamExt;
    use model::types::{
        Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Cost,
        InputType, Model, Provider, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
        Usage,
    };
    use model::{ApiProvider, AssistantMessageEventStream};
    use serde_json::Value;
    use tokio::io::{AsyncWriteExt, BufReader, duplex};

    fn test_model() -> Model {
        Model {
            id: "test-model".into(),
            name: "Test".into(),
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            base_url: "https://api.test.com".into(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 128_000,
            max_tokens: 4096,
            headers: None,
            compat: None,
            thinking_level_map: None,
        }
    }

    fn assistant_text_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// Local clone of the `mock_text_provider` from `tests/common/mod.rs`.
    /// Cargo isolates integration test helpers from `src/` unit tests,
    /// so we duplicate the minimum needed here.
    struct MockTextProvider {
        text: String,
    }

    impl ApiProvider for MockTextProvider {
        fn stream(
            &self,
            _model: Model,
            _context: Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream<'static> {
            let text = self.text.clone();
            Box::pin(async_stream::stream! {
                let partial = assistant_text_message("");
                yield AssistantMessageEvent::Start { partial: partial.clone() };
                yield AssistantMessageEvent::TextStart {
                    content_index: 0,
                    partial: partial.clone(),
                };
                yield AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: text.clone(),
                    partial: partial.clone(),
                };
                let final_msg = assistant_text_message(&text);
                yield AssistantMessageEvent::TextEnd {
                    content_index: 0,
                    content: text.clone(),
                    partial: final_msg.clone(),
                };
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: final_msg,
                };
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

    fn mock_text_provider(text: &str) -> Box<dyn ApiProvider + Send + Sync> {
        Box::new(MockTextProvider { text: text.into() })
    }

    /// Build a session whose registry has the mock text provider wired
    /// up for the test model's API. Returns the session ready for the
    /// dispatcher.
    fn session_with_mock(text: &str) -> AgentSession {
        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            mock_text_provider(text),
            Some("test".into()),
        );
        AgentSession::in_memory_with_client(test_model(), Vec::new(), client)
    }

    /// Drain all JSONL frames from the writer side until EOF.
    ///
    /// Tests close the dispatcher's input first (drop `in_tx`); the
    /// dispatcher then closes the writer; this drains the writer.
    async fn drain_frames(rx: tokio::io::DuplexStream) -> Vec<Value> {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut rx = rx;
        rx.read_to_end(&mut bytes).await.unwrap();
        let cursor = std::io::Cursor::new(bytes);
        let reader = BufReader::new(cursor);
        let mut stream = Box::pin(read_jsonl::<_, Value>(reader));
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(v) => out.push(v),
                Err(e) => panic!("frame error while reading: {e}"),
            }
        }
        out
    }

    /// Convenience: drive the dispatcher on a spawned task, return the
    /// (input-writer, output-reader, dispatcher-join-handle) triple.
    async fn spawn_dispatcher(
        session: AgentSession,
    ) -> (
        tokio::io::DuplexStream,
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<Result<(), RpcServerError>>,
    ) {
        let (in_tx, in_rx) = duplex(8192);
        let (out_tx, out_rx) = duplex(8192);
        let reader = BufReader::new(in_rx);
        let handle = tokio::spawn(run_rpc_server(reader, out_tx, session));
        (in_tx, out_rx, handle)
    }

    #[tokio::test]
    async fn smoke_get_state_returns_session_id() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"get_state\",\"id\":\"1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "expected one response, got: {frames:#?}");
        let resp = &frames[0];
        assert_eq!(resp["type"], "response");
        assert_eq!(resp["command"], "get_state");
        assert_eq!(resp["success"], true);
        assert_eq!(resp["id"], "1");
        assert!(
            resp["data"]["sessionId"].is_string(),
            "sessionId must be present, got: {resp:?}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn get_messages_on_fresh_session_returns_empty_array() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"get_messages\",\"id\":\"2\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1);
        let resp = &frames[0];
        assert_eq!(resp["command"], "get_messages");
        assert_eq!(resp["success"], true);
        let messages = resp["data"]["messages"].as_array().unwrap();
        assert!(
            messages.is_empty(),
            "messages must be empty on fresh session"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn prompt_emits_assistant_text_event_then_success_response() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("hello")).await;

        in_tx
            .write_all(b"{\"type\":\"prompt\",\"message\":\"hi\",\"id\":\"42\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert!(!frames.is_empty(), "expected at least one frame, got none");

        // The success response must appear (last response with the right id).
        let response = frames
            .iter()
            .find(|f| f["type"] == "response")
            .expect("expected a response frame");
        assert_eq!(response["command"], "prompt");
        assert_eq!(response["success"], true);
        assert_eq!(response["id"], "42");

        // At least one event frame must carry the assistant text "hello".
        let saw_text = frames
            .iter()
            .any(|f| f["type"] == "event" && f.to_string().contains("\"hello\""));
        assert!(
            saw_text,
            "expected at least one event frame to carry the assistant text. frames: {frames:#?}"
        );

        // Ordering invariant: events stream during the turn, the success
        // response arrives AFTER the turn completes. This is the
        // load-bearing contract of the single-task dispatcher model.
        let response_idx = frames
            .iter()
            .position(|f| f["type"] == "response")
            .expect("expected a success response frame");
        let first_event_idx = frames
            .iter()
            .position(|f| f["type"] == "event")
            .expect("expected at least one event frame before the response");
        assert!(
            first_event_idx < response_idx,
            "events must arrive before the success response; \
             got first_event_idx={first_event_idx}, response_idx={response_idx}, \
             frames: {frames:#?}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn new_session_returns_cancelled_false() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"new_session\",\"id\":\"3\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1);
        let resp = &frames[0];
        assert_eq!(resp["command"], "new_session");
        assert_eq!(resp["success"], true);
        assert_eq!(resp["data"]["cancelled"], false);

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn new_session_preserves_client_registry_so_subsequent_prompt_works() {
        // Regression guard for I2: `new_session` must reuse the existing
        // `Client` so the provider registry survives the reset. If the
        // handler regresses to `Client::new()`, the prompt issued AFTER
        // `new_session` would fail with `ProviderNotFound` because the
        // mock_text_provider lives only on the original client.
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("hello-after-reset")).await;

        // 1. Reset the session.
        in_tx
            .write_all(b"{\"type\":\"new_session\",\"id\":\"ns-1\"}\n")
            .await
            .unwrap();

        // 2. Prompt on the reset session — must succeed because the
        //    registry was preserved.
        in_tx
            .write_all(b"{\"type\":\"prompt\",\"message\":\"hi\",\"id\":\"p-1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        // Locate the new_session response: success with cancelled=false.
        let new_session_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "new_session")
            .expect("expected a new_session response");
        assert_eq!(new_session_resp["success"], true);
        assert_eq!(new_session_resp["data"]["cancelled"], false);
        assert_eq!(new_session_resp["id"], "ns-1");

        // Locate the prompt response: success is the real registry-survival
        // signal — if `new_session` had dropped the registry, the
        // `OpenAICompletions` provider lookup would fail and the handler
        // would emit `success: false` with `prompt failed: ...
        // ProviderNotFound`. A green prompt response on the reset session
        // proves the provider survived.
        let prompt_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "prompt")
            .expect("expected a prompt response");
        assert_eq!(
            prompt_resp["success"], true,
            "prompt after new_session must succeed; if registry was \
             dropped the provider lookup would fail. frames: {frames:#?}"
        );
        assert_eq!(prompt_resp["id"], "p-1");

        // C1 regression: event listeners must survive `new_session`.
        // The dispatcher subscribes once at startup; if the `NewSession`
        // handler regressed to wholesale-replacing the session, the
        // post-reset `prompt` would emit no event frames because the
        // subscription was attached to the dropped session. Count any
        // event frame received AFTER the new_session response — they
        // can only originate from the reset session's turn.
        let new_session_idx = frames
            .iter()
            .position(|f| f["type"] == "response" && f["command"] == "new_session")
            .expect("expected a new_session response");
        let events_after_reset = frames
            .iter()
            .skip(new_session_idx + 1)
            .filter(|f| f["type"] == "event")
            .count();
        assert!(
            events_after_reset >= 1,
            "expected at least one event frame on the post-new_session \
             prompt — listeners must survive new_session. frames: {frames:#?}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn malformed_line_yields_error_response_then_real_command_succeeds() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"not json\n{\"type\":\"get_state\",\"id\":\"7\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "expected two frames, got: {frames:#?}");

        // First frame: parse-error response (no id, success: false,
        // command "invalid" — distinct from any real command kind).
        let err = &frames[0];
        assert_eq!(err["type"], "response");
        assert_eq!(err["command"], "invalid");
        assert_eq!(err["success"], false);
        assert!(
            err["error"].as_str().unwrap_or("").contains("invalid JSON"),
            "expected parse error message, got: {err}"
        );

        // Second frame: real success response.
        let ok = &frames[1];
        assert_eq!(ok["type"], "response");
        assert_eq!(ok["command"], "get_state");
        assert_eq!(ok["success"], true);
        assert_eq!(ok["id"], "7");

        handle.await.unwrap().unwrap();
    }

    /// `abort` returns success even when no turn is running, and a
    /// subsequent `abort` is also idempotent. The token is replaced at
    /// the next `send_message` so a stale cancel doesn't poison future
    /// turns.
    #[tokio::test]
    async fn abort_with_no_inflight_turn_succeeds() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"abort","id":"1"}
{"type":"abort","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "frames: {frames:#?}");
        for f in &frames {
            assert_eq!(f["command"], "abort");
            assert_eq!(f["success"], true);
        }

        handle.await.unwrap().unwrap();
    }

    /// `bash` runs a simple command and surfaces stdout + exit code on the
    /// wire. The current mapping puts the executor's combined output on
    /// `stdout` and leaves `stderr` empty.
    #[tokio::test]
    async fn bash_executes_simple_command() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"bash\",\"id\":\"1\",\"command\":\"echo hello\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        let resp = &frames[0];
        assert_eq!(resp["command"], "bash");
        assert_eq!(resp["success"], true);
        assert_eq!(resp["id"], "1");
        let data = &resp["data"];
        let stdout = data["stdout"].as_str().expect("stdout is string");
        assert!(
            stdout.contains("hello"),
            "expected hello in stdout: {stdout:?}"
        );
        assert_eq!(data["exitCode"], 0);
        assert_eq!(data["truncated"], false);

        handle.await.unwrap().unwrap();
    }

    /// A failing command surfaces its exit code on the wire.
    #[tokio::test]
    async fn bash_failure_surfaces_exit_code() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"bash\",\"id\":\"1\",\"command\":\"exit 7\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        let resp = &frames[0];
        assert_eq!(resp["command"], "bash");
        assert_eq!(resp["success"], true);
        assert_eq!(resp["data"]["exitCode"], 7);

        handle.await.unwrap().unwrap();
    }

    /// `abort_bash` is idempotent: returns success even when no bash is
    /// running, matching how `abort` already behaves.
    #[tokio::test]
    async fn abort_bash_with_no_inflight_succeeds() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"abort_bash","id":"1"}
{"type":"abort_bash","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "frames: {frames:#?}");
        for f in &frames {
            assert_eq!(f["command"], "abort_bash");
            assert_eq!(f["success"], true);
        }

        handle.await.unwrap().unwrap();
    }

    /// `abort_bash` on an in-flight `bash` interrupts the running command
    /// rather than waiting for it to finish naturally. The wall-clock
    /// timeout guards against regressions: a real hang would manifest
    /// as the 5s budget elapsing, not a 30s sleep completing.
    #[tokio::test]
    async fn bash_abort_interrupts_running_command() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;
        in_tx
            .write_all(b"{\"type\":\"bash\",\"id\":\"1\",\"command\":\"sleep 30\"}\n")
            .await
            .unwrap();
        // Give the dispatcher a moment to enter run_bash before we
        // signal cancel — otherwise the abort token gets reset by the
        // bash handler after the abort fires.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        in_tx
            .write_all(b"{\"type\":\"abort_bash\",\"id\":\"2\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        // Wall-clock budget: a regression manifests as timeout, not 30s hang.
        let frames = tokio::time::timeout(std::time::Duration::from_secs(5), drain_frames(out_rx))
            .await
            .expect("dispatcher must respond within 5s when bash is aborted");
        let bash = frames.iter().find(|f| f["id"] == "1").unwrap();
        assert_eq!(bash["data"]["truncated"], true);
        // Abort marker lands on stderr per `BashRpcData` doc; stdout is
        // empty on the cancel arm.
        assert!(
            bash["data"]["stderr"]
                .as_str()
                .unwrap_or("")
                .contains("aborted"),
            "expected stderr to carry abort marker, got: {bash:#?}"
        );
        assert_eq!(bash["data"]["stdout"], "");
        handle.await.unwrap().unwrap();
    }

    /// `set_thinking_level` should mutate `stream_options.reasoning` and
    /// surface the new value via the next `get_state`.
    #[tokio::test]
    async fn set_thinking_level_mutates_stream_options() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"set_thinking_level","id":"1","level":"high"}
{"type":"get_state","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "frames: {frames:#?}");
        assert_eq!(frames[0]["command"], "set_thinking_level");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[1]["command"], "get_state");
        assert_eq!(frames[1]["data"]["thinkingLevel"], "high");

        handle.await.unwrap().unwrap();
    }

    /// `cycle_thinking_level` returns the new level and rotates the
    /// full ladder (default Medium → High).
    #[tokio::test]
    async fn cycle_thinking_level_rotates_from_default() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"cycle_thinking_level\",\"id\":\"1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        assert_eq!(frames[0]["command"], "cycle_thinking_level");
        assert_eq!(frames[0]["success"], true);
        // Default unset is treated as Medium → High.
        assert_eq!(frames[0]["data"]["level"], "high");

        handle.await.unwrap().unwrap();
    }

    /// Mode setters reflect immediately in `get_state`.
    #[tokio::test]
    async fn steering_and_followup_modes_round_trip() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"set_steering_mode","id":"1","mode":"all"}
{"type":"set_follow_up_mode","id":"2","mode":"all"}
{"type":"set_auto_compaction","id":"3","enabled":false}
{"type":"set_auto_retry","id":"4","enabled":false}
{"type":"get_state","id":"5"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 5, "frames: {frames:#?}");
        let state = &frames[4]["data"];
        assert_eq!(state["steeringMode"], "all");
        assert_eq!(state["followUpMode"], "all");
        assert_eq!(state["autoCompactionEnabled"], false);

        handle.await.unwrap().unwrap();
    }

    /// `set_session_name` propagates into `get_state.session_name` via
    /// `SessionManager::append_label`.
    #[tokio::test]
    async fn set_session_name_persists() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"set_session_name","id":"1","name":"my session"}
{"type":"get_state","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        // Three frames: set_session_name response, the
        // SessionInfoChanged event, then the get_state response. Order
        // between the event and the second response can race, so we
        // identify by type instead of index.
        assert_eq!(frames.len(), 3, "frames: {frames:#?}");
        let response_frames: Vec<_> = frames.iter().filter(|f| f["type"] == "response").collect();
        let event_frames: Vec<_> = frames.iter().filter(|f| f["type"] == "event").collect();
        assert_eq!(response_frames.len(), 2, "expected 2 responses");
        assert_eq!(event_frames.len(), 1, "expected 1 session_info_changed");
        assert_eq!(response_frames[0]["command"], "set_session_name");
        assert_eq!(response_frames[0]["success"], true);
        assert_eq!(response_frames[1]["data"]["sessionName"], "my session");
        // The event carries the new name. WireSessionEvent uses `kind`
        // as the serde tag (not `type`), so the inner discriminator is
        // `kind: "session_info_changed"`.
        assert_eq!(event_frames[0]["event"]["kind"], "session_info_changed");
        assert_eq!(event_frames[0]["event"]["name"], "my session");

        handle.await.unwrap().unwrap();
    }

    /// `get_session_stats` returns the session id, model id, and cwd.
    #[tokio::test]
    async fn get_session_stats_returns_basic_fields() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"get_session_stats\",\"id\":\"1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        let data = &frames[0]["data"];
        assert!(data["sessionId"].is_string(), "stats: {data:#?}");
        assert!(data["modelId"].is_string());
        assert!(data["cwd"].is_string());

        handle.await.unwrap().unwrap();
    }

    /// `get_commands` returns the built-in command set tagged
    /// `source: "builtin"`.
    #[tokio::test]
    async fn get_commands_returns_builtins() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"get_commands\",\"id\":\"1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        let cmds = frames[0]["data"]["commands"].as_array().unwrap();
        assert!(!cmds.is_empty(), "expected at least one builtin command");
        for c in cmds {
            assert_eq!(
                c["source"], "builtin",
                "expected builtin source, got: {c:#?}"
            );
        }
        // Sanity check: `/help` should be in there.
        let names: Vec<&str> = cmds.iter().filter_map(|c| c["name"].as_str()).collect();
        assert!(
            names.contains(&"help"),
            "expected /help in builtin commands, got: {names:?}"
        );

        handle.await.unwrap().unwrap();
    }

    /// `set_model` finds the model in the registry, updates the session,
    /// and surfaces it via `get_state`.
    #[tokio::test]
    async fn set_model_unknown_returns_error() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"set_model","id":"1","provider":"nope","modelId":"nope"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1, "frames: {frames:#?}");
        assert_eq!(frames[0]["command"], "set_model");
        assert_eq!(frames[0]["success"], false);
        assert!(
            frames[0]["error"]
                .as_str()
                .unwrap_or("")
                .contains("model not found"),
            "expected error: {:#?}",
            frames[0]
        );

        handle.await.unwrap().unwrap();
    }

    /// `steer` sent with no prompt in flight enqueues + acks immediately,
    /// and the queued message shows up in `get_state.pending_message_count`.
    #[tokio::test]
    async fn steer_enqueues_and_acks() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"steer","id":"1","message":"hi"}
{"type":"get_state","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "frames: {frames:#?}");
        assert_eq!(frames[0]["command"], "steer");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["id"], "1");
        assert_eq!(frames[1]["command"], "get_state");
        assert_eq!(frames[1]["data"]["pendingMessageCount"], 1);

        handle.await.unwrap().unwrap();
    }

    /// `follow_up` sent with no prompt in flight enqueues + acks
    /// immediately, with the same `pending_message_count` round-trip as
    /// `steer`.
    #[tokio::test]
    async fn follow_up_enqueues_and_acks() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"follow_up","id":"1","message":"later"}
{"type":"get_state","id":"2"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 2, "frames: {frames:#?}");
        assert_eq!(frames[0]["command"], "follow_up");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["id"], "1");
        assert_eq!(frames[1]["data"]["pendingMessageCount"], 1);

        handle.await.unwrap().unwrap();
    }

    /// `pending_message_count` reports the SUM of the two queues, not just
    /// one of them. Pin the contract so a future regression that reads
    /// only `steering_queue` (or only `follow_up_queue`) is caught.
    #[tokio::test]
    async fn pending_message_count_sums_both_queues() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                br#"{"type":"steer","id":"1","message":"a"}
{"type":"follow_up","id":"2","message":"b"}
{"type":"get_state","id":"3"}
"#,
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 3, "frames: {frames:#?}");
        assert_eq!(frames[2]["data"]["pendingMessageCount"], 2);

        handle.await.unwrap().unwrap();
    }

    /// While a long-running `prompt` is in flight, a `steer` frame is
    /// dispatched inline by the dispatcher loop and its response arrives
    /// BEFORE the prompt's. Wall-clock timeout guards against
    /// regressions: a deferred `steer` would only surface after the
    /// pending-forever provider returned, which it never does.
    #[tokio::test]
    async fn steer_during_prompt_is_acked_immediately() {
        // Provider that pends forever inside `stream` so the agent loop
        // never returns naturally. The dispatcher's mid-flight
        // interrupt is the only way the test can finish.
        struct PendingForeverProvider;
        impl ApiProvider for PendingForeverProvider {
            fn stream(
                &self,
                _model: Model,
                _context: Context,
                _options: Option<StreamOptions>,
            ) -> AssistantMessageEventStream<'static> {
                Box::pin(async_stream::stream! {
                    let () = std::future::pending().await;
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: assistant_text_message("unreachable"),
                    };
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

        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PendingForeverProvider),
            Some("test".into()),
        );
        let session = AgentSession::in_memory_with_client(test_model(), Vec::new(), client);

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        in_tx
            .write_all(b"{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hello\"}\n")
            .await
            .unwrap();
        // Give the dispatcher a moment to enter `send_message` before
        // the steer arrives — otherwise the steer races with the
        // prompt's own initial steering-queue poll.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        in_tx
            .write_all(b"{\"type\":\"steer\",\"id\":\"2\",\"message\":\"mid-stream\"}\n")
            .await
            .unwrap();
        // Give the dispatcher a chance to ack the steer, then close
        // the input. Closing alone is not enough to unblock the
        // pending provider — we abort the dispatcher task via the
        // wall-clock timeout below.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(in_tx);

        // The steer ack must arrive within the budget. The prompt
        // response will never arrive (pending forever), so we time
        // out the drain and abort the dispatcher.
        let frames = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            let mut rx = out_rx;
            // Read until we have at least one frame for "steer";
            // the prompt frame will never come.
            let mut buf = [0u8; 4096];
            loop {
                let n = rx.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&bytes).contains("\"command\":\"steer\"") {
                    break;
                }
            }
            bytes
        })
        .await
        .expect("steer ack must arrive within 5s during in-flight prompt");

        let text = String::from_utf8_lossy(&frames);
        assert!(
            text.contains("\"command\":\"steer\""),
            "expected steer ack frame, got: {text}"
        );
        assert!(
            text.contains("\"id\":\"2\""),
            "expected steer ack to carry id=2, got: {text}"
        );

        // Clean up the dispatcher task — the prompt is hung forever.
        handle.abort();
    }

    /// Abort fired during an in-flight prompt is dispatched inline (not
    /// deferred) — its ack arrives promptly and the dispatcher finishes
    /// within a wall-clock budget instead of hanging on the
    /// pending-forever provider. Mirrors the abort_bash-during-bash test
    /// from T-A1.
    #[tokio::test]
    async fn abort_during_prompt_is_acked_immediately() {
        // Provider that pends forever inside `stream` — only an inline
        // `session.abort()` (flipping the cancellation token) can let
        // the prompt unwind so the dispatcher exits cleanly.
        struct PendingForeverProvider;
        impl ApiProvider for PendingForeverProvider {
            fn stream(
                &self,
                _model: Model,
                _context: Context,
                _options: Option<StreamOptions>,
            ) -> AssistantMessageEventStream<'static> {
                Box::pin(async_stream::stream! {
                    let () = std::future::pending().await;
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: assistant_text_message("unreachable"),
                    };
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

        let client = model::Client::new();
        client.registry.register(
            Api::OpenAICompletions,
            Box::new(PendingForeverProvider),
            Some("test".into()),
        );
        let session = AgentSession::in_memory_with_client(test_model(), Vec::new(), client);

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        in_tx
            .write_all(b"{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hi\"}\n")
            .await
            .unwrap();
        // Give the dispatcher a moment to enter the prompt's inner
        // select before the abort arrives.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        in_tx
            .write_all(b"{\"type\":\"abort\",\"id\":\"2\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        // Wall-clock budget: a regression (deferred abort) manifests as
        // timeout because the pending-forever provider never returns.
        let frames = tokio::time::timeout(std::time::Duration::from_secs(5), drain_frames(out_rx))
            .await
            .expect("dispatcher must respond within 5s when prompt is aborted");

        // The abort ack must be present. Order vs the prompt response
        // doesn't matter — what's load-bearing is that the inline
        // dispatch sent the ack before we ever returned to the deferred
        // bucket, which is what allowed the dispatcher to finish at all.
        let abort = frames
            .iter()
            .find(|f| f["id"] == "2")
            .expect("abort response must be present");
        assert_eq!(abort["command"], "abort");
        assert_eq!(abort["success"], true);

        handle.await.unwrap().unwrap();
    }

    /// Seed `session_manager` with `texts.len()` user messages and return
    /// the JSONL entry IDs assigned to each, in input order. Decouples
    /// fork/clone tests from a full `send_message` round-trip.
    fn seed_user_messages(session: &mut AgentSession, texts: &[&str]) -> Vec<String> {
        use model::UserMessage;
        let mgr = session.session_manager_mut();
        texts
            .iter()
            .map(|t| {
                mgr.append_message(Message::User(UserMessage::new_text(*t)))
                    .expect("append_message must succeed on in-memory session")
            })
            .collect()
    }

    #[tokio::test]
    async fn fork_truncates_history_and_returns_text() {
        let mut session = session_with_mock("ignored");
        let ids = seed_user_messages(&mut session, &["first", "second", "third"]);
        let fork_target = ids[1].clone();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        // Issue fork at the second user message — history should
        // truncate to just the first.
        let cmd = format!("{{\"type\":\"fork\",\"id\":\"f-1\",\"entryId\":\"{fork_target}\"}}\n");
        in_tx.write_all(cmd.as_bytes()).await.unwrap();
        in_tx
            .write_all(b"{\"type\":\"get_state\",\"id\":\"gs-1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        let fork_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "fork")
            .expect("expected a fork response");
        assert_eq!(fork_resp["success"], true, "frames: {frames:#?}");
        assert_eq!(fork_resp["id"], "f-1");
        assert_eq!(fork_resp["data"]["text"], "second");
        assert_eq!(fork_resp["data"]["cancelled"], false);

        let state_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "get_state")
            .expect("expected a get_state response");
        assert_eq!(state_resp["success"], true);
        assert_eq!(
            state_resp["data"]["messageCount"], 1,
            "fork must truncate to entries strictly before target; frames: {frames:#?}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fork_unknown_entry_id_returns_error() {
        let session = session_with_mock("ignored");
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        in_tx
            .write_all(b"{\"type\":\"fork\",\"id\":\"f-2\",\"entryId\":\"bogus_entry_id\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        let resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "fork")
            .expect("expected a fork response");
        assert_eq!(resp["success"], false, "frames: {frames:#?}");
        assert_eq!(resp["id"], "f-2");
        let err = resp["error"].as_str().unwrap_or("");
        assert!(
            err.contains("not found"),
            "expected 'not found' in error, got: {err}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn clone_preserves_full_history_and_changes_session_id() {
        let mut session = session_with_mock("ignored");
        seed_user_messages(&mut session, &["a", "b", "c"]);
        let pre_id = session.session_id().to_string();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        in_tx
            .write_all(b"{\"type\":\"clone\",\"id\":\"c-1\"}\n")
            .await
            .unwrap();
        in_tx
            .write_all(b"{\"type\":\"get_state\",\"id\":\"gs-2\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        let clone_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "clone")
            .expect("expected a clone response");
        assert_eq!(clone_resp["success"], true, "frames: {frames:#?}");
        assert_eq!(clone_resp["id"], "c-1");
        assert_eq!(clone_resp["data"]["cancelled"], false);

        let state_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "get_state")
            .expect("expected a get_state response");
        assert_eq!(
            state_resp["data"]["messageCount"], 3,
            "clone must keep all messages; frames: {frames:#?}"
        );
        let post_id = state_resp["data"]["sessionId"]
            .as_str()
            .expect("sessionId must be a string")
            .to_string();
        assert_ne!(
            post_id, pre_id,
            "clone must produce a fresh session id; pre={pre_id} post={post_id}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fork_preserves_listeners_so_post_fork_events_reach_subscribers() {
        // C1-style regression guard for fork: the dispatcher subscribed
        // once at startup; if the fork handler regressed to wholesale-
        // replacing the session, post-fork events would never reach
        // the wire. After fork we issue a `prompt` whose mock provider
        // emits at least one assistant-message event; counting any
        // event frame received AFTER the fork response proves the
        // subscription survived. Mirrors
        // `new_session_preserves_client_registry_so_subsequent_prompt_works`.
        let mut session = session_with_mock("post-fork-reply");
        let ids = seed_user_messages(&mut session, &["seed-1", "seed-2"]);
        let fork_target = ids[1].clone();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        let cmd = format!("{{\"type\":\"fork\",\"id\":\"f-3\",\"entryId\":\"{fork_target}\"}}\n");
        in_tx.write_all(cmd.as_bytes()).await.unwrap();
        in_tx
            .write_all(b"{\"type\":\"prompt\",\"message\":\"hi\",\"id\":\"p-1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        let fork_idx = frames
            .iter()
            .position(|f| f["type"] == "response" && f["command"] == "fork")
            .expect("expected a fork response");
        let events_after_fork = frames
            .iter()
            .skip(fork_idx + 1)
            .filter(|f| f["type"] == "event")
            .count();
        assert!(
            events_after_fork >= 1,
            "expected at least one event frame after fork — listeners must \
             survive fork. frames: {frames:#?}"
        );

        let prompt_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "prompt")
            .expect("expected a prompt response");
        assert_eq!(prompt_resp["success"], true, "frames: {frames:#?}");

        handle.await.unwrap().unwrap();
    }

    /// Build a real on-disk session under `cwd` containing `texts` as
    /// successive user messages. Returns the JSONL path and the session
    /// id of the constructed file. Used by `switch_session` tests; the
    /// dispatcher's session is in-memory by default, so the source side
    /// of the switch needs a separate, real `SessionManager`.
    fn write_session_with_users(
        cwd: &std::path::Path,
        texts: &[&str],
    ) -> (std::path::PathBuf, String) {
        use crate::core::session_manager::SessionManager;
        use model::UserMessage;
        let mut sm = SessionManager::create(cwd).expect("create session");
        for t in texts {
            sm.append_message(Message::User(UserMessage::new_text(*t)))
                .expect("append message");
        }
        (sm.path().to_path_buf(), sm.id().to_string())
    }

    #[tokio::test]
    async fn switch_session_loads_jsonl_and_replaces_active() {
        // Materialize a real on-disk session under a tempdir, then ask
        // the dispatcher (whose session is in-memory and empty) to
        // switch to it. After the switch, `get_state` must reflect the
        // loaded file's id and message count.
        let tmp = tempfile::TempDir::new().unwrap();
        let (path, src_id) = write_session_with_users(tmp.path(), &["hello", "world", "third"]);
        let path_str = path.to_str().unwrap().to_string();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        let cmd = format!(
            "{{\"type\":\"switch_session\",\"id\":\"sw-1\",\"sessionPath\":\"{}\"}}\n",
            path_str
        );
        in_tx.write_all(cmd.as_bytes()).await.unwrap();
        in_tx
            .write_all(b"{\"type\":\"get_state\",\"id\":\"gs-sw-1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        let switch_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "switch_session")
            .expect("expected a switch_session response");
        assert_eq!(switch_resp["success"], true, "frames: {frames:#?}");
        assert_eq!(switch_resp["id"], "sw-1");
        assert_eq!(switch_resp["data"]["cancelled"], false);

        let state_resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "get_state")
            .expect("expected a get_state response");
        assert_eq!(state_resp["success"], true);
        assert_eq!(
            state_resp["data"]["sessionId"], src_id,
            "switch must adopt the loaded file's session id; frames: {frames:#?}"
        );
        assert_eq!(
            state_resp["data"]["messageCount"], 3,
            "switch must adopt the loaded file's message count; frames: {frames:#?}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn switch_session_unknown_path_returns_error() {
        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(
                b"{\"type\":\"switch_session\",\"id\":\"sw-2\",\"sessionPath\":\"/tmp/does-not-exist-zzzz.jsonl\"}\n",
            )
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        let resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "switch_session")
            .expect("expected a switch_session response");
        assert_eq!(resp["success"], false, "frames: {frames:#?}");
        assert_eq!(resp["id"], "sw-2");
        let err = resp["error"].as_str().unwrap_or("");
        assert!(
            err.contains("failed"),
            "expected 'failed' in error, got: {err}"
        );

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn get_fork_messages_returns_user_messages_only() {
        // Seed two user messages and one assistant message; the
        // response must list only the user entries, in order, with
        // their assigned entry ids and concatenated text.
        use model::UserMessage;
        let mut session = session_with_mock("ignored");
        let mgr = session.session_manager_mut();
        let id_first = mgr
            .append_message(Message::User(UserMessage::new_text("first user")))
            .unwrap();
        // Assistant message in between — must not appear in fork list.
        mgr.append_message(Message::Assistant(assistant_text_message("hi")))
            .unwrap();
        let id_second = mgr
            .append_message(Message::User(UserMessage::new_text("second user")))
            .unwrap();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session).await;

        in_tx
            .write_all(b"{\"type\":\"get_fork_messages\",\"id\":\"gfm-1\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        let resp = frames
            .iter()
            .find(|f| f["type"] == "response" && f["command"] == "get_fork_messages")
            .expect("expected a get_fork_messages response");
        assert_eq!(resp["success"], true, "frames: {frames:#?}");
        assert_eq!(resp["id"], "gfm-1");

        let messages = resp["data"]["messages"].as_array().expect("messages array");
        assert_eq!(
            messages.len(),
            2,
            "expected exactly the 2 user messages, got: {messages:?}"
        );
        assert_eq!(messages[0]["entryId"], id_first);
        assert_eq!(messages[0]["text"], "first user");
        assert_eq!(messages[1]["entryId"], id_second);
        assert_eq!(messages[1]["text"], "second user");

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn switch_session_preserves_listeners() {
        // Mirrors `fork_preserves_listeners_*`: the dispatcher subscribes
        // once at startup. If `switch_session` regressed to wholesale-
        // replacing the session, post-switch responses (and any future
        // events) would never reach the wire because the subscription
        // would be attached to the dropped session. Issue a `get_state`
        // AFTER the switch and assert it round-trips — that response
        // can only be emitted by the post-switch dispatcher loop, which
        // requires the writer task and event-pump glue to have survived.
        let tmp = tempfile::TempDir::new().unwrap();
        let (path, src_id) = write_session_with_users(tmp.path(), &["alpha", "beta"]);
        let path_str = path.to_str().unwrap().to_string();

        let (mut in_tx, out_rx, handle) = spawn_dispatcher(session_with_mock("ignored")).await;

        let cmd = format!(
            "{{\"type\":\"switch_session\",\"id\":\"sw-3\",\"sessionPath\":\"{}\"}}\n",
            path_str
        );
        in_tx.write_all(cmd.as_bytes()).await.unwrap();
        in_tx
            .write_all(b"{\"type\":\"get_state\",\"id\":\"gs-sw-3\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;

        let switch_idx = frames
            .iter()
            .position(|f| f["type"] == "response" && f["command"] == "switch_session")
            .expect("expected a switch_session response");
        let state_resp = frames
            .iter()
            .skip(switch_idx + 1)
            .find(|f| f["type"] == "response" && f["command"] == "get_state")
            .expect("expected a get_state response after the switch");
        assert_eq!(state_resp["success"], true, "frames: {frames:#?}");
        assert_eq!(
            state_resp["data"]["sessionId"], src_id,
            "post-switch get_state must report the new session id"
        );

        handle.await.unwrap().unwrap();
    }
}
