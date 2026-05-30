//! `GET /download/:id` and `POST /download/register` — out-of-band download.
//!
//! `register` maps a server-produced file path (an `export_html` output under
//! the session cwd) onto a fresh download id; `GET /download/:id` then streams
//! the bytes back with `Content-Disposition: attachment` so the browser saves
//! the file. Registration is the safety gate: only a file that resolves inside
//! the session cwd can be registered, so an arbitrary path can never be served.
//!
//! Uploaded attachment blobs share the same id namespace, so `GET /download/:id`
//! also serves anything previously written by `POST /upload` (used by tests and
//! by any future "re-download my attachment" affordance).

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::path::{Component, PathBuf};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use crate::app::AppState;

/// Body of `POST /download/register`: the server-side path to expose.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    /// Absolute or cwd-relative path the server produced (e.g. an export file).
    pub path: String,
}

/// Response of `POST /download/register`: the id to fetch via `GET /download/:id`.
#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub id: String,
}

/// Register a server-produced file for download. The path MUST resolve to an
/// existing file inside the session cwd, which is the guarantee that only files
/// the server itself produced are ever served.
pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let requested = PathBuf::from(&req.path);
    let abs = if requested.is_absolute() {
        requested
    } else {
        state.cwd.join(requested)
    };

    // Containment is checked LEXICALLY first (no filesystem I/O), so a path that
    // escapes the cwd is rejected as 403 whether or not the target exists —
    // canonicalize would otherwise turn a non-existent escaping path into a 404,
    // masking the traversal. Both sides are normalized in the same (non-resolved)
    // space so this is unaffected by symlinked cwд roots (e.g. macOS /var ->
    // /private/var, where mixing canonical and non-canonical would misfire).
    let lex_cwd = lexical_normalize(&state.cwd);
    if !lexical_normalize(&abs).starts_with(&lex_cwd) {
        return Err((
            StatusCode::FORBIDDEN,
            "path is outside the session directory".to_string(),
        ));
    }

    // Now resolve symlinks and re-check containment (a symlink INSIDE the cwd
    // could still point outside), then require an existing regular file.
    let cwd = tokio::fs::canonicalize(&state.cwd)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("cwd error: {e}")))?;
    let canonical = tokio::fs::canonicalize(&abs)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "file not found".to_string()))?;
    if !canonical.starts_with(&cwd) {
        return Err((
            StatusCode::FORBIDDEN,
            "path is outside the session directory".to_string(),
        ));
    }
    if !canonical.is_file() {
        return Err((StatusCode::NOT_FOUND, "not a file".to_string()));
    }

    let content_type = content_type_for(&canonical);
    let id = state.blobs.register_file(canonical, content_type);
    Ok(Json(RegisterResponse { id }))
}

/// Stream the bytes stored under `id`. 404 on unknown id or missing file.
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    let blob = state
        .blobs
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "unknown download id".to_string()))?;

    let file = tokio::fs::File::open(&blob.path).await.map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            "download contents missing".to_string(),
        )
    })?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let disposition = format!(
        "attachment; filename=\"{}\"",
        blob.file_name.replace('"', "")
    );
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, blob.content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

/// Lexically normalize a path WITHOUT touching the filesystem: `.` is dropped
/// and `..` pops the previous component (never above the root). No symlink
/// resolution. Used to detect `..`-traversal before any I/O, so an escaping path
/// that does not exist is still classified as out-of-cwd rather than "not
/// found". Both the candidate and the cwd are passed through this so the
/// containment check is consistent regardless of symlinked roots.
fn lexical_normalize(path: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

/// Best-effort content type from a file extension. Defaults to a generic binary
/// type so the browser downloads rather than renders.
fn content_type_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::BlobStore;

    #[tokio::test]
    async fn download_unknown_id_is_404() {
        let state = Arc::new(AppState {
            cwd: std::env::temp_dir(),
            model: "test/model".to_string(),
            provider: None,
            web_dir: None,
            blobs: BlobStore::new(),
        });
        let err = download(State(state), Path("missing".to_string()))
            .await
            .expect_err("unknown id must 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn upload_then_download_round_trips() {
        let state = Arc::new(AppState {
            cwd: std::env::temp_dir(),
            model: "test/model".to_string(),
            provider: None,
            web_dir: None,
            blobs: BlobStore::new(),
        });
        let bytes = b"round-trip payload";
        let (id, _) = state
            .blobs
            .put_upload(bytes, "f.bin", "application/octet-stream")
            .unwrap();

        let resp = download(State(state), Path(id))
            .await
            .expect("download should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let collected = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body should collect");
        assert_eq!(&collected[..], bytes);
    }

    #[tokio::test]
    async fn register_rejects_path_outside_cwd() {
        let dir = std::env::temp_dir().join(format!("hand-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = Arc::new(AppState {
            cwd: dir.clone(),
            model: "test/model".to_string(),
            provider: None,
            web_dir: None,
            blobs: BlobStore::new(),
        });
        // A path that escapes the cwd must be rejected as FORBIDDEN (the lexical
        // containment check fires before any filesystem lookup), never 404 and
        // never registered.
        let err = register(
            State(state),
            Json(RegisterRequest {
                path: "../../../../etc/hosts".to_string(),
            }),
        )
        .await
        .expect_err("out-of-cwd path must not register");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn register_traversal_to_nonexistent_is_403_not_404() {
        // A `..`-traversal whose target does not exist must still be classified
        // as out-of-cwd (403) — existence is not checked before containment.
        let dir = std::env::temp_dir().join(format!("hand-dl-trav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = Arc::new(AppState {
            cwd: dir.clone(),
            model: "test/model".to_string(),
            provider: None,
            web_dir: None,
            blobs: BlobStore::new(),
        });
        let err = register(
            State(state),
            Json(RegisterRequest {
                path: "../../../../nonexistent-xyzzy/secret.txt".to_string(),
            }),
        )
        .await
        .expect_err("escaping traversal must not register");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn register_then_download_serves_cwd_file() {
        let dir = std::env::temp_dir().join(format!("hand-dl-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("export.html");
        std::fs::write(&file, b"<html>export</html>").unwrap();
        let state = Arc::new(AppState {
            cwd: dir.clone(),
            model: "test/model".to_string(),
            provider: None,
            web_dir: None,
            blobs: BlobStore::new(),
        });

        let reg = register(
            State(state.clone()),
            Json(RegisterRequest {
                path: "export.html".to_string(),
            }),
        )
        .await
        .expect("registration of a cwd file should succeed");

        let resp = download(State(state), Path(reg.id.clone()))
            .await
            .expect("download of registered file should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        let collected = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&collected[..], b"<html>export</html>");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
