//! Terminal image protocol layer.
//!
//! Encodes raw image bytes into the escape sequences understood by the host
//! terminal (Kitty graphics protocol, iTerm2 inline images, or a plain-text
//! fallback). Also provides cheap dimension probes that read magic bytes
//! without performing full image decoding, plus an OSC 8 hyperlink helper
//! used by the markdown renderer.
//!
//! # Cell-size detection
//!
//! Pixel-per-cell metrics depend on the terminal font and zoom level. The
//! "correct" way to obtain them is to issue `CSI 16 t` and parse the reply,
//! but many terminals never answer and the probe blocks. To stay reliable,
//! [`detect_capabilities`] only inspects environment variables; cell
//! dimensions default to `(8, 16)`. Callers that have a better measurement
//! (e.g. from a successful `CSI 16 t` round-trip in their own event loop)
//! can override via [`set_cell_dimensions`].

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, RwLock};

/// Image rendering protocol selected for the current terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty graphics protocol (Kitty, Ghostty, WezTerm).
    Kitty,
    /// iTerm2 inline images (iTerm2.app).
    ITerm2,
    /// Neither protocol available — render a plain-text placeholder.
    Fallback,
}

/// Pixel dimensions of an image as decoded from its container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Pixel dimensions of a single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    /// Pixels per column.
    pub width: u16,
    /// Pixels per row.
    pub height: u16,
}

impl Default for CellDimensions {
    fn default() -> Self {
        // Conservative default; matches typical 8x16 monospace bitmap fonts.
        Self {
            width: 8,
            height: 16,
        }
    }
}

/// Caller-supplied rendering options.
#[derive(Debug, Clone)]
pub struct ImageRenderOptions {
    /// Maximum width in cells (None = unconstrained).
    pub max_cols: Option<u16>,
    /// Maximum height in cells (None = unconstrained).
    pub max_rows: Option<u16>,
    /// Maintain image aspect ratio when scaling.
    pub preserve_aspect: bool,
    /// Optional label shown in the fallback placeholder.
    pub label: Option<String>,
}

impl Default for ImageRenderOptions {
    fn default() -> Self {
        Self {
            max_cols: None,
            max_rows: None,
            preserve_aspect: true,
            label: None,
        }
    }
}

/// Snapshot of what the host terminal supports.
#[derive(Debug, Clone, Copy)]
pub struct TerminalImageCapabilities {
    /// Terminal speaks Kitty graphics protocol.
    pub kitty: bool,
    /// Terminal speaks iTerm2 inline images.
    pub iterm2: bool,
    /// Pixel size of one terminal cell.
    pub cell_dimensions: CellDimensions,
}

impl TerminalImageCapabilities {
    /// Pick the preferred encoder for the current capabilities.
    pub fn protocol(&self) -> ImageProtocol {
        if self.kitty {
            ImageProtocol::Kitty
        } else if self.iterm2 {
            ImageProtocol::ITerm2
        } else {
            ImageProtocol::Fallback
        }
    }
}

/// Process-wide image-id allocator (Kitty assigns ids per image).
static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);

