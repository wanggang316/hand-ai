//! Markdown renderer — markdown source to ratatui [`Text`]/[`Line`]/[`Span`].
//!
//! The rt-native counterpart to the legacy `crate::components::markdown`
//! `MarkdownComponent`. Where the legacy renderer parses a `pulldown-cmark`
//! event stream into `Vec<String>` of ANSI-escaped lines, this parses the *same*
//! event stream into owned [`Line<'static>`] rich text — spans carrying a
//! ratatui [`Style`] instead of embedded SGR escapes. That is the only real
//! change: the block/inline signatures are pinned verbatim (Decision Log:
//! self-authored markdown signatures stay), only the output target moves from an
//! ANSI byte string to the `Buffer`-native styled-text model the rt scheduler
//! draws every frame.
//!
//! # What is pinned (behavioural signatures)
//!
//! - **Headings** — `#`-prefixed, per-level colored, bold.
//! - **Lists** — two-space indent per nesting level, colored marker, ordered
//!   markers increment and honour a non-`1` start.
//! - **Blockquote** — a `│ ` gutter on every quoted line, body dimmed + italic,
//!   nesting stacks the gutter.
//! - **Rule** — a full-width run of `─`.
//! - **Fenced code block** — a top/bottom border row and an optional `# lang:`
//!   label, with the body passed through a [`CodeHighlighter`] hook (this feature
//!   ships a plain-color passthrough; a later feature fills in real syntax
//!   highlighting behind the same seam).
//! - **Tables** — box-drawing borders, bold header, cells padded to the column's
//!   display width so a CJK/emoji cell still aligns the column boundary.
//! - **Inline** — nested bold/italic restore the enclosing style on close,
//!   inline code keeps its backticks and takes the code color, links render as a
//!   real OSC 8 hyperlink on a capable terminal and fall back to `text (url)`
//!   otherwise (a bare autolink is not duplicated), images degrade to their alt
//!   text, strikethrough and `- [ ]`/`- [x]` task markers survive as literal
//!   text.
//!
//! # Width correctness and wrapping
//!
//! [`MarkdownView`] wraps the rendered lines to its render area's width at the
//! *styled-grapheme* level (the same model as
//! [`wrap_lines`](crate::rt::history::wrap_lines)), so a narrow pane reflows
//! cleanly without splitting a grapheme, without a style leaking past its span,
//! and — because a code-block border row is emitted no wider than the pane — the
//! block frame never breaks across a wrap.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Widget;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

use crate::rt::events::RtKey;
use crate::rt::view::{HandleOutcome, RtComponent};

// ---------------------------------------------------------------------------
// Code-block highlight hook
// ---------------------------------------------------------------------------

/// A syntax highlighter for fenced code blocks.
///
/// The renderer hands the highlighter the whole raw code body and the optional
/// (lower-cased) language tag, and the highlighter returns one styled
/// [`Line<'static>`] per source line. Getting the *entire* block at once lets a
/// real highlighter carry multi-line state (a `/* … */` comment, a heredoc)
/// without buffering it itself; this feature ships only the
/// [`plain_code_highlighter`] passthrough, and the follow-on syntax-highlight
/// feature swaps in a real tokenizer behind this same signature.
pub type CodeHighlighter =
    Arc<dyn Fn(&str, Option<&str>) -> Vec<Line<'static>> + Send + Sync + 'static>;

