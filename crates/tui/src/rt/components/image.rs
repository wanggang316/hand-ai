//! Image widget + graphics-protocol emission for the rt stack.
//!
//! # Why an image needs a channel that bypasses the `Buffer`
//!
//! A ratatui [`Buffer`] is a grid of text cells. The graphics protocols that put
//! a *picture* on the screen — the Kitty graphics protocol (`\x1b_G…\x1b\\`, an
//! APC string) and iTerm2 inline images (`\x1b]1337;File=…`, an OSC) — are not
//! cell content: they are escape sequences the terminal interprets out of band,
//! positioned at the current cursor. They cannot be stored in a cell and diffed
//! like text. So a graphics image on the rt stack is a **two-part** thing:
//!
//! 1. **In the buffer:** [`RtImage`] reserves `N` rows (blank cells in graphics
//!    mode, a bordered placeholder box in fallback mode). This is what the frame
//!    diff paints and clears, so the image's footprint scrolls, resizes, and
//!    repaints like any other widget — no ghost when it moves or the pane shrinks.
//! 2. **Out of band:** in graphics mode, [`RtImage::render`] does *not* try to
//!    stuff the escape into a cell. It records a [`PendingEmission`] — the encoded
//!    escape plus the viewport-local row it belongs on — into a shared
//!    [`RawEmissionQueue`]. After `terminal.draw` returns, the draw-owning task
//!    drains the queue and writes each escape at its row via
//!    [`RawEmissionQueue::flush_to`], moving the real cursor there first.
//!
//! This split is the load-bearing new mechanism. It respects invariant #1 of the
//! architecture ("the scheduler owns the terminal"): the widget never writes to
//! the terminal itself — it only *renders into a buffer* and *enqueues* an
//! emission; the single draw-owning task performs the raw write, ordered strictly
//! after the frame's `draw`. `m2-image-scrollback` extends this same queue to
//! carry images into native scrollback (history) and to attach OSC 8 hyperlinks;
//! nothing about the queue's shape needs to change for that — a scrollback image
//! is just a [`PendingEmission`] whose row is resolved against a committed history
//! line instead of a viewport row.
//!
//! # Protocol selection
//!
//! Capability detection is delegated wholesale to
//! [`crate::terminal_image::detect_capabilities`] / [`get_capabilities`] — the
//! legacy encoder module is the single source of truth for "what does this
//! terminal speak", and it is reused unchanged. This module only *resolves* the
//! snapshot into a concrete plan ([`resolve_protocol`]) and *frames* the bytes:
//!
//! - **Kitty** (ghostty / wezterm / kitty): `encode_kitty`, an APC with a
//!   transfer header and `i=<id>`. Kitty transmits PNG (`f=100`), so a non-PNG
//!   source is transcoded to PNG first ([`transcode_to_png`]); a source that
//!   cannot be decoded degrades to the fallback box rather than emitting an
//!   invalid APC.
//! - **iTerm2**: `encode_iterm2`, an OSC 1337 carrying the source bytes natively
//!   (iTerm2 decodes jpeg/gif/webp/png itself — no transcode).
//! - **Fallback** (plain terminal, or a multiplexer that swallows graphics): a
//!   bordered placeholder box painted into cells, labelled with the filename/alt
//!   and, when the bytes sniff, a `[<mime> WxH]` tag. No graphics bytes are ever
//!   produced on this path.

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};
use crate::terminal_image::{
    ImageDimensions, ImageRenderOptions, allocate_image_id, calculate_image_rows, encode_iterm2,
    encode_kitty, get_capabilities, get_image_dimensions, image_fallback,
};

