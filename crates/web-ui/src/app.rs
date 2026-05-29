//! Router assembly and shared server state.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

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
    /// Directory holding the built frontend assets.
    pub web_dir: PathBuf,
}

/// Build the axum router: a `/ws` WebSocket upgrade, a health check, and
/// static asset serving for the built frontend. When the frontend has not
/// been built yet (e.g. running against the Vite dev server, or a bare
/// smoke test) `/` falls back to a minimal inline page that exercises the
/// streaming seam directly.
pub fn router(state: AppState) -> Router {
    let web_dir = state.web_dir.clone();
    let state = Arc::new(state);

    Router::new()
        .route("/ws", get(crate::ws::ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(index))
        .fallback_service(ServeDir::new(web_dir))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Serve the built `index.html` when present, else a minimal inline page.
async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    let index_path = state.web_dir.join("index.html");
    match tokio::fs::read_to_string(&index_path).await {
        Ok(html) => Html(html),
        Err(_) => Html(DEV_INDEX.to_string()),
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
    if (f.type === "event" && f.event && f.event.kind === "agent" && f.event.message) {
      const blocks = f.event.message.content || [];
      const text = blocks.filter(b => b.type === "text").map(b => b.text).join("");
      out.textContent = "[connected]\n" + text;
    } else {
      out.textContent += "\n" + e.data;
    }
  } catch { out.textContent += "\n" + e.data; }
};
document.getElementById("f").addEventListener("submit", (ev) => {
  ev.preventDefault();
  ws.send(JSON.stringify({ type: "prompt", id: "1", message: document.getElementById("m").value }));
});
</script>
</body>
</html>
"#;