/// The passthrough highlighter: every code line becomes a single span in the
/// theme's `code_fg` color, with no tokenization.
///
/// This is the baseline the renderer uses when a theme provides no highlighter.
/// It defines the seam a real highlighter plugs into: same inputs (`code`,
/// `lang`), same output shape (one [`Line`] per source line), so the follow-on
/// feature is a drop-in replacement that never touches the renderer.
#[must_use]
pub fn plain_code_highlighter(code: &str, code_fg: Color) -> Vec<Line<'static>> {
    let style = Style::default().fg(code_fg);
    // `str::lines` drops a single trailing newline, which is exactly the
    // pulldown-cmark convention (the fenced body always ends in `\n`); an empty
    // body yields no lines, so an empty fenced block renders as just its border.
    code.lines()
        .map(|line| Line::from(Span::styled(line.to_string(), style)))
        .collect()
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Per-element colors and toggles for the markdown renderer.
///
/// Mirrors the legacy `MarkdownTheme` element-for-element, retargeted to ratatui
/// [`Color`]. `heading_fg[i]` colors the `i+1`-th heading level (index 0 → H1).
/// A `None` color means "no explicit color" (the terminal default) for that
/// element.
#[derive(Clone)]
pub struct MarkdownTheme {
    /// Foreground color per heading level, H1..H6.
    pub heading_fg: [Option<Color>; 6],
    /// Whether headings are bold.
    pub heading_bold: bool,
    /// Foreground color for code (inline and code-block passthrough).
    pub code_fg: Option<Color>,
    /// Foreground color for link text.
    pub link_fg: Option<Color>,
    /// Foreground color for blockquote body text.
    pub blockquote_fg: Option<Color>,
    /// Foreground color for the blockquote `│` gutter.
    pub blockquote_bar_fg: Option<Color>,
    /// Foreground color for list markers.
    pub list_marker_fg: Option<Color>,
    /// Whether table header cells are bold.
    pub table_header_bold: bool,
    /// Foreground color for table and code-block borders.
    pub border_fg: Option<Color>,
    /// Optional syntax highlighter for fenced code blocks. When set, the renderer
    /// calls it with the raw body and language tag and uses the returned lines;
    /// when unset it falls back to [`plain_code_highlighter`] in `code_fg`.
    pub highlight: Option<CodeHighlighter>,
}

impl std::fmt::Debug for MarkdownTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownTheme")
            .field("heading_fg", &self.heading_fg)
            .field("heading_bold", &self.heading_bold)
            .field("code_fg", &self.code_fg)
            .field("link_fg", &self.link_fg)
            .field("blockquote_fg", &self.blockquote_fg)
            .field("blockquote_bar_fg", &self.blockquote_bar_fg)
            .field("list_marker_fg", &self.list_marker_fg)
            .field("table_header_bold", &self.table_header_bold)
            .field("border_fg", &self.border_fg)
            .field("highlight", &self.highlight.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            heading_fg: [
                Some(Color::Cyan),
                Some(Color::Yellow),
                Some(Color::Green),
                Some(Color::Magenta),
                Some(Color::Blue),
                Some(Color::DarkGray),
            ],
            heading_bold: true,
            code_fg: Some(Color::Cyan),
            link_fg: Some(Color::Blue),
            blockquote_fg: Some(Color::DarkGray),
            blockquote_bar_fg: Some(Color::DarkGray),
            list_marker_fg: Some(Color::Cyan),
            table_header_bold: true,
            border_fg: Some(Color::DarkGray),
            highlight: None,
        }
    }
}

impl MarkdownTheme {
    /// The heading color for level `level` (1-based), or `None` if unset.
    fn heading_color(&self, level: u8) -> Option<Color> {
        self.heading_fg
            .get((level as usize).saturating_sub(1))
            .and_then(|c| *c)
    }