/// The graphics protocol this terminal will actually receive, resolved from a
/// [`crate::terminal_image::TerminalImageCapabilities`] snapshot.
///
/// The rt-layer view of protocol selection. It is deliberately the same three
/// arms as [`crate::terminal_image::ImageProtocol`] but re-exposed here so this
/// module (and its tests) name the resolved choice without reaching across the
/// legacy boundary in call sites, and so the resolution *rule* — kitty wins a
/// tie, a multiplexer forces fallback — is pinned by rt-owned tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedProtocol {
    /// Kitty graphics protocol (Kitty, Ghostty, WezTerm). PNG payload.
    Kitty,
    /// iTerm2 inline images. Native (untranscoded) payload.
    ITerm2,
    /// No graphics protocol: paint a cell placeholder, emit zero graphics bytes.
    Fallback,
}

/// Resolve the *current* terminal's capabilities into a concrete protocol choice.
///
/// A thin wrapper over [`get_capabilities`] (which caches
/// [`detect_capabilities`](crate::terminal_image::detect_capabilities)) that maps
/// the capability snapshot to a [`ResolvedProtocol`]. The selection rule lives in
/// the legacy `protocol()` and is reused verbatim:
///
/// - a multiplexer (tmux/screen) reports neither capability → [`Fallback`], even
///   when the outer terminal *would* speak a protocol (the multiplexer swallows
///   graphics);
/// - Kitty wins a Kitty+iTerm2 tie;
/// - iTerm2 only when Kitty is absent.
///
/// [`Fallback`]: ResolvedProtocol::Fallback
#[must_use]
pub fn resolve_protocol() -> ResolvedProtocol {
    resolve(get_capabilities().kitty, get_capabilities().iterm2)
}

/// Pure resolution of `(kitty, iterm2)` capability flags into a protocol.
///
/// Split out from [`resolve_protocol`] so the matrix — ghostty→Kitty,
/// tmux-suppressed→Fallback, kitty+iterm2→Kitty — is unit-tested without touching
/// the global capability cache. The capability flags themselves carry the
/// multiplexer suppression (detect returns `kitty=false, iterm2=false` inside a
/// multiplexer), so this function is just the tie-break.
#[must_use]
pub fn resolve(kitty: bool, iterm2: bool) -> ResolvedProtocol {
    if kitty {
        ResolvedProtocol::Kitty
    } else if iterm2 {
        ResolvedProtocol::ITerm2
    } else {
        ResolvedProtocol::Fallback
    }
}

/// The image container format, sniffed from magic bytes.
///
/// Used both to build the fallback `[<mime> …]` tag and to decide whether the
/// Kitty path must transcode (everything but [`Png`] does).
///
/// [`Png`]: ImageFormat::Png
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG — Kitty transmits this natively (`f=100`).
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
    /// Magic bytes matched no known container.
    Unknown,
}

impl ImageFormat {
    /// Sniff the container from the leading magic bytes.
    #[must_use]
    pub fn sniff(data: &[u8]) -> Self {
        if data.len() >= 4 && data[..4] == [0x89, 0x50, 0x4e, 0x47] {
            ImageFormat::Png
        } else if data.len() >= 2 && data[..2] == [0xff, 0xd8] {
            ImageFormat::Jpeg
        } else if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
            ImageFormat::Gif
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            ImageFormat::Webp
        } else {
            ImageFormat::Unknown
        }
    }

    /// The short MIME-ish tag shown in the fallback placeholder (`png`, `jpeg`,
    /// …). [`Unknown`](ImageFormat::Unknown) has no tag.
    #[must_use]
    pub fn mime_tag(self) -> Option<&'static str> {
        match self {
            ImageFormat::Png => Some("png"),
            ImageFormat::Jpeg => Some("jpeg"),
            ImageFormat::Gif => Some("gif"),
            ImageFormat::Webp => Some("webp"),
            ImageFormat::Unknown => None,
        }
    }
}

/// The placeholder tag for a non-graphics terminal: `[<mime> WxH]` when both the
/// format and dimensions sniff, `[<mime>]` when only the format is known (a
/// corrupt file with a valid magic prefix but no readable dimensions), or `None`
/// when the bytes match no container at all.
///
/// This is the label that lets a plain-terminal capture confirm *which* format a
/// dropped image was and, when known, its pixel size — without decoding it.
#[must_use]
pub fn sniff_label(data: &[u8]) -> Option<String> {
    let mime = ImageFormat::sniff(data).mime_tag()?;
    match get_image_dimensions(data) {
        Some(ImageDimensions { width, height }) => Some(format!("[{mime} {width}x{height}]")),
        None => Some(format!("[{mime}]")),
    }
}

