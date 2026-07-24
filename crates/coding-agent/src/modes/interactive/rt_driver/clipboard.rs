//! Ctrl+V clipboard paste for the rt interactive driver (VAL-IMG-010).
//!
//! Pressing Ctrl+V pulls the system clipboard into the chat editor:
//!
//! - **text clipboard:** the text is inserted verbatim at the caret (the cheap,
//!   always-probable path);
//! - **image clipboard:** the image is written to a temp PNG and its **absolute
//!   path** is inserted at the caret, so the agent picks it up as a file
//!   reference (the expensive path — exercised manually / with a seeded
//!   clipboard);
//! - **empty / unavailable / failure:** the editor is left unchanged and a red
//!   status line lands in chat.
//!
//! The clipboard read itself ([`read_clipboard_text`] / [`read_clipboard_image`])
//! lives in [`crate::utils`]; this module owns the *decision* — text vs. image
//! vs. error, and the temp-PNG write — so the control flow is unit-tested without
//! touching the real system clipboard.

use std::path::{Path, PathBuf};

use crate::utils::clipboard_image::{ClipboardImage, extension_for_image_mime_type};

/// What a Ctrl+V paste resolves to, ready for the input loop to apply to the
/// editor / chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteOutcome {
    /// Insert this text at the caret verbatim (the clipboard held text).
    InsertText(String),
    /// Insert this absolute path at the caret (the clipboard held an image, now
    /// written to a temp PNG).
    InsertPath(String),
    /// Nothing usable on the clipboard, or a write failed: leave the editor
    /// unchanged and land this error line in chat.
    Failed(String),
}

/// Resolve a Ctrl+V paste from the two clipboard reads, writing an image to disk
/// under `temp_dir` when the clipboard holds one.
///
/// Precedence matches the legacy binding: **text first** (the common, cheap case),
/// then image, then the "nothing to paste" error. `text` and `image` are the
/// already-performed reads (`None` for text = no text on the clipboard; the image
/// `Result` distinguishes "no image" from "clipboard unavailable"), injected so
/// the decision is testable without the system clipboard.
#[must_use]
pub fn resolve_paste(
    text: Option<String>,
    image: Result<Option<ClipboardImage>, String>,
    temp_dir: &Path,
) -> PasteOutcome {
    // Text wins: verbatim insert, the cheap probable path.
    if let Some(text) = text
        && !text.is_empty()
    {
        return PasteOutcome::InsertText(text);
    }

    match image {
        Ok(Some(img)) => write_image_and_path(&img, temp_dir),
        Ok(None) => PasteOutcome::Failed("[clipboard: nothing to paste]".to_string()),
        Err(e) => PasteOutcome::Failed(format!("[clipboard unavailable: {e}]")),
    }
}

/// Write a clipboard image to a temp file under `temp_dir` and return an
/// [`PasteOutcome::InsertPath`] with its absolute path, or a
/// [`PasteOutcome::Failed`] if the write failed.
fn write_image_and_path(image: &ClipboardImage, temp_dir: &Path) -> PasteOutcome {
    let ext = extension_for_image_mime_type(&image.mime_type).unwrap_or("png");
    let file_name = format!("hand-clipboard-{}.{ext}", unique_stamp());
    let path = temp_dir.join(file_name);
    match std::fs::write(&path, &image.bytes) {
        Ok(()) => PasteOutcome::InsertPath(absolute_lossy(&path)),
        Err(e) => PasteOutcome::Failed(format!("[clipboard image write failed: {e}]")),
    }
}

/// A per-paste unique stamp for the temp file name, so two rapid image pastes
/// never collide. Nanosecond wall-clock time is monotonic-enough for a file name
/// and needs no extra dependency.
fn unique_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// The absolute path string for `path`, canonicalising the *parent* directory so
/// a temp dir given as a relative path still yields an absolute reference the
/// agent can resolve. Falls back to the lossy display form if canonicalisation
/// fails (the file was just written, so the parent exists in practice).
fn absolute_lossy(path: &Path) -> String {
    let abs: PathBuf = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    };
    abs.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_image() -> ClipboardImage {
        // PNG magic + minimal filler; the write path does not decode it.
        ClipboardImage {
            bytes: b"\x89PNG\r\n\x1a\nfake".to_vec(),
            mime_type: "image/png".to_string(),
        }
    }

    // --- text paste inserts verbatim (VAL-IMG-010, cheap) -----------------

    #[test]
    fn text_clipboard_inserts_verbatim() {
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(Some("hello world".to_string()), Ok(None), dir.path());
        assert_eq!(
            outcome,
            PasteOutcome::InsertText("hello world".to_string()),
            "text on the clipboard inserts verbatim",
        );
    }

    #[test]
    fn text_wins_over_a_simultaneous_image() {
        // If both are present, text is the cheap probable path and wins.
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(Some("typed".to_string()), Ok(Some(png_image())), dir.path());
        assert_eq!(outcome, PasteOutcome::InsertText("typed".to_string()));
    }

    // --- empty / unavailable clipboard → failure (VAL-IMG-010) ------------

    #[test]
    fn empty_clipboard_reports_nothing_to_paste() {
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(None, Ok(None), dir.path());
        assert!(
            matches!(&outcome, PasteOutcome::Failed(msg) if msg.contains("nothing to paste")),
            "an empty clipboard reports nothing to paste, got {outcome:?}",
        );
    }

    #[test]
    fn blank_text_is_treated_as_empty() {
        // An empty string is not a usable paste; it falls through to the image /
        // error path rather than inserting a zero-length paste.
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(Some(String::new()), Ok(None), dir.path());
        assert!(matches!(outcome, PasteOutcome::Failed(_)));
    }

    #[test]
    fn unavailable_clipboard_reports_the_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(None, Err("no display server".to_string()), dir.path());
        assert!(
            matches!(&outcome, PasteOutcome::Failed(msg) if msg.contains("unavailable")),
            "a clipboard read error surfaces as a failure, got {outcome:?}",
        );
    }

    // --- image paste writes a temp PNG + inserts its path (VAL-IMG-010) ---

    #[test]
    fn image_clipboard_writes_temp_png_and_inserts_absolute_path() {
        let dir = tempfile::tempdir().expect("tmp");
        let outcome = resolve_paste(None, Ok(Some(png_image())), dir.path());

        let path = match outcome {
            PasteOutcome::InsertPath(p) => p,
            other => panic!("expected an inserted path, got {other:?}"),
        };
        // The inserted path is absolute and the file was actually written.
        let p = Path::new(&path);
        assert!(p.is_absolute(), "inserted path must be absolute: {path}");
        assert!(p.exists(), "the temp PNG must exist on disk: {path}");
        assert!(path.ends_with(".png"), "png extension expected: {path}");
        let written = std::fs::read(p).expect("read back the temp png");
        assert_eq!(
            &written[..8],
            b"\x89PNG\r\n\x1a\n",
            "the PNG bytes round-trip"
        );
    }
}