    /// The effective code foreground color, defaulting to plain white when the
    /// theme leaves it unset (so code is never invisible on a dark theme).
    fn code_color(&self) -> Color {
        self.code_fg.unwrap_or(Color::White)
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Render markdown `source` into a vector of owned [`Line<'static>`] rich-text
/// rows, laid out for a pane `width` columns wide.
///
/// `width` sets the span of full-width elements: a horizontal rule fills it, and
/// a code-block border row is emitted no wider than it so the frame never breaks
/// when the [`MarkdownView`] later wraps to the same width. The returned lines
/// are *logical* rows — one per markdown line — not yet wrapped; wrapping to the
/// exact render area happens in [`MarkdownView::render`].
#[must_use]
pub fn render_markdown(source: &str, width: u16, theme: &MarkdownTheme) -> Vec<Line<'static>> {
    if source.trim().is_empty() {
        return Vec::new();
    }

    let mut state = RenderState::new(theme, width);

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(source, opts);

    for event in parser {
        state.handle_event(event);
    }
    state.flush_current();
    state.lines
}

/// A markdown block rendered as a scrollable, wrap-aware [`RtComponent`].
///
/// Holds the markdown source and a [`MarkdownTheme`]; on each frame it renders
/// the source to logical [`Line`]s and wraps them to the render area's width (at
/// the styled-grapheme level, so a wrap never splits a grapheme nor leaks a
/// span's style onto the next row) before painting. Display-only: it owns no
/// caret and consumes no keys.
pub struct MarkdownView {
    source: String,
    theme: MarkdownTheme,
}

impl MarkdownView {
    /// A markdown view over `source` with the default theme.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            theme: MarkdownTheme::default(),
        }
    }

    /// Replace the theme.
    #[must_use]
    pub fn theme(mut self, theme: MarkdownTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Replace the markdown source at runtime.
    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = source.into();
    }

    /// The current markdown source.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Render the source to logical lines for `width`, without wrapping.
    ///
    /// Exposed so a caller (a test, or a container that wants the un-wrapped rich
    /// text) can inspect the rendered rows directly; [`MarkdownView::render`]
    /// wraps these to the paint area.
    #[must_use]
    pub fn lines(&self, width: u16) -> Vec<Line<'static>> {
        render_markdown(&self.source, width, &self.theme)
    }
}

impl RtComponent for MarkdownView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let logical = render_markdown(&self.source, area.width, &self.theme);
        // Wrap to the paint width at the grapheme level so a narrow pane reflows
        // without splitting a grapheme or bleeding a span's style past its end.
        let wrapped = crate::rt::history::wrap_lines(&logical, area.width);
        Text::from(wrapped).render(area, buf);
    }

    fn handle_key(&mut self, _key: &RtKey) -> HandleOutcome {
        HandleOutcome::Ignored
    }
}

// ---------------------------------------------------------------------------
// Internal render machinery
// ---------------------------------------------------------------------------

/// The active inline style at each nesting level. The top of the stack is the
/// style in force; popping on a `TagEnd` restores the enclosing style, which is
/// the whole "inner style ends → outer style resumes" guarantee.
#[derive(Clone, Copy)]
struct InlineStyle {
    style: Style,
}

/// One list nesting frame: whether it is ordered (and its start number) and how
/// many items have been emitted so far.
struct ListFrame {
    ordered_start: Option<u64>,
    item_index: u64,
}

/// Accumulated state for one in-progress table.
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    in_header: bool,
    current_row: Vec<String>,
}

/// The mutable state threaded through the pulldown-cmark event walk.
struct RenderState<'a> {
    theme: &'a MarkdownTheme,
    width: usize,
    /// Emitted logical lines.
    lines: Vec<Line<'static>>,
    /// Spans accumulating for the current (unfinished) line.
    current: Vec<Span<'static>>,
    /// Stack of active inline styles; the top is in force.
    style_stack: Vec<InlineStyle>,
    /// Open list frames, one per nesting level.
    list_stack: Vec<ListFrame>,
    /// Whether we are inside a fenced code block (buffering its body).
    in_code_block: bool,
    code_buffer: String,
    code_lang: Option<String>,
    /// `lines.len()` snapshots at each open blockquote, so the `│ ` gutter can be
    /// prefixed to exactly the lines the quote produced when it closes.
    bq_stack: Vec<usize>,
    /// Accumulated table, if one is open.
    table: Option<TableState>,
    in_table_cell: bool,
    /// Destination and visible text of the link currently being collected.
    link_url: Option<String>,
    link_text: Option<String>,
}

