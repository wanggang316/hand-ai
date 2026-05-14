//! Per-session resource pools shared across providers.
//!
//! Providers stash session-scoped state here (today, idle WebSocket
//! connections used by the OpenAI Codex Responses provider) and the
//! controller releases it when a session ends.
//!
//! The current implementation keeps things concrete: a single typed pool
//! keyed by `(session_id, transport)` is enough for the WebSocket-cached
//! transport that motivated the module. As more session-scoped resources
//! land we can generalize without churning every call site.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::types::Transport;

/// Handle to a pooled WebSocket connection.
///
/// The current implementation is intentionally a stub: it carries the
/// last-used timestamp so callers can implement TTL eviction, but the
/// underlying socket is not stored yet because the WebSocket transport is
/// itself a follow-up (see `openai_codex_responses.rs`). Once the
/// transport lands we'll thread a `tokio_tungstenite::WebSocketStream`
/// through here.
#[derive(Debug)]
pub struct WebSocketHandle {
    /// Unix-millis timestamp of the last successful use. Used by callers
    /// to implement an idle-timeout sweep.
    pub last_used_ms: u64,
}

impl WebSocketHandle {
    /// Build a fresh handle stamped with the current wall-clock.
    pub fn new() -> Self {
        Self {
            last_used_ms: now_ms(),
        }
    }
}

impl Default for WebSocketHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Pool of per-session resources. Cheap to clone via `Arc` from providers
/// and the session/controller layer.
#[derive(Debug, Default)]
pub struct SessionResources {
    websocket_pool: Mutex<HashMap<(String, Transport), WebSocketHandle>>,
}

impl SessionResources {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap `self` in `Arc` for handing to providers.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Acquire a WebSocket handle for `session_id`. If a cached idle
    /// handle exists it is returned; otherwise a fresh handle is minted.
    ///
    /// The current stub never errors. The signature is fallible to leave
    /// room for future connection-establishment logic without breaking
    /// callers.
    pub async fn acquire_websocket(
        &self,
        session_id: &str,
        transport: Transport,
    ) -> Result<WebSocketHandle, SessionResourceError> {
        let mut guard = self.websocket_pool.lock().await;
        let key = (session_id.to_string(), transport);
        if let Some(mut handle) = guard.remove(&key) {
            handle.last_used_ms = now_ms();
            return Ok(handle);
        }
        Ok(WebSocketHandle::new())
    }

    /// Return `handle` to the pool keyed by `(session_id, transport)`.
    pub async fn release_websocket(
        &self,
        session_id: &str,
        transport: Transport,
        mut handle: WebSocketHandle,
    ) {
        handle.last_used_ms = now_ms();
        let mut guard = self.websocket_pool.lock().await;
        guard.insert((session_id.to_string(), transport), handle);
    }

    /// Drop all cached handles for `session_id`. Called when a session ends.
    pub async fn cleanup(&self, session_id: &str) {
        let mut guard = self.websocket_pool.lock().await;
        guard.retain(|(sid, _), _| sid != session_id);
    }
}

/// Errors raised by [`SessionResources`].
#[derive(Debug, thiserror::Error)]
pub enum SessionResourceError {
    /// Underlying WebSocket transport unavailable (placeholder for when
    /// the WebSocket transport lands).
    #[error("websocket transport unavailable: {0}")]
    Unavailable(String),
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_then_release_round_trips() {
        let pool = SessionResources::new();
        let h = pool
            .acquire_websocket("sess_a", Transport::Websocket)
            .await
            .expect("acquire");
        pool.release_websocket("sess_a", Transport::Websocket, h)
            .await;

        let h2 = pool
            .acquire_websocket("sess_a", Transport::Websocket)
            .await
            .expect("re-acquire");
        // Released handle should be reused (last_used_ms is monotonic-ish).
        assert!(h2.last_used_ms > 0);
    }

    #[tokio::test]
    async fn distinct_keys_for_distinct_transports() {
        let pool = SessionResources::new();
        let ws = pool
            .acquire_websocket("sess_a", Transport::Websocket)
            .await
            .unwrap();
        let cached = pool
            .acquire_websocket("sess_a", Transport::WebsocketCached)
            .await
            .unwrap();
        pool.release_websocket("sess_a", Transport::Websocket, ws)
            .await;
        pool.release_websocket("sess_a", Transport::WebsocketCached, cached)
            .await;

        let guard = pool.websocket_pool.lock().await;
        assert_eq!(guard.len(), 2, "transport keys must not collide");
    }

    #[tokio::test]
    async fn cleanup_drops_session_entries() {
        let pool = SessionResources::new();
        let h = pool
            .acquire_websocket("sess_a", Transport::Websocket)
            .await
            .unwrap();
        pool.release_websocket("sess_a", Transport::Websocket, h)
            .await;
        pool.cleanup("sess_a").await;

        let guard = pool.websocket_pool.lock().await;
        assert!(guard.is_empty());
    }
}
