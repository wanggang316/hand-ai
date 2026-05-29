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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::app::AppState;

/// Upgrade the connection and hand it a dedicated session task.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let session = crate::session::build_session(&state);

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

    // Browser -> dispatcher: one text frame == one JSONL command line.
    let mut cmd_w = cmd_w;
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
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
