//! Embedded frontend asset serving (single-binary packaging).
//!
//! `web/dist/**` (the built Vite bundle) is compiled into the binary via
//! [`rust-embed`] so a release build is fully self-contained and runnable
//! from any working directory. In debug builds `rust-embed` reads the files
//! from disk at runtime, so an incremental frontend rebuild is picked up
//! without recompiling the server.
//!
//! Serving strategy (see `app.rs` for selection):
//!
//! - When `--web-dir <path>` points at an existing directory, the router
//!   serves from disk via `ServeDir` (dev convenience / explicit override).
//! - Otherwise the embedded assets here are served: an exact path lookup
//!   with a `mime_guess` content type, falling back to `index.html` for `/`
//!   and unknown (SPA) routes. If the embed is somehow empty (e.g. the
//!   frontend was never built), a minimal inline page is served instead.

use axum::body::Body;
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

/// The built frontend bundle, embedded at compile time (release) or read
/// from disk at runtime (debug).
#[derive(RustEmbed)]
#[folder = "web/dist"]
pub struct Assets;

/// Minimal inline page used only when the embedded bundle is empty (the
/// frontend was never built). Mirrors the connectivity-probe page in
/// `app.rs` so a bare server still demonstrates the streaming seam.
const EMPTY_INDEX: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>hand web ui</title></head>
<body style="font-family: ui-monospace, monospace; margin: 2rem;">
<h1>hand web ui</h1>
<p>Frontend bundle not embedded. Build it with <code>scripts/build-web-ui.sh</code>
or run the Vite dev server (see the crate README).</p>
</body>
</html>
"#;

/// Look `path` up in the embedded bundle and build an HTTP response with a
/// `mime_guess` content type. Returns `None` when the asset is absent.
fn serve_embedded(path: &str) -> Option<Response> {
    let asset = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(asset.data.into_owned());
    Some(([(header::CONTENT_TYPE, mime.as_ref().to_owned())], body).into_response())
}

/// Serve the embedded `index.html`, or the inline fallback if the bundle is
/// empty.
pub fn embedded_index() -> Response {
    match Assets::get("index.html") {
        Some(asset) => Html(asset.data.into_owned()).into_response(),
        None => Html(EMPTY_INDEX).into_response(),
    }
}

/// Axum fallback handler for the embedded-asset path. Resolves the request
/// URI against the embedded bundle; on a miss it serves `index.html` so the
/// SPA can handle client-side routes (and `/` itself).
pub async fn embedded_fallback(uri: Uri) -> Response {
    // Strip the leading slash so the path matches the embed's relative keys
    // (e.g. `assets/index-*.js`). An empty path means the document root.
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return embedded_index();
    }
    match serve_embedded(path) {
        Some(resp) => resp,
        // Unknown route: hand back index.html for SPA-style navigation. The
        // status stays 200 because the SPA, not the server, owns routing.
        None => {
            let mut resp = embedded_index();
            // A genuinely missing static asset (has a file extension) is a
            // 404; only extensionless paths fall through to the SPA shell.
            if path.contains('.') {
                *resp.status_mut() = StatusCode::NOT_FOUND;
            }
            resp
        }
    }
}
