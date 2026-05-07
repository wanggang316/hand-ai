//! RPC dispatcher: bridges JSONL stdin/stdout to an [`AgentSession`].
//!
//! Reads [`RpcCommand`] frames from a JSONL input stream, routes each to a
//! handler on an owned [`AgentSession`], and forwards the session's events
//! back out to the JSONL output stream. The dispatcher exits cleanly when
//! the input stream ends.
//!
//! # Phase 1 scope
//!
//! Only five commands are dispatched: `prompt`, `abort`, `new_session`,
//! `get_state`, `get_messages`. Everything else replies with
//! `{success: false, error: "not implemented in Phase 1"}`. `abort` is in
//! that list for now: the underlying [`AgentSession`] does not yet expose
//! an abort hook, so the variant is reported as not implemented (TODO at
//! the call site below — wire to `hand_agent::AgentLoopConfig`'s abort
//! signal in a follow-up).
//!
//! # Concurrency model
//!
//! Single-task. Commands are processed sequentially on the dispatcher's
//! task; while a `prompt` is driving a turn through the agent loop, the
//! dispatcher does not pull the next command. Events stream out in real
//! time because they are emitted by the session's subscribe callback,
//! which forwards through an `mpsc` channel to a separate writer task.
//!
//! The TS port (`pi-coding-agent/src/modes/rpc/rpc-mode.ts`) is
//! multitasking: it parks the in-flight prompt as a Promise and continues
//! reading commands so that `abort`/`get_state`/etc. can interrupt a
//! turn. Porting that requires a thread-safe abort path on
//! [`AgentSession`] which we do not have in Phase 1 — see brief.

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::rpc::jsonl::{JsonlReadError, read_jsonl, write_jsonl};
use crate::rpc::types::{
    CommandsData, ForkData, ForkMessagesData, LastAssistantTextData, MessagesData, NewSessionData,
    QueueMode, RpcCommand, RpcResponse, RpcResponseBody, RpcResultEmpty, RpcResultWithData,
    RpcSessionState, SwitchSessionData,
};
use futures::StreamExt;
use hand_agent::types::AgentEvent;
use model::types::ThinkingLevel;
use serde::Serialize;
use std::io;
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
    Agent(AgentEvent),
    CompactionStart,
    CompactionEnd { summary: String },
    Error { message: String },
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
    while let Some(item) = stream.next().await {
        match item {
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
        RpcCommand::Prompt { id, message, .. } => {
            // Per the brief: emit success once the prompt is queued. In
            // this single-task model the await runs the whole turn
            // synchronously before this match returns, so by the time we
            // reply the turn has already completed. Events arrive on the
            // outbound channel before the response — the writer
            // serializes all frames in send order, so consumers see the
            // event stream first and the success last.
            match session.send_message(&message).await {
                Ok(_) => RpcResponse::new(
                    id,
                    RpcResponseBody::Prompt(RpcResultEmpty::ok()),
                ),
                Err(e) => RpcResponse::new(
                    id,
                    RpcResponseBody::Prompt(RpcResultEmpty::err(format!(
                        "prompt failed: {e}"
                    ))),
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
            // NOTE: this path does not currently honor `parent_session`;
            // session forking lives in a later phase. The brief
            // explicitly lists it as out-of-scope context.
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
            RpcResponse::new(
                id,
                RpcResponseBody::GetState(RpcResultWithData::ok(state)),
            )
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

        // ---- Out-of-scope ----------------------------------------------
        //
        // Each variant returns the same "not implemented in Phase 1"
        // failure but on its own discriminator (the wire `command` field
        // must echo the request's). The branches are tedious but typed
        // — `RpcResponseBody`'s per-command variant is non-uniform
        // (some carry `RpcResultEmpty`, some `RpcResultWithData<T>`),
        // so we cannot compress them into a single `not_implemented(id,
        // kind)` helper without losing type safety.
        //
        // TODO(rpc-server): wire `Abort` to `AgentLoopConfig`'s abort
        // signal once `AgentSession` exposes an abort hook. See the
        // `packages/agent/src/agent_loop.rs` abort mechanism.
        RpcCommand::Abort { id } => not_impl_empty(id, RpcResponseBody::Abort),
        RpcCommand::Steer { id, .. } => not_impl_empty(id, RpcResponseBody::Steer),
        RpcCommand::FollowUp { id, .. } => not_impl_empty(id, RpcResponseBody::FollowUp),
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
                                // per-project) override. Phase 1 has no such
                                // override surface; report `false` until the
                                // settings port lands.
                                is_scoped: false,
                            },
                        ))),
                    )
                }
                None => RpcResponse::new(
                    id,
                    RpcResponseBody::CycleModel(RpcResultWithData::ok(None)),
                ),
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
        RpcCommand::SetThinkingLevel { id, .. } => {
            not_impl_empty(id, RpcResponseBody::SetThinkingLevel)
        }
        RpcCommand::CycleThinkingLevel { id } => {
            not_impl_data(id, RpcResponseBody::CycleThinkingLevel)
        }
        RpcCommand::SetSteeringMode { id, .. } => {
            not_impl_empty(id, RpcResponseBody::SetSteeringMode)
        }
        RpcCommand::SetFollowUpMode { id, .. } => {
            not_impl_empty(id, RpcResponseBody::SetFollowUpMode)
        }
        RpcCommand::Compact { id, .. } => not_impl_data(id, RpcResponseBody::Compact),
        RpcCommand::SetAutoCompaction { id, .. } => {
            not_impl_empty(id, RpcResponseBody::SetAutoCompaction)
        }
        RpcCommand::SetAutoRetry { id, .. } => not_impl_empty(id, RpcResponseBody::SetAutoRetry),
        RpcCommand::AbortRetry { id } => not_impl_empty(id, RpcResponseBody::AbortRetry),
        RpcCommand::Bash { id, .. } => not_impl_data(id, RpcResponseBody::Bash),
        RpcCommand::AbortBash { id } => not_impl_empty(id, RpcResponseBody::AbortBash),
        RpcCommand::GetSessionStats { id } => {
            not_impl_data(id, RpcResponseBody::GetSessionStats)
        }
        RpcCommand::ExportHtml { id, .. } => not_impl_data(id, RpcResponseBody::ExportHtml),
        RpcCommand::SwitchSession { id, .. } => RpcResponse::new(
            id,
            RpcResponseBody::SwitchSession(RpcResultWithData::<SwitchSessionData>::err(
                NOT_IMPLEMENTED,
            )),
        ),
        RpcCommand::Fork { id, .. } => RpcResponse::new(
            id,
            RpcResponseBody::Fork(RpcResultWithData::<ForkData>::err(NOT_IMPLEMENTED)),
        ),
        RpcCommand::Clone { id } => not_impl_data(id, RpcResponseBody::Clone),
        RpcCommand::GetForkMessages { id } => RpcResponse::new(
            id,
            RpcResponseBody::GetForkMessages(RpcResultWithData::<ForkMessagesData>::err(
                NOT_IMPLEMENTED,
            )),
        ),
        RpcCommand::GetLastAssistantText { id } => RpcResponse::new(
            id,
            RpcResponseBody::GetLastAssistantText(RpcResultWithData::<LastAssistantTextData>::err(
                NOT_IMPLEMENTED,
            )),
        ),
        RpcCommand::SetSessionName { id, .. } => {
            not_impl_empty(id, RpcResponseBody::SetSessionName)
        }
        RpcCommand::GetCommands { id } => RpcResponse::new(
            id,
            RpcResponseBody::GetCommands(RpcResultWithData::<CommandsData>::err(NOT_IMPLEMENTED)),
        ),
    }
}

