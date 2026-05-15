//! Markdown component — renders markdown with terminal styling.
//!
//! Uses `pulldown-cmark`'s event stream and a small set of theme colors
//! that are translated to ANSI on render. Plain-text fallback (no
//! styling) is used when a theme color is `None`.

use std::sync::Arc;

use crate::theme::{Color, NamedColor};
use crate::tui::Component;
use crate::utils;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Closure that highlights a code block by language tag. Returns the list of
/// already-ANSI-escaped lines (one per source newline) to render inside the
/// fenced block. When the highlighter cannot handle the language it should
/// return the input split on newlines — the renderer adds the default
/// foreground color itself.
pub type CodeHighlighter = Arc<dyn Fn(&str, Option<&str>) -> Vec<String> + Send + Sync + 'static>;

// ---------------------------------------------------------------------------
// Theme + default-style types
// ---------------------------------------------------------------------------

/// Per-element colors for the markdown renderer.
///
/// Any field set to `None` falls back to plain text. `heading_fg[i]` colors
/// the `i+1`-th heading level (index 0 → H1).
#[derive(Clone)]
pub struct MarkdownTheme {
    pub heading_fg: [Option<Color>; 6],
    pub heading_bold: bool,
    pub code_bg: Option<Color>,
    pub code_fg: Option<Color>,
    pub link_fg: Option<Color>,
    pub blockquote_fg: Option<Color>,
    pub blockquote_bar_fg: Option<Color>,
    pub list_marker_fg: Option<Color>,
    pub table_header_bold: bool,
    pub table_border_fg: Option<Color>,
    /// Optional syntax highlighter for fenced code blocks. When set, the
    /// renderer calls it with the raw code body and the optional language
    /// tag and uses the returned lines as-is (the highlighter is expected
    /// to emit ANSI). When unset, code lines fall back to a flat
    /// `code_fg`-colored render.
    pub highlight: Option<CodeHighlighter>,
}

impl std::fmt::Debug for MarkdownTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownTheme")
            .field("heading_fg", &self.heading_fg)
            .field("heading_bold", &self.heading_bold)
            .field("code_bg", &self.code_bg)
            .field("code_fg", &self.code_fg)
            .field("link_fg", &self.link_fg)
            .field("blockquote_fg", &self.blockquote_fg)
            .field("blockquote_bar_fg", &self.blockquote_bar_fg)
            .field("list_marker_fg", &self.list_marker_fg)
            .field("table_header_bold", &self.table_header_bold)
            .field("table_border_fg", &self.table_border_fg)
            .field("highlight", &self.highlight.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            heading_fg: [
                Some(Color::Named(NamedColor::Cyan)),
                Some(Color::Named(NamedColor::Yellow)),
                Some(Color::Named(NamedColor::Green)),
                Some(Color::Named(NamedColor::Magenta)),
                Some(Color::Named(NamedColor::Blue)),
                Some(Color::Named(NamedColor::BrightBlack)),
            ],
            heading_bold: true,
            code_bg: None,
            code_fg: Some(Color::Named(NamedColor::Cyan)),
            link_fg: Some(Color::Named(NamedColor::Blue)),
            blockquote_fg: Some(Color::Named(NamedColor::BrightBlack)),
            blockquote_bar_fg: Some(Color::Named(NamedColor::BrightBlack)),
            list_marker_fg: Some(Color::Named(NamedColor::Cyan)),
            table_header_bold: true,
            table_border_fg: Some(Color::Named(NamedColor::BrightBlack)),
            highlight: None,
        }
    }
}

/// Default text style applied to body text.
///
/// `Color` carries an owned hex string in some variants, so this struct is
/// `Clone` (not `Copy`).
#[derive(Debug, Clone, Default)]
pub struct DefaultTextStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub italic: bool,
}

// ---------------------------------------------------------------------------
// MarkdownComponent
// ---------------------------------------------------------------------------

/// Markdown renderer for terminal display.
pub struct MarkdownComponent {
    source: String,
    theme: MarkdownTheme,
    default_text_style: DefaultTextStyle,
}

