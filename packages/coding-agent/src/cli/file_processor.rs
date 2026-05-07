//! Resolve `@file` CLI arguments into prompt text and image attachments.
//!
//! TS reference: `cli/file-processor.ts`. Each `@<path>` argument is
//! interpreted as either:
//!
//! - a *text file*, whose contents are wrapped in a `<file name="…">…</file>`
//!   block and concatenated into the returned prompt text;
//! - or an *image file*, base64-encoded into an [`ImageContent`] attachment
//!   and accompanied by a placeholder `<file name="…"></file>` marker in the
//!   text stream so the model can still see the file path.
//!
//! Image type detection uses
//! [`crate::utils::mime::detect_supported_image_mime_type_from_file`] (magic
//! bytes; supports JPEG, PNG, GIF, WebP only).
//!
//! ## Differences from the TS reference
//!
//! 1. **No image resize.** The TS port pulls in a Photon (Rust/WASM)
//!    pipeline via `utils/image-resize.ts` to clamp images to ≤ 2000×2000
//!    and ≤ 4.5 MB base64. That utility is not yet ported to Rust, so this
//!    helper currently passes images through verbatim (equivalent to TS
//!    `autoResizeImages: false`). When `image-resize` lands, this module
//!    should grow an `auto_resize` flag matching the TS option. Tracked as
//!    a follow-up; callers should be aware that very large images will be
//!    forwarded to providers as-is.
//! 2. **Errors are returned, not `process.exit(1)`.** The TS function
//!    prints to stderr and exits the process on missing or unreadable
//!    files. Rust returns a [`FileProcessorError`] so the caller decides
//!    how to surface the failure (the binary still exits non-zero).
//! 3. **Path resolution.** TS calls into `core/tools/path-utils.ts`'s
//!    `resolveReadPath`, which probes a handful of macOS Unicode-space and
//!    NFD/curly-quote variants. That helper has not been ported either, so
//!    we use a minimal resolver: tilde expansion plus cwd-join. macOS
//!    screenshot edge cases will raise [`FileProcessorError::NotFound`]
//!    until `resolveReadPath` is ported.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use model::types::ImageContent;
use thiserror::Error;

use crate::utils::mime::{MimeError, detect_supported_image_mime_type_from_file};

/// Result of resolving one or more `@file` arguments.
#[derive(Debug, Default, Clone)]
pub struct ProcessedFiles {
    /// Concatenated `<file>...</file>` blocks suitable for prepending to
    /// the user prompt. Always non-empty when at least one non-empty file
    /// was processed.
    pub text: String,
    /// Image attachments, in the order the corresponding `@file`
    /// arguments were supplied.
    pub images: Vec<ImageContent>,
}

/// Errors raised by [`process_file_arguments`].
#[derive(Debug, Error)]
pub enum FileProcessorError {
    /// The argument resolved to a path that does not exist.
    #[error("file not found: {0}")]
    NotFound(PathBuf),
    /// The file existed but could not be read.
    #[error("could not read file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// MIME-detection I/O failure (file existed but could not be opened
    /// for sniffing).
    #[error("could not sniff MIME for {path}: {source}")]
    Mime {
        path: PathBuf,
        #[source]
        source: MimeError,
    },
    /// Text file was not valid UTF-8.
    #[error("file {path} is not valid UTF-8")]
    NotUtf8 { path: PathBuf },
}

/// Expand a leading `~` / `~/` to the user's home directory, returning
/// the input untouched on any other prefix.
///
/// The TS reference's `expandPath` also strips a leading `@` because the
/// CLI sometimes passes the literal sigil through; we do the same so
/// callers can hand us either `path` or `@path`.
fn expand_path(input: &str) -> PathBuf {
    let trimmed = input.strip_prefix('@').unwrap_or(input);
    if trimmed == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = trimmed.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(trimmed)
}