/// A single graphics escape queued for emission after the frame draws.
///
/// The out-of-band half of an image: the encoded protocol sequence (`\x1b_G…` or
/// `\x1b]1337;…`) plus the *viewport-local* row it must be written at. The
/// draw-owning task resolves the row against the viewport origin and moves the
/// cursor there before writing `escape` (see [`RawEmissionQueue::flush_to`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEmission {
    /// The fully-encoded graphics escape sequence.
    pub escape: String,
    /// The viewport-local row (`0` = top of the viewport) the image's top edge
    /// occupies. The flush adds the viewport origin to place it absolutely.
    pub row: u16,
    /// How many rows the image reserves. Carried so a later consumer
    /// (scrollback) can reason about the footprint; the viewport flush does not
    /// need it (the terminal advances the cursor itself after the escape).
    pub rows: u16,
}

/// The raw graphics-emission channel: a shared queue of [`PendingEmission`]s an
/// [`RtImage`] fills during `render` and the draw-owning task drains after the
/// frame's `terminal.draw`.
///
/// This is the mechanism that lets graphics bypass the `Buffer` without any
/// widget touching the terminal. A cheap `Arc<Mutex<Vec<_>>>`: cloning hands the
/// *same* queue to every image widget and to the draw task, exactly like the
/// scheduler's [`FrameRequester`](crate::rt::scheduler::FrameRequester) is cloned
/// to producers. Contention is nil — the critical section is a `Vec` push/drain
/// and the draw task is the only reader.
///
/// # Ordering contract
///
/// The draw task must call [`flush_to`](RawEmissionQueue::flush_to) *after*
/// `terminal.draw` (so the reserved blank rows are painted first, then the image
/// is drawn on top) and *before* releasing the synchronized-output block (so the
/// image appears atomically with the frame). Each frame typically calls
/// [`take`](RawEmissionQueue::take) or `flush_to` to drain, leaving the queue
/// empty for the next frame.
#[derive(Debug, Clone, Default)]
pub struct RawEmissionQueue {
    inner: Arc<Mutex<Vec<PendingEmission>>>,
}

impl RawEmissionQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue an emission (called by [`RtImage::render`] in graphics mode).
    pub fn push(&self, emission: PendingEmission) {
        self.lock().push(emission);
    }

    /// Whether the queue currently holds no pending emissions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Drain and return every pending emission, leaving the queue empty.
    ///
    /// The pure-data drain used by tests and by a flush target that wants to
    /// order the writes itself.
    #[must_use]
    pub fn take(&self) -> Vec<PendingEmission> {
        std::mem::take(&mut *self.lock())
    }

    /// Flush every pending emission to `out`, positioning the cursor at each
    /// image's absolute row (`viewport_origin_y + emission.row`) before writing
    /// its escape, then leaving the queue empty.
    ///
    /// `viewport_origin_y` is `frame.area().y` — the row the inline viewport
    /// currently starts at, which `insert_before` slides down as scrollback
    /// fills (architecture invariant #4). Emissions are sorted by row so the
    /// cursor moves monotonically down the frame. The cursor is saved
    /// (`\x1b7`) before the first move and restored (`\x1b8`) after the last, so
    /// the raw emission does not disturb wherever the draw left the caret (e.g.
    /// the input caret ratatui positioned).
    ///
    /// # Errors
    ///
    /// Propagates the first write error; on success the queue is drained.
    pub fn flush_to(
        &self,
        out: &mut impl std::io::Write,
        viewport_origin_y: u16,
    ) -> std::io::Result<()> {
        let mut pending = self.take();
        if pending.is_empty() {
            return Ok(());
        }
        pending.sort_by_key(|e| e.row);
        // Save the cursor once so the input caret / draw position survives the
        // out-of-band writes.
        out.write_all(b"\x1b7")?;
        for emission in &pending {
            let absolute_row = viewport_origin_y.saturating_add(emission.row);
            // CUP is 1-based; column 1 (left edge of the reserved rows).
            write!(out, "\x1b[{};1H", absolute_row.saturating_add(1))?;
            out.write_all(emission.escape.as_bytes())?;
        }
        out.write_all(b"\x1b8")?;
        out.flush()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PendingEmission>> {
        self.inner
            .lock()
            .expect("raw emission queue mutex poisoned")
    }
}

