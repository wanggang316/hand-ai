//! Shared out-of-band blob store backing `POST /upload` and `GET /download/:id`.
//!
//! Two kinds of entries share one id namespace:
//!
//! - **Uploaded** attachment bytes: written by `/upload`, materialized into a
//!   per-process temp directory and keyed by their content hash so identical
//!   uploads coalesce. The browser embeds the returned id in the subsequent
//!   `prompt` frame (out-of-band, to keep WebSocket frames small).
//! - **Server-produced files**: the `export_html` RPC writes an HTML file to the
//!   session cwd and returns its path. `/download/register` validates that the
//!   path lives under the session cwd and maps a fresh id onto it so the browser
//!   can fetch it via `/download/:id`. Only files the server itself produced are
//!   ever served — registration is the safety gate, never an arbitrary path.
//!
//! The store hands `/download` a [`Blob`] describing where the bytes live and a
//! suggested filename + content-type for the response headers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A stored blob: either uploaded bytes on disk, or a registered server file.
#[derive(Debug, Clone)]
pub struct Blob {
    /// Absolute path to the bytes on disk.
    pub path: PathBuf,
    /// Filename suggested to the browser via `Content-Disposition`.
    pub file_name: String,
    /// MIME type for the `Content-Type` header.
    pub content_type: String,
}

/// Thread-safe id -> [`Blob`] map, cheap to clone (shared `Arc`).
#[derive(Clone, Default)]
pub struct BlobStore {
    inner: Arc<Mutex<HashMap<String, Blob>>>,
    /// Per-process temp directory holding uploaded attachment bytes. Created on
    /// first upload and cleaned up when the process exits.
    upload_dir: Arc<Mutex<Option<PathBuf>>>,
}

impl BlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store raw uploaded bytes under a content-hash id. Identical content
    /// coalesces onto the same id (and the same on-disk file). Returns the id
    /// and the byte length.
    pub fn put_upload(
        &self,
        bytes: &[u8],
        file_name: &str,
        content_type: &str,
    ) -> std::io::Result<(String, usize)> {
        let id = sha256_hex(bytes);
        let dir = self.ensure_upload_dir()?;
        let path = dir.join(&id);
        // Idempotent: skip the write if this exact content already landed.
        if !path.exists() {
            std::fs::write(&path, bytes)?;
        }
        let blob = Blob {
            path,
            file_name: sanitize_file_name(file_name),
            content_type: content_type.to_string(),
        };
        self.inner.lock().unwrap().insert(id.clone(), blob);
        Ok((id, bytes.len()))
    }

    /// Register a server-produced file under a fresh id. The caller MUST have
    /// validated that `path` is a file the server itself produced (e.g. an
    /// `export_html` output under the session cwd). Returns the new id.
    pub fn register_file(&self, path: PathBuf, content_type: &str) -> String {
        let id = random_id();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".to_string());
        let blob = Blob {
            path,
            file_name,
            content_type: content_type.to_string(),
        };
        self.inner.lock().unwrap().insert(id.clone(), blob);
        id
    }

    /// Look up a blob by id.
    pub fn get(&self, id: &str) -> Option<Blob> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    /// Resolve (creating if needed) the per-process upload temp dir.
    fn ensure_upload_dir(&self) -> std::io::Result<PathBuf> {
        let mut guard = self.upload_dir.lock().unwrap();
        if let Some(dir) = guard.as_ref() {
            return Ok(dir.clone());
        }
        let dir = std::env::temp_dir().join(format!("hand-web-ui-uploads-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        *guard = Some(dir.clone());
        Ok(dir)
    }
}

/// Lowercase hex sha256 of the input bytes, used as a content-addressed id.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// A process-unique, hard-to-guess id for registered server files.
fn random_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dl-{nanos:x}-{n:x}")
}

/// Strip any path components from a client-supplied filename so it can be used
/// verbatim in a `Content-Disposition` header without traversal risk.
fn sanitize_file_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "attachment".to_string())
}

// ---- Minimal SHA-256 (no extra crate) ---------------------------------------
//
// A small, dependency-free SHA-256 keeps the crate's dependency surface flat;
// it is used only to content-address uploaded blobs, never for security.

struct Sha256;

impl Sha256 {
    fn digest(input: &[u8]) -> [u8; 32] {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];

        let mut msg = input.to_vec();
        let bit_len = (input.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        // `as_chunks` yields fixed-size arrays rather than slices, so the
        // indexing below is bounds-checked once per block instead of on
        // every access. The padding above guarantees a whole number of
        // blocks, so the remainder is empty by construction.
        for chunk in msg.as_chunks::<64>().0 {
            let mut w = [0u32; 64];
            for (i, word) in w.iter_mut().take(16).enumerate() {
                *word = u32::from_be_bytes([
                    chunk[i * 4],
                    chunk[i * 4 + 1],
                    chunk[i * 4 + 2],
                    chunk[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
                let t1 = v[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v[7] = v[6];
                v[6] = v[5];
                v[5] = v[4];
                v[4] = v[3].wrapping_add(t1);
                v[3] = v[2];
                v[2] = v[1];
                v[1] = v[0];
                v[0] = t1.wrapping_add(t2);
            }
            for (hi, vi) in h.iter_mut().zip(v.iter()) {
                *hi = hi.wrapping_add(*vi);
            }
        }

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        // Standard NIST vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn upload_then_get_round_trips_bytes() {
        let store = BlobStore::new();
        let bytes = b"hello attachment bytes";
        let (id, size) = store
            .put_upload(bytes, "note.txt", "text/plain")
            .expect("upload should succeed");
        assert_eq!(size, bytes.len());

        let blob = store.get(&id).expect("blob should be retrievable by id");
        assert_eq!(blob.file_name, "note.txt");
        assert_eq!(blob.content_type, "text/plain");
        let read_back = std::fs::read(&blob.path).expect("on-disk bytes should be readable");
        assert_eq!(read_back, bytes);
    }

    #[test]
    fn identical_uploads_coalesce_onto_one_id() {
        let store = BlobStore::new();
        let (id1, _) = store
            .put_upload(b"same", "a.bin", "application/octet-stream")
            .unwrap();
        let (id2, _) = store
            .put_upload(b"same", "b.bin", "application/octet-stream")
            .unwrap();
        assert_eq!(id1, id2, "content-addressed ids must coalesce");
    }

    #[test]
    fn register_file_assigns_unique_ids() {
        let store = BlobStore::new();
        let id1 = store.register_file(PathBuf::from("/tmp/export-1.html"), "text/html");
        let id2 = store.register_file(PathBuf::from("/tmp/export-2.html"), "text/html");
        assert_ne!(id1, id2);
        let blob = store
            .get(&id1)
            .expect("registered file must be retrievable");
        assert_eq!(blob.file_name, "export-1.html");
    }

    #[test]
    fn unknown_id_returns_none() {
        let store = BlobStore::new();
        assert!(store.get("nope").is_none());
    }

    #[test]
    fn sanitize_strips_path_traversal() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name(""), "attachment");
        assert_eq!(sanitize_file_name("plain.png"), "plain.png");
    }
}