impl MarkdownComponent {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            theme: MarkdownTheme::default(),
            default_text_style: DefaultTextStyle::default(),
        }
    }

    pub fn set_source(&mut self, source: impl Into<String>) {
        let new_source = source.into();
        if new_source != self.source {
            self.source = new_source;
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn set_theme(&mut self, theme: MarkdownTheme) {
        self.theme = theme;
    }

    pub fn theme(&self) -> &MarkdownTheme {
        &self.theme
    }

    pub fn set_default_style(&mut self, style: DefaultTextStyle) {
        self.default_text_style = style;
    }

    pub fn default_style(&self) -> &DefaultTextStyle {
        &self.default_text_style
    }

    fn render_markdown(&self, width: u16) -> Vec<String> {
        if self.source.is_empty() || self.source.trim().is_empty() {
            return Vec::new();
        }

        let default_prefix = self.default_style_prefix();
        let mut ctx = RenderCtx {
            theme: &self.theme,
            default_prefix: default_prefix.clone(),
            width: width as usize,
        };

        let mut state = RenderState::default();

        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(&self.source, opts);

        for event in parser {
            handle_event(event, &mut state, &mut ctx);
        }
        state.flush_paragraph_line();

        let mut wrapped = Vec::new();
        for line in state.lines.drain(..) {
            if utils::visible_width(&line) > width as usize {
                wrapped.extend(utils::wrap_text_with_ansi(&line, width as usize));
            } else {
                wrapped.push(line);
            }
        }
        wrapped
    }

    /// ANSI prefix derived from the default text style (color + italic).
    fn default_style_prefix(&self) -> String {
        let mut s = String::new();
        if let Some(fg) = &self.default_text_style.fg {
            s.push_str(&fg.to_fg_ansi());
        }
        if let Some(bg) = &self.default_text_style.bg {
            s.push_str(&bg.to_bg_ansi());
        }
        if self.default_text_style.italic {
            s.push_str("\x1b[3m");
        }
        s
    }
}

impl Component for MarkdownComponent {
    fn render(&self, width: u16) -> Vec<String> {
        self.render_markdown(width)
    }
}

// ---------------------------------------------------------------------------
// Internal render machinery
// ---------------------------------------------------------------------------

struct RenderCtx<'a> {
    theme: &'a MarkdownTheme,
    default_prefix: String,
    width: usize,
}

#[derive(Default)]
struct RenderState {
    lines: Vec<String>,
    current: String,
    /// Stack of inline style prefixes; the top is re-emitted after `\x1b[0m`
    /// resets so enclosing styling is restored.
    inline_prefix_stack: Vec<String>,
    list_stack: Vec<ListFrame>,
    heading_level: Option<u8>,
    in_code_block: bool,
    /// Buffered raw code-block text. Flushed in TagEnd::CodeBlock so the
    /// highlighter (if any) can see the entire block at once.
    code_buffer: String,
    /// Language tag from the fenced opener, if any. Lower-cased ASCII.
    code_lang: Option<String>,
    blockquote_depth: usize,
    table: Option<TableState>,
    in_table_cell: bool,
    link_url: Option<String>,
    link_text: Option<String>,
    /// Stack of `lines.len()` snapshots taken at Start(BlockQuote); used to
    /// apply the bar prefix to lines emitted before the matching End.
    bq_stack: Vec<usize>,
}

struct ListFrame {
    ordered_start: Option<u64>,
    item_index: u64,
}

struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    in_header: bool,
    current_row: Vec<String>,
}

impl RenderState {
    fn push_inline_prefix(&mut self, prefix: String) {
        self.inline_prefix_stack.push(prefix);
    }

    fn pop_inline_prefix(&mut self) {
        self.inline_prefix_stack.pop();
    }