/// Allocate a fresh Kitty image id. Wraps to 1 on overflow (id 0 is invalid).
pub fn allocate_image_id() -> u32 {
    loop {
        let id = NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

/// Cached capabilities, populated on first access.
static CAPABILITIES: LazyLock<RwLock<Option<TerminalImageCapabilities>>> =
    LazyLock::new(|| RwLock::new(None));

/// Probe environment variables to determine which protocol the terminal speaks.
///
/// Does **not** issue any escape sequences — see module docs for rationale.
pub fn detect_capabilities() -> TerminalImageCapabilities {
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();

    // tmux/screen swallow most graphics sequences; stay conservative.
    let in_multiplexer = std::env::var("TMUX").is_ok()
        || std::env::var("CMUX_WORKSPACE_ID").is_ok()
        || term.starts_with("tmux")
        || term.starts_with("screen");
    if in_multiplexer {
        return TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        };
    }

    let kitty = std::env::var("KITTY_WINDOW_ID").is_ok()
        || term_program == "kitty"
        || term_program == "ghostty"
        || term.contains("ghostty")
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
        || term_program == "wezterm";

    let iterm2 = !kitty
        && (std::env::var("ITERM_SESSION_ID").is_ok() || term_program == "iterm.app");

    TerminalImageCapabilities {
        kitty,
        iterm2,
        cell_dimensions: CellDimensions::default(),
    }
}

/// Read the cached capabilities (detecting on first call).
pub fn get_capabilities() -> TerminalImageCapabilities {
    {
        let guard = CAPABILITIES.read().expect("capabilities cache poisoned");
        if let Some(caps) = *guard {
            return caps;
        }
    }
    let caps = detect_capabilities();
    let mut guard = CAPABILITIES.write().expect("capabilities cache poisoned");
    if guard.is_none() {
        *guard = Some(caps);
    }
    guard.unwrap_or(caps)
}

/// Override the cached capabilities (primarily for tests).
pub fn set_capabilities(caps: TerminalImageCapabilities) {
    let mut guard = CAPABILITIES.write().expect("capabilities cache poisoned");
    *guard = Some(caps);
}

/// Drop the cached capabilities, forcing the next access to re-detect.
pub fn reset_capabilities_cache() {
    let mut guard = CAPABILITIES.write().expect("capabilities cache poisoned");
    *guard = None;
}

/// Read just the cached cell dimensions.
pub fn get_cell_dimensions() -> CellDimensions {
    get_capabilities().cell_dimensions
}

/// Update the cached cell dimensions (e.g. after a successful `CSI 16 t` reply).
pub fn set_cell_dimensions(dims: CellDimensions) {
    let mut guard = CAPABILITIES.write().expect("capabilities cache poisoned");
    if let Some(caps) = guard.as_mut() {
        caps.cell_dimensions = dims;
    } else {
        *guard = Some(TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: dims,
        });
    }
}

/// Maximum bytes per Kitty APC chunk (protocol limit is 4096 base64 chars).
const KITTY_CHUNK_SIZE: usize = 4096;