/// An image widget for the rt stack.
///
/// Renders as **reserved rows plus an out-of-band graphics emission** (graphics
/// terminal) or a **bordered placeholder box** (plain / multiplexer). See the
/// module docs for the buffer-bypass rationale.
///
/// Construct with [`RtImage::new`], optionally give it a [`label`](RtImage::label)
/// (filename/alt shown in the placeholder and, for iTerm2, as the image name),
/// and — for graphics mode — attach a [`RawEmissionQueue`] via
/// [`emission_queue`](RtImage::emission_queue) so `render` has somewhere to
/// enqueue the escape. Without a queue, a graphics-mode image still reserves its
/// rows but enqueues nothing (there is no channel), which is the correct
/// degenerate behaviour for a pure `Buffer` snapshot test.
pub struct RtImage {
    /// Raw source bytes (png/jpeg/gif/webp).
    data: Vec<u8>,
    /// Filename or alt text shown in the placeholder / used as the iTerm2 name.
    label: Option<String>,
    /// Where a graphics-mode emission is queued. `None` = reserve rows only.
    queue: Option<RawEmissionQueue>,
    /// Forced protocol override (tests / the gallery forced-env seam). `None`
    /// resolves from the live terminal capabilities.
    protocol_override: Option<ResolvedProtocol>,
}