    fn current_prefix(&self) -> &str {
        self.inline_prefix_stack
            .last()
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    fn append_text(&mut self, text: &str) {
        if let Some(t) = &mut self.table
            && self.in_table_cell
        {
            if t.in_header {
                if let Some(last) = t.header.last_mut() {
                    last.push_str(text);
                }
            } else if let Some(last) = t.current_row.last_mut() {
                last.push_str(text);
            }
            return;
        }
        if let Some(buf) = &mut self.link_text {
            buf.push_str(text);
            return;
        }
        self.current.push_str(text);
    }

    fn flush_paragraph_line(&mut self) {
        if !self.current.is_empty() {
            self.lines.push(std::mem::take(&mut self.current));
        }
    }

    fn open_blockquote(&mut self) -> usize {
        self.flush_paragraph_line();
        self.lines.len()
    }

    fn close_blockquote(&mut self, start_idx: usize, ctx: &RenderCtx) {
        self.flush_paragraph_line();
        let bar_color = ctx
            .theme
            .blockquote_bar_fg
            .as_ref()
            .map(|c| c.to_fg_ansi())
            .unwrap_or_default();
        let body_color = ctx
            .theme
            .blockquote_fg
            .as_ref()
            .map(|c| c.to_fg_ansi())
            .unwrap_or_default();
        let bar = if bar_color.is_empty() {
            "│ ".to_string()
        } else {
            format!("{bar_color}│\x1b[0m ")
        };
        for i in start_idx..self.lines.len() {
            let original = std::mem::take(&mut self.lines[i]);
            let mut new_line = String::with_capacity(bar.len() + original.len() + 8);
            new_line.push_str(&bar);
            if !body_color.is_empty() {
                new_line.push_str(&body_color);
                new_line.push_str("\x1b[3m");
            }
            new_line.push_str(&original);
            new_line.push_str("\x1b[0m");
            self.lines[i] = new_line;
        }
    }
}

fn handle_event(event: Event<'_>, state: &mut RenderState, ctx: &mut RenderCtx) {
    match event {
        Event::Start(tag) => start_tag(tag, state, ctx),
        Event::End(tag) => end_tag(tag, state, ctx),
        Event::Text(text) => on_text(&text, state, ctx),
        Event::Code(code) => on_inline_code(&code, state, ctx),
        Event::SoftBreak => state.append_text(" "),
        Event::HardBreak => state.flush_paragraph_line(),
        Event::Rule => {
            state.flush_paragraph_line();
            state.lines.push("─".repeat(ctx.width.max(1)));
        }
        _ => {}
    }
}

fn start_tag(tag: Tag<'_>, state: &mut RenderState, ctx: &mut RenderCtx) {
    match tag {
        Tag::Heading { level, .. } => {
            state.flush_paragraph_line();
            let lvl = level as u8;
            state.heading_level = Some(lvl);
            let mut prefix = String::new();
            if ctx.theme.heading_bold {
                prefix.push_str("\x1b[1m");
            }
            if let Some(c) = ctx
                .theme
                .heading_fg
                .get((lvl as usize).saturating_sub(1))
                .and_then(Option::as_ref)
            {
                prefix.push_str(&c.to_fg_ansi());
            }
            state.current.push_str(&prefix);
            state.current.push_str(&"#".repeat(lvl as usize));
            state.current.push(' ');
            state.push_inline_prefix(prefix);
        }
        Tag::Paragraph => {
            if !state.lines.is_empty()
                && !state.lines.last().is_none_or(|l| l.is_empty())
                && state.current.is_empty()
            {
                state.lines.push(String::new());
            }
            // Emit the default text style at paragraph start so body text
            // picks up `DefaultTextStyle`.
            if !ctx.default_prefix.is_empty() && state.current.is_empty() {
                state.current.push_str(&ctx.default_prefix);
            }
        }
        Tag::CodeBlock(kind) => {
            state.in_code_block = true;
            state.flush_paragraph_line();
            let lang = match &kind {
                CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                    Some(lang.to_string().to_ascii_lowercase())
                }
                _ => None,
            };
            state.code_lang = lang.clone();
            state.code_buffer.clear();
            let border = code_border(ctx);
            if let Some(lang) = lang {
                state.lines.push(format!("{border}# lang: {lang}\x1b[0m"));
            }
            state.lines.push(format!(
                "{border}┌───────────────────────────────────┐\x1b[0m"
            ));
        }
        Tag::List(start) => {
            state.flush_paragraph_line();
            state.list_stack.push(ListFrame {
                ordered_start: start,
                item_index: 0,
            });
        }
        Tag::Item => {
            state.flush_paragraph_line();
            let depth = state.list_stack.len().saturating_sub(1);
            let frame = state.list_stack.last_mut();
            let marker = match frame {
                Some(f) => match f.ordered_start {
                    Some(start) => {
                        let n = start + f.item_index;
                        f.item_index += 1;
                        format!("{n}. ")
                    }
                    None => {
                        f.item_index += 1;
                        "- ".to_string()
                    }
                },
                None => "- ".to_string(),
            };
            let indent = "  ".repeat(depth);
            state.current.push_str(&indent);
            state.current.push_str(&marker_styled(&marker, ctx));
            // Restore default text style after marker reset, so item text
            // picks up the body style.
            if !ctx.default_prefix.is_empty() {
                state.current.push_str(&ctx.default_prefix);
            }
        }
        Tag::Strong => {
            state.append_text("\x1b[1m");
            let combined = format!("{}\x1b[1m", state.current_prefix());
            state.push_inline_prefix(combined);
        }
        Tag::Emphasis => {
            state.append_text("\x1b[3m");
            let combined = format!("{}\x1b[3m", state.current_prefix());
            state.push_inline_prefix(combined);
        }
        Tag::Strikethrough => {
            state.append_text("\x1b[9m");
            let combined = format!("{}\x1b[9m", state.current_prefix());
            state.push_inline_prefix(combined);
        }
        Tag::Link { dest_url, .. } => {
            state.link_url = Some(dest_url.to_string());
            state.link_text = Some(String::new());
        }
        Tag::BlockQuote(_) => {
            let start_idx = state.open_blockquote();
            state.bq_stack.push(start_idx);
            state.blockquote_depth += 1;
        }
        Tag::Table(alignments) => {
            state.flush_paragraph_line();
            state.table = Some(TableState {
                alignments,
                header: Vec::new(),
                rows: Vec::new(),
                in_header: false,
                current_row: Vec::new(),
            });
        }
        Tag::TableHead => {
            if let Some(t) = &mut state.table {
                t.in_header = true;
            }
        }
        Tag::TableRow => {
            if let Some(t) = &mut state.table {
                t.current_row.clear();
            }
        }
        Tag::TableCell => {
            if let Some(t) = &mut state.table {
                if t.in_header {
                    t.header.push(String::new());
                } else {
                    t.current_row.push(String::new());
                }
            }
            state.in_table_cell = true;
        }
        _ => {}
    }
}