/// Resolve a CLI `@file` argument to an absolute filesystem path.
///
/// Steps: strip leading `@`, expand `~`, join against `cwd` if relative.
/// Does *not* probe macOS Unicode variants (that lives in the unported
/// `path-utils::resolveReadPath`).
fn resolve_read_path(file_arg: &str, cwd: &Path) -> PathBuf {
    let expanded = expand_path(file_arg);
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

/// Process `@file` arguments into prompt text and image attachments.
///
/// Empty files are skipped silently to match the TS reference. Image
/// detection sniffs magic bytes; everything else is treated as UTF-8 text.
pub fn process_file_arguments(
    file_args: &[String],
    cwd: &Path,
) -> Result<ProcessedFiles, FileProcessorError> {
    let mut out = ProcessedFiles::default();

    for arg in file_args {
        let path = resolve_read_path(arg, cwd);

        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return Err(FileProcessorError::NotFound(path)),
        };

        // Skip empty files (TS contract).
        if metadata.len() == 0 {
            continue;
        }

        let mime = detect_supported_image_mime_type_from_file(&path).map_err(|e| {
            FileProcessorError::Mime {
                path: path.clone(),
                source: e,
            }
        })?;

        match mime {
            Some(mime) => {
                let bytes = fs::read(&path).map_err(|e| FileProcessorError::Read {
                    path: path.clone(),
                    source: e,
                })?;
                let data = BASE64.encode(&bytes);
                out.images.push(ImageContent {
                    content_type: "image".to_string(),
                    data,
                    mime_type: mime,
                });
                // Placeholder marker so the model can still see the file
                // path next to the inline image attachment. We do not emit
                // a dimension note (no resize -> no scale factor).
                out.text
                    .push_str(&format!("<file name=\"{}\"></file>\n", path.display()));
            }
            None => {
                let bytes = fs::read(&path).map_err(|e| FileProcessorError::Read {
                    path: path.clone(),
                    source: e,
                })?;
                let content = String::from_utf8(bytes)
                    .map_err(|_| FileProcessorError::NotUtf8 { path: path.clone() })?;
                out.text.push_str(&format!(
                    "<file name=\"{}\">\n{}\n</file>\n",
                    path.display(),
                    content
                ));
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 1×1 transparent PNG, base64-decoded into raw bytes.
    fn tiny_png_bytes() -> Vec<u8> {
        BASE64
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
            )
            .expect("valid embedded PNG")
    }

    #[test]
    fn returns_empty_when_no_args() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = process_file_arguments(&[], tmp.path()).expect("ok");
        assert!(result.text.is_empty());
        assert!(result.images.is_empty());
    }

    #[test]
    fn wraps_text_files_in_file_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("note.txt");
        fs::write(&path, "hello world\n").expect("write");

        let args = vec!["note.txt".to_string()];
        let result = process_file_arguments(&args, tmp.path()).expect("ok");
        assert!(result.images.is_empty());
        assert!(
            result
                .text
                .starts_with(&format!("<file name=\"{}\">\n", path.display()))
        );
        assert!(result.text.contains("hello world\n"));
        assert!(result.text.ends_with("</file>\n"));
    }

    #[test]
    fn strips_leading_at_sigil() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("note.txt");
        fs::write(&path, "x").expect("write");

        let args = vec!["@note.txt".to_string()];
        let result = process_file_arguments(&args, tmp.path()).expect("ok");
        assert!(result.text.contains("note.txt"));
    }

    #[test]
    fn skips_empty_files_silently() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("empty.txt");
        fs::File::create(&path).expect("create");

        let args = vec!["empty.txt".to_string()];
        let result = process_file_arguments(&args, tmp.path()).expect("ok");
        assert!(result.text.is_empty());
        assert!(result.images.is_empty());
    }

    #[test]
    fn missing_file_yields_not_found_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let args = vec!["nope.txt".to_string()];
        let err = process_file_arguments(&args, tmp.path()).expect_err("missing file should error");
        match err {
            FileProcessorError::NotFound(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn detects_png_and_emits_image_attachment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pixel.png");
        let mut f = fs::File::create(&path).expect("create");
        f.write_all(&tiny_png_bytes()).expect("write png");
        drop(f);

        let args = vec!["pixel.png".to_string()];
        let result = process_file_arguments(&args, tmp.path()).expect("ok");
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[0].content_type, "image");
        // Decoded base64 round-trips back to the original bytes.
        let decoded = BASE64.decode(&result.images[0].data).expect("valid base64");
        assert_eq!(decoded, tiny_png_bytes());
        // Text marker present, with no dimension note.
        assert!(
            result
                .text
                .contains(&format!("<file name=\"{}\"></file>", path.display()))
        );
    }

    #[test]
    fn non_utf8_text_file_yields_not_utf8_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bin.dat");
        // Bytes that are neither a recognised image magic header nor valid UTF-8.
        fs::write(&path, [0x00u8, 0xFF, 0xFE, 0xFD]).expect("write");

        let args = vec!["bin.dat".to_string()];
        let err = process_file_arguments(&args, tmp.path()).expect_err("non-utf8 should error");
        match err {
            FileProcessorError::NotUtf8 { .. } => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn expand_path_handles_tilde() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(expand_path("~"), home);
        assert_eq!(expand_path("~/foo"), home.join("foo"));
        // Leading `@` plus tilde.
        assert_eq!(expand_path("@~/bar"), home.join("bar"));
        // Plain relative path.
        assert_eq!(expand_path("foo/bar"), PathBuf::from("foo/bar"));
    }
}