/// Encode `data` as a Kitty graphics protocol transmit-and-display sequence.
pub fn encode_kitty(image_id: u32, data: &[u8], opts: &ImageRenderOptions) -> String {
    let payload = STANDARD.encode(data);

    let mut params: Vec<String> = vec!["a=T".into(), "f=100".into(), "q=2".into()];
    if let Some(c) = opts.max_cols {
        params.push(format!("c={c}"));
    }
    if let Some(r) = opts.max_rows {
        params.push(format!("r={r}"));
    }
    params.push(format!("i={image_id}"));
    let header = params.join(",");

    if payload.len() <= KITTY_CHUNK_SIZE {
        return format!("\x1b_G{header};{payload}\x1b\\");
    }

    let mut out = String::with_capacity(payload.len() + 32);
    let mut offset = 0;
    let mut first = true;
    while offset < payload.len() {
        let end = (offset + KITTY_CHUNK_SIZE).min(payload.len());
        let chunk = &payload[offset..end];
        let is_last = end >= payload.len();
        if first {
            out.push_str(&format!("\x1b_G{header},m=1;{chunk}\x1b\\"));
            first = false;
        } else if is_last {
            out.push_str(&format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
        offset = end;
    }
    out
}

/// Encode `data` as an iTerm2 inline-image OSC 1337 sequence.
pub fn encode_iterm2(data: &[u8], opts: &ImageRenderOptions) -> String {
    let payload = STANDARD.encode(data);

    let mut params: Vec<String> = vec!["inline=1".into()];
    match opts.max_cols {
        Some(c) => params.push(format!("width={c}")),
        None => params.push("width=auto".into()),
    }
    match opts.max_rows {
        Some(r) => params.push(format!("height={r}")),
        None => params.push("height=auto".into()),
    }
    if !opts.preserve_aspect {
        params.push("preserveAspectRatio=0".into());
    }
    if let Some(label) = &opts.label {
        let name_b64 = STANDARD.encode(label.as_bytes());
        params.push(format!("name={name_b64}"));
    }

    format!("\x1b]1337;File={}:{payload}\x07", params.join(";"))
}

/// Plain-text placeholder shown when no graphics protocol is available.
pub fn image_fallback(opts: &ImageRenderOptions) -> Vec<String> {
    let label = opts.label.as_deref().unwrap_or("Image");
    let cols = opts.max_cols.unwrap_or(40).max(8) as usize;
    let rows = opts.max_rows.unwrap_or(3).max(1) as usize;

    let inner_w = cols.saturating_sub(2);
    let mut lines = Vec::with_capacity(rows + 2);

    let bar = "─".repeat(inner_w);
    lines.push(format!("┌{bar}┐"));

    let mid = rows / 2;
    for r in 0..rows {
        if r == mid {
            let text: String = label.chars().take(inner_w).collect();
            let text_w = text.chars().count();
            let pad = inner_w.saturating_sub(text_w);
            let left = pad / 2;
            let right = pad - left;
            lines.push(format!(
                "│{}{text}{}│",
                " ".repeat(left),
                " ".repeat(right)
            ));
        } else {
            lines.push(format!("│{}│", " ".repeat(inner_w)));
        }
    }

    lines.push(format!("└{bar}┘"));
    lines
}

/// Render `data` according to the current terminal's capabilities.
///
/// Kitty/iTerm2 outputs are returned as a single line (the encoded escape
/// sequence). The fallback path returns a multi-line placeholder.
pub fn render_image(data: &[u8], opts: &ImageRenderOptions) -> Vec<String> {
    let caps = get_capabilities();
    match caps.protocol() {
        ImageProtocol::Kitty => vec![encode_kitty(allocate_image_id(), data, opts)],
        ImageProtocol::ITerm2 => vec![encode_iterm2(data, opts)],
        ImageProtocol::Fallback => image_fallback(opts),
    }
}

/// Probe `data` for image dimensions by sniffing magic bytes.
pub fn get_image_dimensions(data: &[u8]) -> Option<ImageDimensions> {
    get_png_dimensions(data)
        .or_else(|| get_jpeg_dimensions(data))
        .or_else(|| get_gif_dimensions(data))
        .or_else(|| get_webp_dimensions(data))
}

/// PNG: 8-byte signature, then IHDR chunk with width/height at offsets 16/20.
pub fn get_png_dimensions(data: &[u8]) -> Option<ImageDimensions> {
    if data.len() < 24 {
        return None;
    }
    if data[0] != 0x89 || data[1] != 0x50 || data[2] != 0x4e || data[3] != 0x47 {
        return None;
    }
    let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
    Some(ImageDimensions { width, height })
}

/// JPEG: scan for SOFn marker (0xFFC0..=0xFFC2) and read its dimensions.
pub fn get_jpeg_dimensions(data: &[u8]) -> Option<ImageDimensions> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return None;
    }
    let mut offset = 2usize;
    while offset + 9 < data.len() {
        if data[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = data[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes([data[offset + 5], data[offset + 6]]) as u32;
            let width = u16::from_be_bytes([data[offset + 7], data[offset + 8]]) as u32;
            return Some(ImageDimensions { width, height });
        }
        if offset + 3 >= data.len() {
            return None;
        }
        let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        offset = offset.checked_add(2 + length)?;
    }
    None
}

/// GIF: bytes 6-9 hold width/height in little-endian.
pub fn get_gif_dimensions(data: &[u8]) -> Option<ImageDimensions> {
    if data.len() < 10 {
        return None;
    }
    let sig = &data[..6];
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    Some(ImageDimensions { width, height })
}

/// WebP: dispatch on the chunk fourcc (VP8 / VP8L / VP8X).
pub fn get_webp_dimensions(data: &[u8]) -> Option<ImageDimensions> {
    if data.len() < 30 {
        return None;
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &data[12..16];
    if chunk == b"VP8 " {
        // Lossy: width/height are 14-bit values at byte offsets 26 and 28.
        let width = u16::from_le_bytes([data[26], data[27]]) as u32 & 0x3fff;
        let height = u16::from_le_bytes([data[28], data[29]]) as u32 & 0x3fff;
        Some(ImageDimensions { width, height })
    } else if chunk == b"VP8L" {
        // Lossless: 32-bit little-endian word at offset 21 carries (width-1, height-1).
        if data.len() < 25 {
            return None;
        }
        let bits = u32::from_le_bytes(data[21..25].try_into().ok()?);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        Some(ImageDimensions { width, height })
    } else if chunk == b"VP8X" {
        // Extended: 24-bit canvas dimensions at offsets 24 and 27, biased by 1.
        let width = (u32::from(data[24])
            | (u32::from(data[25]) << 8)
            | (u32::from(data[26]) << 16))
            + 1;
        let height = (u32::from(data[27])
            | (u32::from(data[28]) << 8)
            | (u32::from(data[29]) << 16))
            + 1;
        Some(ImageDimensions { width, height })
    } else {
        None
    }
}

/// Compute the number of terminal rows an image occupies if scaled to fit
/// the current cell width. `max_rows` clamps the result.
pub fn calculate_image_rows(image: &ImageDimensions, max_rows: Option<u16>) -> u16 {
    if image.width == 0 || image.height == 0 {
        return 1;
    }
    let cells = get_cell_dimensions();
    let cell_h = u32::from(cells.height.max(1));

    // Rows = ceil(height_px / cell_height_px).
    let mut rows = image.height.div_ceil(cell_h).max(1);
    if let Some(limit) = max_rows {
        rows = rows.min(u32::from(limit));
    }
    rows.min(u32::from(u16::MAX)) as u16
}

/// Wrap `text` in an OSC 8 hyperlink envelope.
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Kitty delete-by-id sequence (uppercase `I` also frees image data).
pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id}\x1b\\")
}