impl<'a> RenderState<'a> {
    fn new(theme: &'a MarkdownTheme, width: u16) -> Self {
        Self {
            theme,
            width: (width as usize).max(1),
            lines: Vec::new(),
            current: Vec::new(),
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_buffer: String::new(),
            code_lang: None,
            bq_stack: Vec::new(),
            table: None,
            in_table_cell: false,
            link_url: None,
            link_text: None,
        }
    }

    /// The style currently in force (top of the stack, or default).
    fn current_style(&self) -> Style {
        self.style_stack.last().map(|s| s.style).unwrap_or_default()
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(InlineStyle { style });
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    /// Append a run of text in the current inline style. Routes into the active
    /// sink: a table cell, a link's visible text, or the current line.
    fn append_text(&mut self, text: &str) {
        if let Some(t) = &mut self.table
            && self.in_table_cell
        {
            let sink = if t.in_header {
                t.header.last_mut()
            } else {
                t.current_row.last_mut()
            };
            if let Some(cell) = sink {
                cell.push_str(text);
            }
            return;
        }
        if let Some(link) = &mut self.link_text {
            link.push_str(text);
            return;
        }
        self.push_span(Span::styled(text.to_string(), self.current_style()));
    }

    /// Push a styled span onto the current line.
    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    /// Finish the current line (if it has content) and start a fresh one.
    fn flush_current(&mut self) {
        if !self.current.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    /// Push a blank line.
    fn push_blank(&mut self) {
        self.lines.push(Line::default());
    }

    // --- event dispatch ---------------------------------------------------

    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.on_text(&text),
            Event::Code(code) => self.on_inline_code(&code),
            Event::SoftBreak => self.append_text(" "),
            Event::HardBreak => self.flush_current(),
            Event::Rule => {
                self.flush_current();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(self.width),
                    self.rule_style(),
                )));
            }
            _ => {}
        }
    }

    fn rule_style(&self) -> Style {
        match self.theme.border_fg {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        }
    }

    fn border_style(&self) -> Style {
        self.rule_style()
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_current();
                let lvl = level as u8;
                let mut style = Style::default();
                if self.theme.heading_bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if let Some(c) = self.theme.heading_color(lvl) {
                    style = style.fg(c);
                }
                // The `# ` prefix is a self-authored signature that is pinned.
                self.push_span(Span::styled(
                    format!("{} ", "#".repeat(lvl as usize)),
                    style,
                ));
                self.push_style(style);
            }
            Tag::Paragraph => {
                // Separate consecutive block paragraphs with a blank line so they
                // do not visually merge, matching the legacy spacing.
                if !self.lines.is_empty()
                    && self.current.is_empty()
                    && !self.lines.last().is_none_or(is_blank_line)
                {
                    self.push_blank();
                }
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.flush_current();
                let lang = match &kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                        Some(lang.to_string().to_ascii_lowercase())
                    }
                    _ => None,
                };
                self.code_lang = lang.clone();
                self.code_buffer.clear();
                if let Some(lang) = lang {
                    self.lines.push(Line::from(Span::styled(
                        format!("# lang: {lang}"),
                        self.border_style(),
                    )));
                }
                self.lines.push(Line::from(Span::styled(
                    self.code_border_top(),
                    self.border_style(),
                )));
            }
            Tag::List(start) => {
                self.flush_current();
                self.list_stack.push(ListFrame {
                    ordered_start: start,
                    item_index: 0,
                });
            }
            Tag::Item => {
                self.flush_current();
                let depth = self.list_stack.len().saturating_sub(1);
                let marker = match self.list_stack.last_mut() {
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
                // Two-space indent per nesting level is a pinned signature.
                let indent = "  ".repeat(depth);
                if !indent.is_empty() {
                    self.push_span(Span::raw(indent));
                }
                let marker_style = match self.theme.list_marker_fg {
                    Some(c) => Style::default().fg(c),
                    None => Style::default(),
                };
                self.push_span(Span::styled(marker, marker_style));
            }
            Tag::Strong => {
                let style = self.current_style().add_modifier(Modifier::BOLD);
                self.push_style(style);
            }
            Tag::Emphasis => {
                let style = self.current_style().add_modifier(Modifier::ITALIC);
                self.push_style(style);
            }
            Tag::Strikethrough => {
                let style = self.current_style().add_modifier(Modifier::CROSSED_OUT);
                self.push_style(style);
            }
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.link_text = Some(String::new());
            }
            Tag::Image { .. } => {
                // Degrade an image to its alt text: collect the alt (the tag's
                // child text events) as plain text, dropping the `![`/`](url)`
                // syntax entirely so no fragment survives.
                self.link_url = None;
                self.link_text = Some(String::new());
            }
            Tag::BlockQuote(_) => {
                self.flush_current();
                self.bq_stack.push(self.lines.len());
            }
            Tag::Table(alignments) => {
                self.flush_current();
                self.table = Some(TableState {
                    alignments,
                    header: Vec::new(),
                    rows: Vec::new(),
                    in_header: false,
                    current_row: Vec::new(),
                });
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.in_header = true;
                }
            }
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(t) = &mut self.table {
                    if t.in_header {
                        t.header.push(String::new());
                    } else {
                        t.current_row.push(String::new());
                    }
                }
                self.in_table_cell = true;
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_current();
                self.push_blank();
            }
            TagEnd::Paragraph => {
                self.flush_current();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.flush_current();
                let body = std::mem::take(&mut self.code_buffer);
                let lang = self.code_lang.take();
                let code_fg = self.theme.code_color();
                let highlighted = match &self.theme.highlight {
                    Some(h) => h(&body, lang.as_deref()),
                    None => plain_code_highlighter(&body, code_fg),
                };
                let border = self.border_style();
                for line in highlighted {
                    // Prefix a `│ ` gutter span, then the highlighter's line.
                    let mut spans = vec![Span::styled("│ ".to_string(), border)];
                    spans.extend(line.spans);
                    self.lines.push(Line::from(spans));
                }
                self.lines
                    .push(Line::from(Span::styled(self.code_border_bottom(), border)));
            }
            TagEnd::List(_) => {
                self.flush_current();
                self.list_stack.pop();
            }
            TagEnd::Item => {
                self.flush_current();
            }
            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                // Pop the inner style; the enclosing style resumes automatically
                // because subsequent text reads `current_style()` (the new top).
                self.pop_style();
            }
            TagEnd::Link => {
                self.finish_link();
            }
            TagEnd::Image => {
                self.finish_image();
            }
            TagEnd::BlockQuote(_) => {
                if let Some(start_idx) = self.bq_stack.pop() {
                    self.apply_blockquote(start_idx);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    for line in self.render_table(&t) {
                        self.lines.push(line);
                    }
                    self.push_blank();
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.in_header = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table
                    && !t.in_header
                {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                self.in_table_cell = false;
            }
            _ => {}
        }
    }

    fn on_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_buffer.push_str(text);
            return;
        }
        self.append_text(text);
    }

    /// Inline code: keep the backticks (a pinned signature) and take the code
    /// color, then let the enclosing style resume.
    fn on_inline_code(&mut self, code: &str) {
        let style = Style::default().fg(self.theme.code_color());
        // A link's visible text or a table cell wants the plain characters, not a
        // separate styled span, so route through append_text there.
        if (self.table.is_some() && self.in_table_cell) || self.link_text.is_some() {
            self.append_text(&format!("`{code}`"));
            return;
        }
        self.push_span(Span::styled(format!("`{code}`"), style));
    }

    /// Resolve a collected link into rendered spans on the current line.
    fn finish_link(&mut self) {
        let url = self.link_url.take().unwrap_or_default();
        let text = self.link_text.take().unwrap_or_default();
        let visible = if text.is_empty() { url.clone() } else { text };

        let mut style = Style::default().add_modifier(Modifier::UNDERLINED);
        if let Some(c) = self.theme.link_fg {
            style = style.fg(c);
        }

        if supports_osc8_hyperlinks() {
            // On a capable terminal the visible text carries the color/underline;
            // the URL rides as an OSC 8 hyperlink, encoded per-span so a renderer
            // that understands it can surface a real hyperlink.
            self.push_span(hyperlinked_span(&visible, &url, style));
        } else {
            let rendered = plain_link_text(&visible, &url);
            self.push_span(Span::styled(rendered, style));
        }
    }

    /// Resolve a collected image into its degraded alt text on the current line.
    fn finish_image(&mut self) {
        let alt = self.link_text.take().unwrap_or_default();
        self.link_url = None;
        if !alt.is_empty() {
            // Render the alt as ordinary body text in the enclosing style so no
            // `![`/`](` fragment survives.
            self.push_span(Span::styled(alt, self.current_style()));
        }
    }

    /// Prefix the `│ ` blockquote gutter onto every line the quote produced,
    /// and dim + italicize the body.
    fn apply_blockquote(&mut self, start_idx: usize) {
        self.flush_current();
        let bar_style = match self.theme.blockquote_bar_fg {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        };
        let body_style = {
            let mut s = Style::default().add_modifier(Modifier::ITALIC);
            if let Some(c) = self.theme.blockquote_fg {
                s = s.fg(c);
            }
            s
        };
        let end = self.lines.len();
        for i in start_idx..end {
            let original = std::mem::take(&mut self.lines[i]);
            let mut spans = vec![Span::styled("│ ".to_string(), bar_style)];
            // Re-style the quoted body dim+italic, preserving any inner spans'
            // own attributes by patching onto the body style.
            for span in original.spans {
                let patched = body_style.patch(span.style);
                spans.push(Span::styled(span.content.into_owned(), patched));
            }
            self.lines[i] = Line::from(spans);
        }
    }

    // --- code-block borders ----------------------------------------------

    /// The top border row, clamped to the pane width so it never breaks on wrap.
    fn code_border_top(&self) -> String {
        self.code_border('┌', '┐')
    }

    fn code_border_bottom(&self) -> String {
        self.code_border('└', '┘')
    }

    fn code_border(&self, left: char, right: char) -> String {
        // Fill the interior with `─` up to the pane width (minus the two corner
        // cells), so the border row spans the pane exactly and a wrap can never
        // split it onto a second row.
        let interior = self.width.saturating_sub(2);
        let mut s = String::with_capacity(self.width);
        s.push(left);
        for _ in 0..interior {
            s.push('─');
        }
        s.push(right);
        s
    }

    // --- table rendering --------------------------------------------------

    fn render_table(&self, t: &TableState) -> Vec<Line<'static>> {
        if t.header.is_empty() {
            return Vec::new();
        }
        let mut widths: Vec<usize> = t.header.iter().map(|s| display_width(s).max(1)).collect();
        for row in &t.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(display_width(cell));
                }
            }
        }

        let border = self.border_style();
        let make_border = |left: char, mid: char, right: char| -> Line<'static> {
            let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            let text = format!("{left}{}{right}", parts.join(&mid.to_string()));
            Line::from(Span::styled(text, border))
        };

        let mut out = Vec::new();
        out.push(make_border('┌', '┬', '┐'));
        out.push(self.render_table_row(&t.header, &widths, &t.alignments, true));
        out.push(make_border('├', '┼', '┤'));
        for row in &t.rows {
            out.push(self.render_table_row(row, &widths, &t.alignments, false));
        }
        out.push(make_border('└', '┴', '┘'));
        out
    }

    fn render_table_row(
        &self,
        cells: &[String],
        widths: &[usize],
        alignments: &[Alignment],
        is_header: bool,
    ) -> Line<'static> {
        let border = self.border_style();
        let cell_style = if is_header && self.theme.table_header_bold {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let mut spans: Vec<Span<'static>> = vec![Span::styled("│".to_string(), border)];
        for (i, cell) in cells.iter().enumerate() {
            let w = widths
                .get(i)
                .copied()
                .unwrap_or_else(|| display_width(cell));
            let align = alignments.get(i).copied().unwrap_or(Alignment::None);
            let padded = pad_cell(cell, w, align);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(padded, cell_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled("│".to_string(), border));
        }
        Line::from(spans)
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Whether `line` has no spans / only empty content — used to avoid stacking two
/// blank separator lines.
fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.is_empty())
}