fn end_tag(tag: TagEnd, state: &mut RenderState, ctx: &mut RenderCtx) {
    match tag {
        TagEnd::Heading(_) => {
            state.append_text("\x1b[0m");
            state.flush_paragraph_line();
            state.lines.push(String::new());
            state.heading_level = None;
            state.pop_inline_prefix();
        }
        TagEnd::Paragraph => {
            if !state.current.is_empty() && !ctx.default_prefix.is_empty() {
                state.append_text("\x1b[0m");
            }
            state.flush_paragraph_line();
        }
        TagEnd::CodeBlock => {
            state.in_code_block = false;
            state.flush_paragraph_line();
            // Flush the buffered code body, either via the highlighter
            // closure (when the theme provides one) or via the legacy
            // single-color render. Splitting here gives the highlighter a
            // whole-block view so it can do multi-line stateful tokens
            // (e.g. `/*...*/` comments) without buffering itself.
            let border = code_border(ctx);
            let body = std::mem::take(&mut state.code_buffer);
            let lang = state.code_lang.take();
            let highlighted: Option<Vec<String>> = ctx
                .theme
                .highlight
                .as_ref()
                .map(|h| h(&body, lang.as_deref()));
            let code_fg = ctx
                .theme
                .code_fg
                .as_ref()
                .map(|c| c.to_fg_ansi())
                .unwrap_or_else(|| "\x1b[37m".to_string());
            let rendered_lines: Vec<String> = match highlighted {
                Some(lines) => lines
                    .into_iter()
                    .map(|line| format!("{border}│\x1b[0m {line}\x1b[0m"))
                    .collect(),
                None => body
                    .lines()
                    .map(|line| format!("{border}│\x1b[0m {code_fg}{line}\x1b[0m"))
                    .collect(),
            };
            for line in rendered_lines {
                state.lines.push(line);
            }
            state.lines.push(format!(
                "{border}└───────────────────────────────────┘\x1b[0m"
            ));
        }
        TagEnd::List(_) => {
            state.flush_paragraph_line();
            state.list_stack.pop();
        }
        TagEnd::Item => {
            if !state.current.is_empty() && !ctx.default_prefix.is_empty() {
                state.append_text("\x1b[0m");
            }
            state.flush_paragraph_line();
        }
        TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
            state.append_text("\x1b[0m");
            state.pop_inline_prefix();
            let prefix = state.current_prefix().to_string();
            if !prefix.is_empty() {
                state.append_text(&prefix);
            } else if !ctx.default_prefix.is_empty() {
                state.append_text(&ctx.default_prefix);
            }
        }
        TagEnd::Link => {
            let url = state.link_url.take().unwrap_or_default();
            let text = state.link_text.take().unwrap_or_default();
            let visible = if text.is_empty() { url.clone() } else { text };
            let mut inner = String::new();
            if let Some(c) = &ctx.theme.link_fg {
                inner.push_str(&c.to_fg_ansi());
            }
            inner.push_str("\x1b[4m");
            inner.push_str(&visible);
            inner.push_str("\x1b[0m");
            let restored = state.current_prefix().to_string();
            let hyperlinked = hyperlink(&inner, &url);
            state.current.push_str(&hyperlinked);
            if !restored.is_empty() {
                state.current.push_str(&restored);
            } else if !ctx.default_prefix.is_empty() {
                state.current.push_str(&ctx.default_prefix);
            }
        }
        TagEnd::BlockQuote(_) => {
            if let Some(start_idx) = state.bq_stack.pop() {
                state.close_blockquote(start_idx, ctx);
            }
            state.blockquote_depth = state.blockquote_depth.saturating_sub(1);
        }
        TagEnd::Table => {
            if let Some(t) = state.table.take() {
                let rendered = render_table(&t, ctx);
                state.lines.extend(rendered);
                state.lines.push(String::new());
            }
        }
        TagEnd::TableHead => {
            if let Some(t) = &mut state.table {
                t.in_header = false;
            }
        }
        TagEnd::TableRow => {
            if let Some(t) = &mut state.table
                && !t.in_header
            {
                let row = std::mem::take(&mut t.current_row);
                t.rows.push(row);
            }
        }
        TagEnd::TableCell => {
            state.in_table_cell = false;
        }
        _ => {}
    }
}