/// Canonical "not implemented" error string. Matches the brief verbatim
/// so the round-trip tests can assert against it.
const NOT_IMPLEMENTED: &str = "not implemented in Phase 1";

fn not_impl_empty(
    id: Option<String>,
    wrap: impl FnOnce(RpcResultEmpty) -> RpcResponseBody,
) -> RpcResponse {
    RpcResponse::new(id, wrap(RpcResultEmpty::err(NOT_IMPLEMENTED)))
}

/// Helper for variants whose Failure arm is `RpcResultWithData<T>::Failure`.
/// `T` is inferred from the wrapping variant; the closure picks one.
fn not_impl_data<T>(
    id: Option<String>,
    wrap: impl FnOnce(RpcResultWithData<T>) -> RpcResponseBody,
) -> RpcResponse {
    RpcResponse::new(id, wrap(RpcResultWithData::<T>::err(NOT_IMPLEMENTED)))
}

/// Build a snapshot of the session for `get_state`.
///
/// TODOs (each tracks a Rust subsystem that does not yet exist; the wire
/// shape here uses defaults so consumers can still parse a `RpcSessionState`):
///
/// - `is_streaming`: tracked by the dispatcher in TS via `pendingPrompt`
///   state; needs an `AgentSession::is_streaming()` accessor (owner: core).
/// - `is_compacting`: needs a `compacting` flag on `AgentSession`
///   (owner: core/compaction).
/// - `pending_message_count`: needs a steer/follow-up queue on the
///   session (owner: core/queue, parity port pending).
/// - `auto_compaction_enabled`: needs settings exposure
///   (owner: core/settings).
/// - `steering_mode` / `follow_up_mode`: hard-coded to `OneAtATime` to
///   match `AgentLoopConfig` defaults; needs accessors when those are
///   made mutable (owner: core/agent_session).
/// - `session_file`: not exposed on `SessionManager` (owner: core/session).
/// - `thinking_level`: derived from `stream_options().reasoning` if set,
///   else defaults to `Medium` (the TS default; owner: core/settings).
fn build_session_state(session: &AgentSession) -> RpcSessionState {
    let model_value = serde_json::to_value(session.model()).ok();
    let thinking_level = session
        .stream_options()
        .reasoning
        .unwrap_or(ThinkingLevel::Medium);
    RpcSessionState {
        model: model_value,
        thinking_level,
        is_streaming: false,
        is_compacting: false,
        steering_mode: QueueMode::OneAtATime,
        follow_up_mode: QueueMode::OneAtATime,
        session_file: None,
        session_id: session.session_id().to_string(),
        session_name: session.label().map(str::to_string),
        auto_compaction_enabled: true,
        message_count: session.message_count() as u64,
        pending_message_count: 0,
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
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("ignored")).await;

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
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("ignored")).await;

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
        assert!(messages.is_empty(), "messages must be empty on fresh session");

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn prompt_emits_assistant_text_event_then_success_response() {
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("hello")).await;

        in_tx
            .write_all(b"{\"type\":\"prompt\",\"message\":\"hi\",\"id\":\"42\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert!(
            !frames.is_empty(),
            "expected at least one frame, got none"
        );

        // The success response must appear (last response with the right id).
        let response = frames
            .iter()
            .find(|f| f["type"] == "response")
            .expect("expected a response frame");
        assert_eq!(response["command"], "prompt");
        assert_eq!(response["success"], true);
        assert_eq!(response["id"], "42");

        // At least one event frame must carry the assistant text "hello".
        let saw_text = frames.iter().any(|f| {
            f["type"] == "event" && f.to_string().contains("\"hello\"")
        });
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
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("ignored")).await;

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
    async fn out_of_scope_variant_returns_not_implemented_error() {
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("ignored")).await;

        in_tx
            .write_all(b"{\"type\":\"steer\",\"message\":\"x\",\"id\":\"5\"}\n")
            .await
            .unwrap();
        drop(in_tx);

        let frames = drain_frames(out_rx).await;
        assert_eq!(frames.len(), 1);
        let resp = &frames[0];
        assert_eq!(resp["command"], "steer");
        assert_eq!(resp["success"], false);
        assert_eq!(resp["error"], "not implemented in Phase 1");

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
        let (mut in_tx, out_rx, handle) =
            spawn_dispatcher(session_with_mock("ignored")).await;

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
}