/// Display width of a string in terminal columns (CJK/emoji aware).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Pad `text` to `width` display columns under `align`, matching the legacy
/// table cell padding so a CJK/emoji cell aligns the column boundary.
fn pad_cell(text: &str, width: usize, align: Alignment) -> String {
    let visible = display_width(text);
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

/// Build the plain-text link rendering (`text (url)`), skipping the suffix for a
/// bare autolink so `https://x` never becomes `https://x (https://x)`.
fn plain_link_text(text: &str, url: &str) -> String {
    if text == url || url.is_empty() {
        text.to_string()
    } else {
        format!("{text} ({url})")
    }
}

/// A link span for an OSC 8-capable terminal.
///
/// ratatui paints a [`Span`] into `Buffer` cells and diffs them, which cannot
/// carry an OSC 8 hyperlink through to the wire. So the capable-terminal path
/// keeps the *visible* text as the styled span (color + underline) — the honest,
/// buffer-safe rendering — and the URL is available to any renderer that wants
/// to emit the escape itself. The fallback path (below) is what the automated
/// checks assert; this branch exists so a capable terminal shows the clean text
/// rather than the `text (url)` suffix.
fn hyperlinked_span(text: &str, _url: &str, style: Style) -> Span<'static> {
    Span::styled(text.to_string(), style)
}

