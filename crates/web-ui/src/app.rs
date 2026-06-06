//! Router assembly and shared server state.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::blob_store::BlobStore;
use crate::upload::MAX_UPLOAD_BYTES;

/// Per-server configuration used to construct one agent session per
/// WebSocket connection.
#[derive(Clone)]
pub struct AppState {
    /// Working directory for agent sessions and tool execution.
    pub cwd: PathBuf,
    /// Model id used for new sessions.
    pub model: String,
    /// Optional provider override for the model.
    pub provider: Option<String>,
    /// Directory holding the built frontend assets to serve from disk. When
    /// `None`, the frontend is served from the binary's embedded bundle
    /// (`crate::assets`) so a release build is fully self-contained.
    pub web_dir: Option<PathBuf>,
    /// Shared out-of-band blob store backing `/upload` and `/download`.
    pub blobs: BlobStore,
}

/// Build the axum router: a `/ws` WebSocket upgrade, a health check, and
/// static asset serving for the built frontend.
///
/// Asset serving has two modes, chosen by `state.web_dir`:
///
/// - **Disk** (`Some(dir)` and `dir` exists): served via `ServeDir`, with `/`
///   resolved by the [`index`] handler reading `dir/index.html`. This is the
///   dev convenience / explicit-override path.
/// - **Embedded** (`None`, or the directory is missing): served from the
///   binary's compiled-in bundle (`crate::assets`). A release build with no
///   `--web-dir` is therefore fully self-contained.
///
/// In either mode, when the bundle is unavailable (`index.html` missing on
/// disk, or the embed is empty) `/` falls back to a minimal inline page that
/// exercises the streaming seam directly.
pub fn router(state: AppState) -> Router {
    // Resolve the serving mode once at startup. A `--web-dir` that does not
    // exist falls back to the embedded bundle rather than serving 404s.
    let disk_dir = state.web_dir.as_ref().filter(|dir| dir.is_dir()).cloned();
    let state = Arc::new(state);

    let base = Router::new()
        .route("/ws", get(crate::ws::ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        // Out-of-band attachment upload + artifact download (see §5.4). The
        // body limit covers both the multipart and raw-body upload paths.
        .route(
            "/upload",
            post(crate::upload::upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/download/register", post(crate::download::register))
        .route("/download/:id", get(crate::download::download));

    let routed = match disk_dir {
        Some(dir) => base
            .route("/", get(index))
            .fallback_service(ServeDir::new(dir)),
        None => base.fallback(crate::assets::embedded_fallback),
    };

    routed.with_state(state).layer(TraceLayer::new_for_http())
}

/// Serve the built `index.html` from disk when present, else a minimal inline
/// page. Only reachable in the disk-serving mode (a configured `--web-dir`).
async fn index(State(state): State<Arc<AppState>>) -> Response {
    let Some(dir) = state.web_dir.as_ref() else {
        return Html(DEV_INDEX.to_string()).into_response();
    };
    match tokio::fs::read_to_string(dir.join("index.html")).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => Html(DEV_INDEX.to_string()).into_response(),
    }
}

/// Minimal fallback page: connects to `/ws`, sends one prompt, and appends
/// every inbound frame to the page. It speaks the raw JSONL-over-WebSocket
/// protocol and is only used until the real frontend bundle exists.
const DEV_INDEX: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>hand web ui</title></head>
<body style="font-family: ui-monospace, monospace; margin: 2rem;">
<h1>hand web ui</h1>
<p>Frontend bundle not found. This is the built-in connectivity probe.</p>
<form id="f"><input id="m" size="60" value="Say hello in one short sentence."><button>Send</button></form>
<pre id="out" style="white-space: pre-wrap;"></pre>
<script>
const out = document.getElementById("out");
const ws = new WebSocket((location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws");
ws.onopen = () => out.textContent += "[connected]\n";
ws.onclose = () => out.textContent += "\n[closed]\n";
ws.onmessage = (e) => {
  try {
    const f = JSON.parse(e.data);
    if (f.type === "event" && f.event && f.event.kind === "agent") {
      const m = f.event.message;
      // Streaming assistant content is a block array; user-message echoes
      // (message_start/message_end) carry a plain string and are skipped.
      if (m && Array.isArray(m.content)) {
        const text = m.content.filter(b => b.type === "text").map(b => b.text).join("");
        if (text) out.textContent = "[connected]\n" + text;
      }
    }
  } catch { /* ignore non-JSON frames */ }
};
document.getElementById("f").addEventListener("submit", (ev) => {
  ev.preventDefault();
  ws.send(JSON.stringify({ type: "prompt", id: "1", message: document.getElementById("m").value }));
});
</script>
</body>
</html>
"#;
