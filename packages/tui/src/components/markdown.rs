//! Markdown component — renders markdown with terminal styling.

use crate::tui::Component;
use crate::utils;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// Markdown renderer for terminal display.
pub struct MarkdownComponent {
    source: String,
    cache: Option<(String, u16, Vec<String>)>,
}

impl MarkdownComponent {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            cache: None,
        }
    }

    pub fn set_source(&mut self, source: impl Into<String>) {
        let new_source = source.into();
        if new_source != self.source {
            self.source = new_source;
            self.cache = None;
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    fn render_markdown(&self, width: u16) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut current_line = String::new();
        let parser = Parser::new(&self.source);

        let mut in_code_block = false;
        let mut _in_heading = false;
        let mut heading_level = 0u8;
        let mut _in_list = false;
        let mut _in_bold = false;
        let mut _in_italic = false;
        let mut _in_link = false;
        let mut list_depth: usize = 0;

        for event in parser {
            match event {
                Event::Start(tag) => match &tag {
                    Tag::Heading { level, .. } => {
                        _in_heading = true;
                        heading_level = *level as u8;
                        flush_line(&mut current_line, &mut lines);
                    }
                    Tag::CodeBlock(_) => {
                        in_code_block = true;
                        flush_line(&mut current_line, &mut lines);
                        lines.push("\x1b[90m┌───────────────────────────────────┐\x1b[0m".to_string());
                    }
                    Tag::Paragraph => {
                        if !lines.is_empty() && !lines.last().is_none_or(|l| l.is_empty()) {
                            lines.push(String::new());
                        }
                    }
                    Tag::List(_) => {
                        _in_list = true;
                        list_depth += 1;
                    }
                    Tag::Item => {
                        flush_line(&mut current_line, &mut lines);
                        let indent = "  ".repeat(list_depth.saturating_sub(1));
                        current_line = format!("{}• ", indent);
                    }
                    Tag::Strong => {
                        _in_bold = true;
                        current_line.push_str("\x1b[1m");
                    }
                    Tag::Emphasis => {
                        _in_italic = true;
                        current_line.push_str("\x1b[3m");
                    }
                    Tag::Link { .. } => {
                        _in_link = true;
                        current_line.push_str("\x1b[4;34m");
                    }
                    Tag::BlockQuote(_) => {
                        flush_line(&mut current_line, &mut lines);
                        current_line.push_str("\x1b[90m│ \x1b[0m");
                    }
                    _ => {}
                },
                Event::End(tag_end) => match &tag_end {
                    TagEnd::Heading(_) => {
                        let heading_text = std::mem::take(&mut current_line);
                        let styled = match heading_level {
                            1 => format!("\x1b[1;36m# {}\x1b[0m", heading_text),
                            2 => format!("\x1b[1;33m## {}\x1b[0m", heading_text),
                            3 => format!("\x1b[1;32m### {}\x1b[0m", heading_text),
                            _ => format!(
                                "\x1b[1m{} {}\x1b[0m",
                                "#".repeat(heading_level as usize),
                                heading_text
                            ),
                        };
                        lines.push(styled);
                        _in_heading = false;
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        flush_line(&mut current_line, &mut lines);
                        lines.push("\x1b[90m└───────────────────────────────────┘\x1b[0m".to_string());
                    }
                    TagEnd::Paragraph => {
                        flush_line(&mut current_line, &mut lines);
                    }
                    TagEnd::List(_) => {
                        _in_list = false;
                        list_depth = list_depth.saturating_sub(1);
                    }
                    TagEnd::Item => {
                        flush_line(&mut current_line, &mut lines);
                    }
                    TagEnd::Strong => {
                        _in_bold = false;
                        current_line.push_str("\x1b[0m");
                    }
                    TagEnd::Emphasis => {
                        _in_italic = false;
                        current_line.push_str("\x1b[0m");
                    }
                    TagEnd::Link => {
                        _in_link = false;
                        current_line.push_str("\x1b[0m");
                    }
                    TagEnd::BlockQuote(_) => {
                        flush_line(&mut current_line, &mut lines);
                    }
                    _ => {}
                },
                Event::Text(text) => {
                    if in_code_block {
                        // Render code with dim styling
                        for code_line in text.lines() {
                            lines.push(format!("\x1b[90m│\x1b[0m \x1b[37m{}\x1b[0m", code_line));
                        }
                    } else {
                        current_line.push_str(&text);
                    }
                }
                Event::Code(code) => {
                    current_line.push_str(&format!("\x1b[36m`{}`\x1b[0m", code));
                }
                Event::SoftBreak => {
                    current_line.push(' ');
                }
                Event::HardBreak => {
                    flush_line(&mut current_line, &mut lines);
                }
                Event::Rule => {
                    flush_line(&mut current_line, &mut lines);
                    lines.push("─".repeat(width as usize));
                }
                _ => {}
            }
        }

        flush_line(&mut current_line, &mut lines);

        // Word-wrap long lines
        let mut wrapped = Vec::new();
        for line in &lines {
            if utils::visible_width(line) > width as usize {
                wrapped.extend(utils::wrap_text(line, width as usize));
            } else {
                wrapped.push(line.clone());
            }
        }

        wrapped
    }
}

fn flush_line(current: &mut String, lines: &mut Vec<String>) {
    if !current.is_empty() {
        lines.push(std::mem::take(current));
    }
}

impl Component for MarkdownComponent {
    fn render(&self, width: u16) -> Vec<String> {
        if let Some((cached_src, cached_w, cached_lines)) = &self.cache
            && cached_src == &self.source && *cached_w == width {
                return cached_lines.clone();
            }
        self.render_markdown(width)
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(lines.iter().any(|l| l.contains("Title")));
        assert!(lines.iter().any(|l| l.contains("\x1b[1;36m"))); // H1 color
    }

    #[test]
    fn test_markdown_bold() {
        let md = MarkdownComponent::new("This is **bold** text");
        let lines = md.render(80);
        assert!(lines.iter().any(|l| l.contains("\x1b[1m"))); // Bold
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
        assert!(lines.iter().any(|l| l.contains("┌"))); // top border
        assert!(lines.iter().any(|l| l.contains("└"))); // bottom border
    }

    #[test]
    fn test_markdown_list() {
        let md = MarkdownComponent::new("- item1\n- item2\n- item3");
        let lines = md.render(80);
        assert!(lines.iter().filter(|l| l.contains("•")).count() >= 3);
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
        assert!(lines.iter().any(|l| l.contains("─")));
    }

    #[test]
    fn test_markdown_empty() {
        let md = MarkdownComponent::new("");
        let lines = md.render(80);
        // Should not crash
        assert!(lines.is_empty() || lines.iter().all(|l| l.is_empty()));
    }
}