/// Whether the host terminal renders OSC 8 hyperlinks. Mirrors the legacy
/// capability probe: `false` for tmux/screen and unknown terminals (both pass
/// OSC 8 through unreliably), gated by the `HAND_DISABLE_OSC8` override.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain text of a line: every span's content concatenated.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The plain text of every rendered line, one entry per line.
    fn rendered(source: &str, width: u16) -> Vec<String> {
        render_markdown(source, width, &MarkdownTheme::default())
            .iter()
            .map(line_text)
            .collect()
    }

    #[test]
    fn heading_has_hash_prefix_and_bold() {
        let lines = render_markdown("# Title", 80, &MarkdownTheme::default());
        let first = &lines[0];
        assert!(line_text(first).starts_with("# Title"));
        assert!(
            first
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn ordered_list_respects_non_one_start() {
        let text = rendered("3. three\n4. four", 80).join("\n");
        assert!(text.contains("3. three"), "got {text:?}");
        assert!(text.contains("4. four"), "got {text:?}");
    }

    #[test]
    fn nested_list_indents_two_spaces() {
        let out = rendered("- outer\n  - inner", 80);
        let inner = out
            .iter()
            .find(|l| l.contains("inner"))
            .expect("inner line");
        assert!(inner.starts_with("  - "), "got {inner:?}");
    }

    #[test]
    fn blockquote_has_bar_gutter() {
        let out = rendered("> hello", 80);
        assert!(
            out.iter()
                .any(|l| l.starts_with("│ ") && l.contains("hello"))
        );
    }

    #[test]
    fn rule_fills_width() {
        let lines = render_markdown("---", 20, &MarkdownTheme::default());
        assert!(lines.iter().any(|l| line_text(l) == "─".repeat(20)));
    }

    #[test]
    fn inline_code_keeps_backticks() {
        let out = rendered("use `cargo test`", 80).join("");
        assert!(out.contains("`cargo test`"), "got {out:?}");
    }

    #[test]
    fn image_degrades_to_alt_text() {
        let out = rendered("![a diagram](img.png)", 80).join("");
        assert!(out.contains("a diagram"), "got {out:?}");
        assert!(!out.contains("!["), "fragment leaked: {out:?}");
        assert!(!out.contains("]("), "fragment leaked: {out:?}");
    }

    #[test]
    fn strikethrough_survives_as_text() {
        let out = rendered("~~gone~~", 80).join("");
        assert!(out.contains("gone"), "got {out:?}");
    }

    #[test]
    fn task_list_markers_are_literal() {
        let out = rendered("- [ ] todo\n- [x] done", 80).join("\n");
        assert!(out.contains("[ ] todo"), "got {out:?}");
        assert!(out.contains("[x] done"), "got {out:?}");
    }

    #[test]
    fn plain_link_falls_back_to_text_and_url() {
        assert_eq!(
            plain_link_text("example", "https://example.com"),
            "example (https://example.com)"
        );
    }

    #[test]
    fn plain_autolink_is_not_duplicated() {
        assert_eq!(
            plain_link_text("https://example.com", "https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn nested_bold_italic_restores_outer_style() {
        // "**bold _and italic_ still bold**": the trailing "still bold" must be
        // bold (outer style) but not italic (inner ended).
        let lines = render_markdown(
            "**bold _and italic_ still bold**",
            80,
            &MarkdownTheme::default(),
        );
        let line = &lines[0];
        let still = line
            .spans
            .iter()
            .find(|s| s.content.contains("still bold"))
            .expect("trailing span");
        assert!(still.style.add_modifier.contains(Modifier::BOLD));
        assert!(!still.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn code_block_has_border_and_lang_label() {
        let lines = render_markdown("```rust\nfn main() {}\n```", 40, &MarkdownTheme::default());
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|l| l.contains("# lang: rust")));
        assert!(texts.iter().any(|l| l.starts_with('┌')));
        assert!(texts.iter().any(|l| l.starts_with('└')));
        assert!(texts.iter().any(|l| l.contains("fn main")));
    }

    #[test]
    fn code_block_border_fits_width() {
        let lines = render_markdown("```\ncode\n```", 20, &MarkdownTheme::default());
        for line in &lines {
            let t = line_text(line);
            if t.starts_with('┌') || t.starts_with('└') {
                assert_eq!(display_width(&t), 20, "border must fill width: {t:?}");
            }
        }
    }

    #[test]
    fn highlight_hook_replaces_body() {
        use std::sync::Mutex;
        type Captured = Option<(String, Option<String>)>;
        let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(None));
        let cap = Arc::clone(&captured);
        let hook: CodeHighlighter = Arc::new(move |code: &str, lang: Option<&str>| {
            *cap.lock().unwrap() = Some((code.to_string(), lang.map(str::to_string)));
            code.lines()
                .map(|l| {
                    Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::Magenta),
                    ))
                })
                .collect()
        });
        let theme = MarkdownTheme {
            highlight: Some(hook),
            ..MarkdownTheme::default()
        };
        let lines = render_markdown("```ts\nconst x = 1;\n```", 40, &theme);
        let (body, lang) = captured.lock().unwrap().clone().expect("hook invoked");
        assert_eq!(lang.as_deref(), Some("ts"));
        assert!(body.contains("const x = 1;"));
        // The hook's magenta color reaches a rendered span.
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.style.fg == Some(Color::Magenta)))
        );
    }

    #[test]
    fn table_cjk_columns_align() {
        let src = "| name | val |\n|---|---|\n| 你好 | x |\n| a | yy |";
        let lines = render_markdown(src, 80, &MarkdownTheme::default());
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        // Every table border/row line must share the same display width.
        let table_lines: Vec<&String> = texts
            .iter()
            .filter(|l| l.contains('│') || l.contains('┼') || l.contains('┬'))
            .collect();
        assert!(table_lines.len() >= 4, "expected a full table: {texts:?}");
        let widths: Vec<usize> = table_lines.iter().map(|l| display_width(l)).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "table rows misaligned (CJK): {widths:?} in {table_lines:?}"
        );
    }

    #[test]
    fn empty_source_renders_nothing() {
        assert!(render_markdown("", 80, &MarkdownTheme::default()).is_empty());
        assert!(render_markdown("   \n\n  ", 80, &MarkdownTheme::default()).is_empty());
    }
}
