//! `POST /upload` — out-of-band attachment upload.
//!
//! Accepts attachment bytes either as a `multipart/form-data` body (the first
//! file field, carrying the browser's filename + content-type) or as a raw
//! request body (any other content type, with optional `x-file-name` /
//! `Content-Type` headers). The bytes are stored in the shared [`BlobStore`]
//! under a content-hash id and the id + byte length are returned as JSON. The
//! browser embeds that id in the subsequent `prompt` frame instead of inlining
//! large base64, keeping WebSocket frames small.
//!
//! The whole request body is read once via [`Bytes`]; multipart bodies are then
//! parsed with `multer`. A single body extractor is used because axum forbids
//! combining two body-consuming extractors in one handler.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use serde::Serialize;
use std::sync::Arc;

use crate::app::AppState;

/// Maximum accepted upload size: 50 MB, matching the document extractor cap.
pub const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

/// JSON response: the content id and stored byte length.
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub id: String,
    pub size: usize,
}

/// Handle `POST /upload`. Dispatches on the request content type: multipart
/// bodies are parsed field-by-field; anything else is treated as a raw body.
pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    if body.len() > MAX_UPLOAD_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload too large".to_string(),
        ));
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Ok(boundary) = multer::parse_boundary(content_type) {
        return upload_multipart(&state, body, boundary).await;
    }

    // Raw body upload: filename / content-type come from optional headers.
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty upload".to_string()));
    }
    let file_name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("attachment");
    let raw_content_type = if content_type.is_empty() {
        "application/octet-stream"
    } else {
        content_type
    };
    store_bytes(&state, &body, file_name, raw_content_type)
}

/// Parse a multipart body, taking the first non-empty file part (one carrying a
/// filename). Plain form fields (no filename) are skipped, not stored.
async fn upload_multipart(
    state: &Arc<AppState>,
    body: Bytes,
    boundary: String,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    let stream = futures::stream::once(async move { Ok::<Bytes, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid multipart: {e}")))?
    {
        // Only true file parts (those carrying a filename) are stored; a plain
        // form field without a filename is not an uploaded file and must not
        // coalesce into a blob. Drain and skip it. (A request with no file part
        // falls through to the `no file field in upload` 400 below.)
        let Some(file_name) = field.file_name().map(str::to_string) else {
            let _ = field.bytes().await;
            continue;
        };
        let content_type = field
            .content_type()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let data = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid field bytes: {e}")))?;
        if data.is_empty() {
            continue;
        }
        if data.len() > MAX_UPLOAD_BYTES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload too large".to_string(),
            ));
        }
        return store_bytes(state, &data, &file_name, &content_type);
    }
    Err((
        StatusCode::BAD_REQUEST,
        "no file field in upload".to_string(),
    ))
}

/// Persist bytes in the shared store and build the JSON response.
fn store_bytes(
    state: &Arc<AppState>,
    bytes: &[u8],
    file_name: &str,
    content_type: &str,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    match state.blobs.put_upload(bytes, file_name, content_type) {
        Ok((id, size)) => Ok(Json(UploadResponse { id, size })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store upload: {e}"),
        )),
    }
}