fn on_text(text: &str, state: &mut RenderState, ctx: &mut RenderCtx) {
    if state.in_code_block {
        // Buffer the raw block body; rendering happens in TagEnd::CodeBlock
        // so the highlighter (if any) gets the entire block at once.
        let _ = ctx; // ctx is still passed for symmetry with future hooks.
        state.code_buffer.push_str(text);
        return;
    }
    state.append_text(text);
}

fn on_inline_code(code: &str, state: &mut RenderState, ctx: &mut RenderCtx) {
    let mut s = String::new();
    if let Some(bg) = &ctx.theme.code_bg {
        s.push_str(&bg.to_bg_ansi());
    }
    if let Some(fg) = &ctx.theme.code_fg {
        s.push_str(&fg.to_fg_ansi());
    } else {
        s.push_str("\x1b[36m");
    }
    s.push('`');
    s.push_str(code);
    s.push('`');
    s.push_str("\x1b[0m");
    let restored = state.current_prefix().to_string();
    if !restored.is_empty() {
        s.push_str(&restored);
    } else if !ctx.default_prefix.is_empty() {
        s.push_str(&ctx.default_prefix);
    }
    state.append_text(&s);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn code_border(ctx: &RenderCtx) -> String {
    match &ctx.theme.table_border_fg {
        Some(c) => c.to_fg_ansi(),
        None => "\x1b[90m".to_string(),
    }
}

fn marker_styled(marker: &str, ctx: &RenderCtx) -> String {
    match &ctx.theme.list_marker_fg {
        Some(c) => format!("{}{}\x1b[0m", c.to_fg_ansi(), marker),
        None => marker.to_string(),
    }
}

/// Build an OSC 8 hyperlink wrapper around `text` with destination `url`,
/// or fall back to `text (url)` when the host terminal won't render OSC 8
/// reliably. tmux/screen without passthrough silently swallows the
/// sequence, dropping the URL from the rendered output; better to show
/// the URL as plain text than to lose it entirely.
fn hyperlink(text: &str, url: &str) -> String {
    hyperlink_with_support(text, url, supports_osc8_hyperlinks())
}

/// Pure helper extracted for tests: choose OSC 8 vs. plain-text fallback
/// based on the supplied support flag.
fn hyperlink_with_support(text: &str, url: &str, osc8_supported: bool) -> String {
    if osc8_supported {
        format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
    } else {
        // Suffix the URL in parens so the user can copy it. Skip the
        // suffix when text already equals the URL (autolinks) to avoid
        // `https://x (https://x)` redundancy.
        if text == url {
            text.to_string()
        } else {
            format!("{text} ({url})")
        }
    }
}

/// Whether the host terminal renders OSC 8 hyperlinks. Defaults to
/// `false` for unknown terminals and any tmux / screen multiplexer
/// session — both pass OSC 8 through to the outer terminal unreliably,
/// often swallowing the sequence entirely.
fn supports_osc8_hyperlinks() -> bool {
    if std::env::var("HAND_DISABLE_OSC8").is_ok_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    if std::env::var_os("TMUX").is_some() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    if term.starts_with("tmux") || term.starts_with("screen") {
        return false;
    }
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    matches!(
        term_program.as_str(),
        "iterm.app"
            | "wezterm"
            | "ghostty"
            | "vscode"
            | "kitty"
            | "alacritty"
            | "warpterminal"
            | "apple_terminal"
    ) || std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
        || std::env::var("ITERM_SESSION_ID").is_ok()
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

fn render_table(t: &TableState, ctx: &RenderCtx) -> Vec<String> {
    if t.header.is_empty() {
        return Vec::new();
    }
    let mut widths: Vec<usize> = t
        .header
        .iter()
        .map(|s| utils::visible_width(s).max(1))
        .collect();
    for row in &t.rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(utils::visible_width(cell));
            }
        }
    }

    let border_color = ctx
        .theme
        .table_border_fg
        .as_ref()
        .map(|c| c.to_fg_ansi())
        .unwrap_or_default();
    let border_reset = if border_color.is_empty() {
        ""
    } else {
        "\x1b[0m"
    };

    let make_border = |left: char, mid: char, right: char| -> String {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!(
            "{border_color}{left}{}{right}{border_reset}",
            parts.join(&mid.to_string())
        )
    };

    let mut out = Vec::new();
    out.push(make_border('┌', '┬', '┐'));
    out.push(render_table_row(
        &t.header,
        &widths,
        &t.alignments,
        true,
        ctx,
    ));
    out.push(make_border('├', '┼', '┤'));
    for row in &t.rows {
        out.push(render_table_row(row, &widths, &t.alignments, false, ctx));
    }
    out.push(make_border('└', '┴', '┘'));
    out
}