impl RtImage {
    /// An image over `data`, protocol resolved from the live terminal.
    #[must_use]
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            label: None,
            queue: None,
            protocol_override: None,
        }
    }

    /// Set the filename / alt label (shown in the fallback box; used as the
    /// iTerm2 image name).
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attach the [`RawEmissionQueue`] a graphics-mode render enqueues into.
    #[must_use]
    pub fn emission_queue(mut self, queue: RawEmissionQueue) -> Self {
        self.queue = Some(queue);
        self
    }

    /// Force a specific protocol, bypassing capability detection.
    ///
    /// The seam the gallery's forced-env switch and the emission tests use to
    /// exercise each protocol path deterministically without a real terminal.
    #[must_use]
    pub fn protocol(mut self, protocol: ResolvedProtocol) -> Self {
        self.protocol_override = Some(protocol);
        self
    }

    /// The protocol this image will use: the override if set, else resolved from
    /// the live terminal capabilities.
    #[must_use]
    pub fn resolved_protocol(&self) -> ResolvedProtocol {
        self.protocol_override.unwrap_or_else(resolve_protocol)
    }

    /// The number of rows this image reserves in `area`, from the sniffed pixel
    /// height scaled to the current cell height, clamped to the area's height.
    ///
    /// Falls back to the whole area height when the bytes do not sniff (an
    /// undecodable blob still reserves a sensible footprint rather than
    /// collapsing to one row).
    #[must_use]
    pub fn reserved_rows(&self, area: Rect) -> u16 {
        let max = area.height;
        match get_image_dimensions(&self.data) {
            Some(dims) => calculate_image_rows(&dims, Some(max)).min(max).max(1),
            None => max.max(1),
        }
        .min(max.max(1))
    }

    /// Build the encoded graphics escape for the given protocol, or `None` when
    /// this image degrades to the fallback box (undecodable source on the Kitty
    /// transcode path).
    ///
    /// Split out and public so an emission test asserts the framing without a
    /// live terminal or a queue.
    #[must_use]
    pub fn encode(&self, protocol: ResolvedProtocol, area: Rect) -> Option<String> {
        let rows = self.reserved_rows(area);
        let opts = ImageRenderOptions {
            max_cols: Some(area.width),
            max_rows: Some(rows),
            preserve_aspect: true,
            label: self.label.clone(),
        };
        match protocol {
            ResolvedProtocol::Kitty => {
                // Kitty transmits PNG. Transcode a non-PNG source; an undecodable
                // source degrades to the box rather than emitting an invalid APC.
                let png = match ImageFormat::sniff(&self.data) {
                    ImageFormat::Png => Some(self.data.clone()),
                    _ => transcode_to_png(&self.data),
                }?;
                Some(encode_kitty(allocate_image_id(), &png, &opts))
            }
            // iTerm2 decodes every accepted format itself: pass the bytes native.
            ResolvedProtocol::ITerm2 => Some(encode_iterm2(&self.data, &opts)),
            ResolvedProtocol::Fallback => None,
        }
    }

    /// Paint the bordered fallback placeholder into `area`, with a label that
    /// pairs the filename/alt with the sniffed `[<mime> WxH]` tag when available.
    fn render_fallback(&self, area: Rect, buf: &mut Buffer) {
        let label = self.fallback_label();
        let opts = ImageRenderOptions {
            max_cols: Some(area.width),
            max_rows: Some(area.height.saturating_sub(2).max(1)),
            preserve_aspect: true,
            label: Some(label),
        };
        let lines = image_fallback(&opts);
        for (i, line) in lines.iter().enumerate() {
            let y = area.y.saturating_add(i as u16);
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            buf.set_stringn(area.x, y, line, area.width as usize, Style::default());
        }
    }

    /// The fallback box label: the filename/alt joined with the sniff tag, or
    /// whichever of the two is present.
    fn fallback_label(&self) -> String {
        let name = self.label.as_deref();
        let tag = sniff_label(&self.data);
        match (name, tag) {
            (Some(name), Some(tag)) => format!("{name} {tag}"),
            (Some(name), None) => name.to_string(),
            (None, Some(tag)) => tag,
            (None, None) => "image".to_string(),
        }
    }
}

impl RtComponent for RtImage {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        match self.resolved_protocol() {
            ResolvedProtocol::Fallback => self.render_fallback(area, buf),
            protocol => {
                // Reserve the rows as blank cells so the frame diff paints/clears
                // the image's footprint, then queue the escape for the draw task
                // to emit out of band. If encoding degrades (undecodable source
                // on the Kitty path), fall back to the placeholder box.
                match self.encode(protocol, area) {
                    Some(escape) => {
                        let rows = self.reserved_rows(area);
                        // Blank the reserved rows: the graphics image is painted
                        // over them out of band, and blanking is what clears any
                        // prior frame's content under the image.
                        for dy in 0..rows {
                            let y = area.y.saturating_add(dy);
                            buf.set_stringn(
                                area.x,
                                y,
                                " ".repeat(area.width as usize),
                                area.width as usize,
                                Style::default(),
                            );
                        }
                        if let Some(queue) = &self.queue {
                            queue.push(PendingEmission {
                                escape,
                                row: area.y,
                                rows,
                            });
                        }
                    }
                    None => self.render_fallback(area, buf),
                }
            }
        }
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

/// Decode `data` (jpeg/gif/webp/png) and re-encode it as PNG for the Kitty path,
/// or `None` when the bytes cannot be decoded.
///
/// The `image` crate is scoped to exactly the four accepted decoders plus the PNG
/// encoder (see `Cargo.toml`). A decode failure returns `None` so the caller
/// degrades to the placeholder box instead of transmitting a corrupt APC —
/// "never emit an invalid graphics sequence" is the invariant this guards.
#[must_use]
pub fn transcode_to_png(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Base64-encode `data` (exposed for the gallery's forced-env demo, which builds
/// a synthetic emission without going through [`RtImage`]).
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    STANDARD.encode(data)
}