/// Kitty delete-all sequence.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A\x1b\\".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// Serializes tests that mutate the global capability cache.
    static CAPS_LOCK: Mutex<()> = Mutex::new(());

    fn fallback_caps() -> TerminalImageCapabilities {
        TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions {
                width: 8,
                height: 16,
            },
        }
    }

    #[test]
    fn allocate_image_id_returns_distinct_values() {
        let a = allocate_image_id();
        let b = allocate_image_id();
        let c = allocate_image_id();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn allocate_image_id_never_zero() {
        for _ in 0..16 {
            assert_ne!(allocate_image_id(), 0);
        }
    }

    #[test]
    fn png_dimensions_parses_known_header() {
        // Minimal PNG header: signature + IHDR length+type + 800x600 + filler.
        let mut data = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        data.extend_from_slice(&[0, 0, 0, 13]); // IHDR chunk length
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&800u32.to_be_bytes());
        data.extend_from_slice(&600u32.to_be_bytes());
        let dims = get_png_dimensions(&data).expect("png");
        assert_eq!(dims.width, 800);
        assert_eq!(dims.height, 600);
    }

    #[test]
    fn png_dimensions_rejects_bad_magic() {
        let bad = vec![0u8; 32];
        assert!(get_png_dimensions(&bad).is_none());
    }

    #[test]
    fn png_dimensions_rejects_short_input() {
        assert!(get_png_dimensions(&[]).is_none());
        assert!(get_png_dimensions(&[0x89, 0x50, 0x4e, 0x47]).is_none());
    }

    #[test]
    fn jpeg_dimensions_parses_sof0() {
        // SOI + APP0 segment with proper length, then SOF0 marker with 480x640.
        let mut data = vec![0xff, 0xd8]; // SOI
        // APP0: marker, length=16, JFIF header
        data.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
        data.extend_from_slice(b"JFIF\0");
        data.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0]); // pad to 16 bytes total
        // SOF0: marker, length=17, precision=8, height=480, width=640, components=...
        data.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        data.extend_from_slice(&480u16.to_be_bytes());
        data.extend_from_slice(&640u16.to_be_bytes());
        data.extend_from_slice(&[0u8; 10]); // dummy components/quant tables fillers
        let dims = get_jpeg_dimensions(&data).expect("jpeg");
        assert_eq!(dims.width, 640);
        assert_eq!(dims.height, 480);
    }

    #[test]
    fn jpeg_dimensions_rejects_non_jpeg() {
        assert!(get_jpeg_dimensions(&[0, 0, 0, 0]).is_none());
        assert!(get_jpeg_dimensions(&[]).is_none());
    }

    #[test]
    fn gif_dimensions_parses_gif89a() {
        let mut data = b"GIF89a".to_vec();
        data.extend_from_slice(&320u16.to_le_bytes());
        data.extend_from_slice(&240u16.to_le_bytes());
        let dims = get_gif_dimensions(&data).expect("gif");
        assert_eq!(dims.width, 320);
        assert_eq!(dims.height, 240);
    }

    #[test]
    fn gif_dimensions_parses_gif87a() {
        let mut data = b"GIF87a".to_vec();
        data.extend_from_slice(&100u16.to_le_bytes());
        data.extend_from_slice(&50u16.to_le_bytes());
        let dims = get_gif_dimensions(&data).expect("gif");
        assert_eq!(dims.width, 100);
        assert_eq!(dims.height, 50);
    }

    #[test]
    fn gif_dimensions_rejects_bad_signature() {
        assert!(get_gif_dimensions(b"NOTAGIF12345").is_none());
        assert!(get_gif_dimensions(&[]).is_none());
    }

    #[test]
    fn webp_vp8_dimensions() {
        // RIFF...WEBPVP8 ...; place dimensions at offsets 26/28.
        let mut data = vec![0u8; 32];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"WEBP");
        data[12..16].copy_from_slice(b"VP8 ");
        data[26..28].copy_from_slice(&200u16.to_le_bytes());
        data[28..30].copy_from_slice(&150u16.to_le_bytes());
        let dims = get_webp_dimensions(&data).expect("webp vp8");
        assert_eq!(dims.width, 200);
        assert_eq!(dims.height, 150);
    }

    #[test]
    fn webp_vp8l_dimensions() {
        // VP8L stores (width-1, height-1) packed into 28 bits at offset 21.
        let mut data = vec![0u8; 32];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"WEBP");
        data[12..16].copy_from_slice(b"VP8L");
        let width = 256u32 - 1;
        let height = 128u32 - 1;
        let bits = width | (height << 14);
        data[21..25].copy_from_slice(&bits.to_le_bytes());
        let dims = get_webp_dimensions(&data).expect("webp vp8l");
        assert_eq!(dims.width, 256);
        assert_eq!(dims.height, 128);
    }

    #[test]
    fn webp_vp8x_dimensions() {
        // VP8X stores 24-bit (width-1, height-1) at offsets 24 and 27.
        let mut data = vec![0u8; 32];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"WEBP");
        data[12..16].copy_from_slice(b"VP8X");
        let w_minus_1 = 1024u32 - 1;
        let h_minus_1 = 512u32 - 1;
        data[24] = (w_minus_1 & 0xff) as u8;
        data[25] = ((w_minus_1 >> 8) & 0xff) as u8;
        data[26] = ((w_minus_1 >> 16) & 0xff) as u8;
        data[27] = (h_minus_1 & 0xff) as u8;
        data[28] = ((h_minus_1 >> 8) & 0xff) as u8;
        data[29] = ((h_minus_1 >> 16) & 0xff) as u8;
        let dims = get_webp_dimensions(&data).expect("webp vp8x");
        assert_eq!(dims.width, 1024);
        assert_eq!(dims.height, 512);
    }

    #[test]
    fn webp_rejects_non_riff() {
        assert!(get_webp_dimensions(&[0u8; 32]).is_none());
        assert!(get_webp_dimensions(&[]).is_none());
    }

    #[test]
    fn get_image_dimensions_dispatches_by_magic() {
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&10u32.to_be_bytes());
        png.extend_from_slice(&20u32.to_be_bytes());
        let dims = get_image_dimensions(&png).expect("png");
        assert_eq!(dims.width, 10);
        assert_eq!(dims.height, 20);
    }

    #[test]
    fn get_image_dimensions_returns_none_for_garbage() {
        assert!(get_image_dimensions(&[0u8; 4]).is_none());
        assert!(get_image_dimensions(&[]).is_none());
    }

    #[test]
    fn encode_kitty_short_payload_produces_apc_envelope() {
        let opts = ImageRenderOptions::default();
        let s = encode_kitty(42, b"hi", &opts);
        assert!(s.starts_with("\x1b_G"));
        assert!(s.ends_with("\x1b\\"));
        assert!(s.contains("a=T"));
        assert!(s.contains("f=100"));
        assert!(s.contains("i=42"));
        // base64 of "hi" is "aGk=".
        assert!(s.contains("aGk="));
    }

    #[test]
    fn encode_kitty_includes_dimensions_when_set() {
        let opts = ImageRenderOptions {
            max_cols: Some(40),
            max_rows: Some(10),
            preserve_aspect: true,
            label: None,
        };
        let s = encode_kitty(1, b"x", &opts);
        assert!(s.contains("c=40"));
        assert!(s.contains("r=10"));
    }

    #[test]
    fn encode_kitty_chunks_large_payloads() {
        // Need >4096 base64 chars → ~3073+ bytes raw.
        let data = vec![0xa5u8; 4096];
        let s = encode_kitty(7, &data, &ImageRenderOptions::default());
        // First chunk has m=1 with main params.
        assert!(s.contains("m=1"));
        // Last chunk has m=0.
        assert!(s.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn encode_iterm2_produces_osc_1337_wrapper() {
        let opts = ImageRenderOptions::default();
        let s = encode_iterm2(b"hi", &opts);
        assert!(s.starts_with("\x1b]1337;File="));
        assert!(s.ends_with('\x07'));
        assert!(s.contains("inline=1"));
        assert!(s.contains("width=auto"));
        assert!(s.contains("height=auto"));
        assert!(s.contains("aGk="));
    }

    #[test]
    fn encode_iterm2_honors_label_and_aspect() {
        let opts = ImageRenderOptions {
            max_cols: Some(20),
            max_rows: None,
            preserve_aspect: false,
            label: Some("photo.png".to_string()),
        };
        let s = encode_iterm2(b"x", &opts);
        assert!(s.contains("width=20"));
        assert!(s.contains("preserveAspectRatio=0"));
        // base64 of "photo.png".
        let expected = STANDARD.encode(b"photo.png");
        assert!(s.contains(&format!("name={expected}")));
    }

    #[test]
    fn calculate_image_rows_basic() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions {
                width: 8,
                height: 16,
            },
        });
        let dims = ImageDimensions {
            width: 320,
            height: 320,
        };
        // 320 / 16 = 20 rows.
        assert_eq!(calculate_image_rows(&dims, None), 20);
    }

    #[test]
    fn calculate_image_rows_clamps_to_max() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions {
                width: 8,
                height: 16,
            },
        });
        let dims = ImageDimensions {
            width: 800,
            height: 800,
        };
        assert_eq!(calculate_image_rows(&dims, Some(10)), 10);
    }

    #[test]
    fn calculate_image_rows_handles_zero_dims() {
        let dims = ImageDimensions {
            width: 0,
            height: 0,
        };
        assert_eq!(calculate_image_rows(&dims, None), 1);
    }

    #[test]
    fn hyperlink_wraps_in_osc8() {
        let s = hyperlink("click", "https://example.com");
        assert_eq!(s, "\x1b]8;;https://example.com\x1b\\click\x1b]8;;\x1b\\");
    }

    #[test]
    fn hyperlink_handles_empty_text() {
        let s = hyperlink("", "https://example.com");
        assert_eq!(s, "\x1b]8;;https://example.com\x1b\\\x1b]8;;\x1b\\");
    }

    #[test]
    fn delete_kitty_image_format() {
        assert_eq!(delete_kitty_image(123), "\x1b_Ga=d,d=I,i=123\x1b\\");
    }

    #[test]
    fn delete_all_kitty_images_format() {
        assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A\x1b\\");
    }

    #[test]
    fn image_fallback_renders_label() {
        let opts = ImageRenderOptions {
            max_cols: Some(20),
            max_rows: Some(3),
            preserve_aspect: true,
            label: Some("photo".to_string()),
        };
        let lines = image_fallback(&opts);
        assert_eq!(lines.len(), 5); // top + 3 rows + bottom
        assert!(lines.iter().any(|l| l.contains("photo")));
        assert!(lines[0].starts_with('┌'));
        assert!(lines.last().unwrap().starts_with('└'));
    }

    #[test]
    fn image_fallback_default_label() {
        let lines = image_fallback(&ImageRenderOptions::default());
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Image")));
    }

    #[test]
    fn render_image_picks_kitty_when_enabled() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: true,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        });
        let lines = render_image(b"hi", &ImageRenderOptions::default());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("\x1b_G"));
    }

    #[test]
    fn render_image_picks_iterm2_when_enabled() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: false,
            iterm2: true,
            cell_dimensions: CellDimensions::default(),
        });
        let lines = render_image(b"hi", &ImageRenderOptions::default());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("\x1b]1337;File="));
    }

    #[test]
    fn render_image_falls_back_when_no_protocol() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(fallback_caps());
        let lines = render_image(b"hi", &ImageRenderOptions::default());
        assert!(lines.len() > 1);
        assert!(lines.iter().any(|l| l.contains("Image")));
    }

    #[test]
    fn capabilities_protocol_priority() {
        let caps = TerminalImageCapabilities {
            kitty: true,
            iterm2: true,
            cell_dimensions: CellDimensions::default(),
        };
        assert_eq!(caps.protocol(), ImageProtocol::Kitty);
        let caps = TerminalImageCapabilities {
            kitty: false,
            iterm2: true,
            cell_dimensions: CellDimensions::default(),
        };
        assert_eq!(caps.protocol(), ImageProtocol::ITerm2);
        let caps = TerminalImageCapabilities {
            kitty: false,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        };
        assert_eq!(caps.protocol(), ImageProtocol::Fallback);
    }

    #[test]
    fn set_and_get_cell_dimensions_round_trip() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(fallback_caps());
        set_cell_dimensions(CellDimensions {
            width: 12,
            height: 24,
        });
        let dims = get_cell_dimensions();
        assert_eq!(dims.width, 12);
        assert_eq!(dims.height, 24);
    }

    #[test]
    fn reset_capabilities_cache_forces_redetect() {
        let _g = CAPS_LOCK.lock().unwrap();
        set_capabilities(TerminalImageCapabilities {
            kitty: true,
            iterm2: false,
            cell_dimensions: CellDimensions::default(),
        });
        assert!(get_capabilities().kitty);
        reset_capabilities_cache();
        // After reset, get_capabilities re-runs detect_capabilities, which
        // depends on the host environment. Just verify it returns *something*.
        let _ = get_capabilities();
    }
}