fn render_table_row(
    cells: &[String],
    widths: &[usize],
    alignments: &[Alignment],
    is_header: bool,
    ctx: &RenderCtx,
) -> String {
    let border_color = ctx
        .theme
        .table_border_fg
        .as_ref()
        .map(|c| c.to_fg_ansi())
        .unwrap_or_default();
    let border_reset = if border_color.is_empty() {
        ""
    } else {
        "\x1b[0m"
    };
    let pipe = format!("{border_color}│{border_reset}");

    let mut s = String::new();
    s.push_str(&pipe);
    for (i, cell) in cells.iter().enumerate() {
        let w = widths
            .get(i)
            .copied()
            .unwrap_or_else(|| utils::visible_width(cell));
        let align = alignments.get(i).copied().unwrap_or(Alignment::None);
        let padded = pad_cell(cell, w, align);
        let styled = if is_header && ctx.theme.table_header_bold {
            format!("\x1b[1m{padded}\x1b[0m")
        } else {
            padded
        };
        s.push(' ');
        s.push_str(&styled);
        s.push(' ');
        s.push_str(&pipe);
    }
    s
}

fn pad_cell(text: &str, width: usize, align: Alignment) -> String {
    let visible = utils::visible_width(text);
    if visible >= width {
        return text.to_string();
    }
    let pad = width - visible;
    match align {
        Alignment::Right => format!("{}{}", " ".repeat(pad), text),
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
        Alignment::Left | Alignment::None => format!("{}{}", text, " ".repeat(pad)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Color, NamedColor};

    fn strip(s: &str) -> String {
        utils::strip_ansi(s)
    }

    #[test]
    fn test_markdown_plain_text() {
        let md = MarkdownComponent::new("Hello world");
        let lines = md.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Hello world")));
    }

    #[test]
    fn test_markdown_heading() {
        let md = MarkdownComponent::new("# Title");
        let lines = md.render(80);
        let joined = lines.join("\n");
        assert!(strip(&joined).contains("Title"));
        assert!(joined.contains("\x1b[1m"));
    }

    #[test]
    fn test_markdown_bold() {
        let md = MarkdownComponent::new("This is **bold** text");
        let lines = md.render(80);
        assert!(lines.iter().any(|l| l.contains("\x1b[1m")));
    }

    #[test]
    fn test_markdown_code_inline() {
        let md = MarkdownComponent::new("Use `cargo test`");
        let lines = md.render(80);
        assert!(lines.iter().any(|l| l.contains("cargo test")));
    }

    #[test]
    fn test_markdown_code_block() {
        let md = MarkdownComponent::new("```\nfn main() {}\n```");
        let lines = md.render(80);
        assert!(lines.iter().any(|l| l.contains("fn main")));
        assert!(lines.iter().any(|l| l.contains('┌')));
        assert!(lines.iter().any(|l| l.contains('└')));
    }

    #[test]
    fn test_markdown_list() {
        let md = MarkdownComponent::new("- item1\n- item2\n- item3");
        let lines = md.render(80);
        assert!(lines.iter().filter(|l| strip(l).contains("- ")).count() >= 3);
    }

    #[test]
    fn test_markdown_set_source() {
        let mut md = MarkdownComponent::new("before");
        md.set_source("after");
        assert_eq!(md.source(), "after");
    }

    #[test]
    fn test_markdown_rule() {
        let md = MarkdownComponent::new("above\n\n---\n\nbelow");
        let lines = md.render(40);
        assert!(lines.iter().any(|l| l.contains('─')));
    }

    // ----- new tests -------------------------------------------------------

    #[test]
    fn test_table_simple() {
        let md = MarkdownComponent::new("| a | b |\n|---|---|\n| 1 | 2 |");
        let lines = md.render(80);
        let joined = lines.join("\n");
        assert!(joined.contains('┌'));
        assert!(joined.contains('├'));
        assert!(joined.contains('└'));
        let stripped = strip(&joined);
        assert!(stripped.contains('a'));
        assert!(stripped.contains('b'));
        assert!(stripped.contains('1'));
        assert!(stripped.contains('2'));
    }

    #[test]
    fn test_table_alignment() {
        let md =
            MarkdownComponent::new("| L | C | R |\n|:---|:---:|---:|\n| left | center | right |");
        let lines = md.render(80);
        let row = lines
            .iter()
            .map(|l| strip(l))
            .find(|l| l.contains("left"))
            .expect("data row");
        // Left align: cell starts with text right after "│ "
        assert!(row.contains("│ left"));
        // Right align: cell ends with text right before " │"
        assert!(row.contains("right │"));
        // Center align: text has a space on each side.
        assert!(row.contains(" center "));
    }

    #[test]
    fn test_link_renders_osc8() {
        // The outer renderer's choice depends on the host env (TERM,
        // TMUX, ...) which is unstable across CI shells. Pin both
        // branches directly on the pure helper instead.
        let osc8 = hyperlink_with_support("example", "https://example.com", true);
        assert!(osc8.contains("\x1b]8;;https://example.com\x1b\\"));
        assert!(osc8.contains("example"));
        assert!(osc8.ends_with("\x1b]8;;\x1b\\"));
    }

    /// tmux/screen and unknown terminals swallow OSC 8. The fallback
    /// renders `text (url)` so the URL is at least visible.
    #[test]
    fn test_link_falls_back_to_plain_text_without_osc8() {
        let plain = hyperlink_with_support("example", "https://example.com", false);
        assert_eq!(plain, "example (https://example.com)");
    }

    /// An autolink (`<https://x>` or bare URL) renders as the URL
    /// itself; the fallback must NOT duplicate it as `https://x (https://x)`.
    #[test]
    fn test_link_autolink_fallback_does_not_duplicate() {
        let plain = hyperlink_with_support("https://example.com", "https://example.com", false);
        assert_eq!(plain, "https://example.com");
    }

    #[test]
    fn test_inline_code_styled() {
        let md = MarkdownComponent::new("Use `cargo test` now");
        let lines = md.render(80);
        let joined = lines.join("");
        // Default theme code_fg = Cyan = "\x1b[36m".
        assert!(joined.contains("\x1b[36m"));
        assert!(joined.contains("`cargo test`"));
    }

    #[test]
    fn test_code_block_language_label() {
        let md = MarkdownComponent::new("```rust\nfn main() {}\n```");
        let lines = md.render(80);
        let joined = lines.join("\n");
        assert!(strip(&joined).contains("# lang: rust"));
    }

    #[test]
    fn test_code_block_highlight_hook_replaces_body() {
        // The highlight hook receives the full block body and language and
        // returns one ANSI-formatted line per input line. We verify the
        // hook is invoked with the right inputs and its output reaches the
        // rendered lines verbatim.
        use std::sync::Mutex;
        let captured: Arc<Mutex<Option<(String, Option<String>)>>> = Arc::new(Mutex::new(None));
        let cap2 = Arc::clone(&captured);
        let hook: CodeHighlighter = Arc::new(move |code: &str, lang: Option<&str>| {
            *cap2.lock().unwrap() = Some((code.to_string(), lang.map(|s| s.to_string())));
            code.lines()
                .map(|l| format!("\x1b[35m{l}\x1b[0m"))
                .collect()
        });
        let mut md = MarkdownComponent::new("```ts\nconst x = 1;\nconst y = 2;\n```");
        let mut theme = MarkdownTheme::default();
        theme.highlight = Some(hook);
        md.set_theme(theme);
        let lines = md.render(80);
        let joined = lines.join("\n");
        // The hook was invoked with the buffered body (including trailing
        // newline from pulldown-cmark) and the lowercased language tag.
        let (body, lang) = captured.lock().unwrap().clone().expect("hook invoked");
        assert_eq!(lang.as_deref(), Some("ts"));
        assert!(body.contains("const x = 1;"));
        assert!(body.contains("const y = 2;"));
        // The magenta SGR (35) from the hook output reaches the rendered
        // line. The default code_fg color path is bypassed.
        assert!(
            joined.contains("\x1b[35m"),
            "missing hook color in {joined:?}"
        );
        // Both code lines render.
        let stripped = strip(&joined);
        assert!(stripped.contains("const x = 1;"));
        assert!(stripped.contains("const y = 2;"));
    }

    #[test]
    fn test_list_nesting_two_levels() {
        let src = "- outer\n  - inner\n  - inner2\n- outer2";
        let md = MarkdownComponent::new(src);
        let lines = md.render(80);
        let inner = lines
            .iter()
            .map(|l| strip(l))
            .find(|l| l.contains("inner") && !l.contains("inner2"))
            .expect("inner line");
        assert!(inner.starts_with("  "), "expected indent, got {inner:?}");
    }

    #[test]
    fn test_blockquote_renders_bar() {
        let md = MarkdownComponent::new("> hello there");
        let lines = md.render(80);
        let joined = lines.join("\n");
        assert!(strip(&joined).contains("│ hello there"));
    }

    #[test]
    fn test_blockquote_nested() {
        let md = MarkdownComponent::new("> outer\n>\n> > inner");
        let lines = md.render(80);
        let joined = lines.join("\n");
        let s = strip(&joined);
        assert!(s.contains("│ outer"));
        assert!(s.contains("│ │ inner"));
    }

    #[test]
    fn test_heading_uses_theme_color() {
        let mut theme = MarkdownTheme::default();
        theme.heading_fg[0] = Some(Color::Named(NamedColor::Red));
        let mut md = MarkdownComponent::new("# Hello");
        md.set_theme(theme);
        let lines = md.render(80);
        assert!(lines.iter().any(|l| l.contains("\x1b[31m")));
    }

    #[test]
    fn test_default_text_style_applies_to_paragraphs() {
        let mut md = MarkdownComponent::new("just text");
        md.set_default_style(DefaultTextStyle {
            fg: Some(Color::Named(NamedColor::Magenta)),
            bg: None,
            italic: false,
        });
        let lines = md.render(80);
        let joined = lines.join("");
        assert!(joined.contains("\x1b[35m"));
    }

    #[test]
    fn test_empty_markdown() {
        let md = MarkdownComponent::new("");
        let lines = md.render(80);
        assert!(lines.is_empty() || lines.iter().all(|l| l.is_empty()));
    }

    #[test]
    fn test_markdown_with_only_whitespace() {
        let md = MarkdownComponent::new("   \n\n   ");
        let lines = md.render(80);
        assert!(lines.is_empty() || lines.iter().all(|l| strip(l).trim().is_empty()));
    }
}
