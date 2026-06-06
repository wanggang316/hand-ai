//! WebSocket <-> RPC dispatcher bridge.
//!
//! The existing [`hand_coding_agent::rpc::run_rpc_server`] dispatcher speaks a
//! newline-delimited JSON (JSONL) protocol over any `AsyncBufRead` + `AsyncWrite`
//! pair. This module adapts a browser WebSocket onto that exact protocol so the
//! command dispatch, event forwarding, and interrupt races are reused unchanged:
//!
//! ```text
//!   browser --text frame--> cmd_w | cmd_r --> run_rpc_server (reader)
//!   run_rpc_server (writer) --> evt_w | evt_r --line--> browser (text frame)
//! ```
//!
//! Each inbound WebSocket text frame carries exactly one JSON command; a `\n`
//! is appended so the JSONL reader frames it correctly. Each JSONL line the
//! dispatcher writes becomes exactly one outbound text frame.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use model::{TextContent, ToolResultContent};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::app::AppState;

/// A `tool_result` frame sent by the browser after executing a browser tool.
///
/// `content` is an array of text/image blocks (matching the client wire shape);
/// text parts are concatenated into the [`ToolResult`](hand_agent::types::ToolResult)
/// returned to the suspended tool closure.
#[derive(Debug, Deserialize)]
struct ToolResultFrame {
    #[serde(rename = "toolCallId")]
    tool_call_id: String,
    content: Vec<ToolResultBlock>,
    #[serde(rename = "isError", default)]
    is_error: bool,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ToolResultBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

/// Cheap pre-check: does this frame's `type` field equal `"tool_result"`?
///
/// Parses just enough to read the discriminator so non-tool-result commands are
/// forwarded verbatim without a full structural deserialize.
fn is_tool_result_frame(text: &str) -> bool {
    #[derive(Deserialize)]
    struct TypeOnly<'a> {
        #[serde(rename = "type", borrow)]
        type_field: Option<&'a str>,
    }
    serde_json::from_str::<TypeOnly>(text)
        .ok()
        .and_then(|t| t.type_field)
        == Some("tool_result")
}

/// Build a [`ToolResult`](hand_agent::types::ToolResult) from a browser frame by
/// concatenating its text blocks. The `is_error` flag is surfaced via the
/// `details` payload so the dispatcher's existing error-handling stays untouched;
/// the agent loop already derives `is_error` for tool results from the closure
/// outcome, so the concatenated text doubles as the error message.
fn frame_to_tool_result(frame: ToolResultFrame) -> hand_agent::types::ToolResult {
    let text = frame
        .content
        .iter()
        .filter(|b| b.block_type == "text")
        .filter_map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("");

    let mut result = hand_agent::types::ToolResult {
        content: vec![ToolResultContent::Text(TextContent::new(text))],
        details: frame.details,
        terminate: None,
    };
    // Preserve the browser's error signal in `details` for UI/logging without
    // changing the content the model sees.
    if frame.is_error {
        let mut details = result.details.take().and_then(|d| match d {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        });
        let map = details.get_or_insert_with(serde_json::Map::new);
        map.insert("isError".to_string(), serde_json::Value::Bool(true));
        result.details = Some(serde_json::Value::Object(map.clone()));
    }
    result
}

/// Upgrade the connection and hand it a dedicated session task.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (session, hub) = crate::session::build_session(&state);

    // In-memory pipes bridging the socket to the JSONL dispatcher.
    let (cmd_w, cmd_r) = tokio::io::duplex(64 * 1024);
    let (evt_w, evt_r) = tokio::io::duplex(64 * 1024);

    // Reuse the existing dispatcher wholesale; only the transport changes.
    let dispatcher = tokio::spawn(hand_coding_agent::rpc::run_rpc_server(
        BufReader::new(cmd_r),
        evt_w,
        session,
    ));

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Browser -> dispatcher: one text frame == one JSONL command line, EXCEPT
    // `tool_result` frames, which are browser-tool replies. Those are routed to
    // the per-connection hub to unblock the suspended tool closure and are NOT
    // forwarded to the dispatcher (which never models `tool_result` commands).
    let mut cmd_w = cmd_w;
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    if is_tool_result_frame(&text) {
                        match serde_json::from_str::<ToolResultFrame>(&text) {
                            Ok(frame) => {
                                let tool_call_id = frame.tool_call_id.clone();
                                hub.resolve(&tool_call_id, frame_to_tool_result(frame));
                            }
                            Err(err) => {
                                tracing::warn!(%err, "dropping malformed tool_result frame");
                            }
                        }
                        continue;
                    }
                    if cmd_w.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                    if cmd_w.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        // Closing the writer signals EOF, so the dispatcher exits cleanly.
        let _ = cmd_w.shutdown().await;
    });

    // Dispatcher -> browser: one JSONL line == one outbound text frame.
    let outbound = tokio::spawn(async move {
        let mut lines = BufReader::new(evt_r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if ws_tx.send(Message::Text(line)).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.send(Message::Close(None)).await;
    });

    // Closing any leg cascades EOF to the others; wait for all to settle.
    let _ = tokio::join!(inbound, outbound, dispatcher);
}
