//! Session export — JSONL and HTML export of sessions.

use crate::core::error::CodingAgentError;
use crate::core::session_manager::SessionManager;
use model::Message;
use pulldown_cmark::{Event, Options, Parser};
use std::path::Path;

/// Export a session to JSONL format (copy the raw session file).
pub fn export_to_jsonl(session: &SessionManager, output: &Path) -> Result<(), CodingAgentError> {
    let source = session.path();
    if source.as_os_str().is_empty() {
        return Err(CodingAgentError::Session(
            "Cannot export an in-memory session to JSONL".into(),
        ));
    }
    std::fs::copy(source, output)
        .map_err(|e| CodingAgentError::Session(format!("Failed to export JSONL: {}", e)))?;
    Ok(())
}

/// Export messages to a simple HTML file.
pub fn export_to_html(
    messages: &[Message],
    session_id: &str,
    model_id: &str,
    output: &Path,
) -> Result<(), CodingAgentError> {
    let session_id_esc = escape_html(session_id);
    let model_id_esc = escape_html(model_id);
    let mut html = String::new();
    html.push_str(&format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Hand Session — {session_id_esc}</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; max-width: 800px; margin: 0 auto; padding: 2rem; background: #1a1a2e; color: #eee; }}
.header {{ border-bottom: 1px solid #333; padding-bottom: 1rem; margin-bottom: 2rem; }}
.header h1 {{ margin: 0; font-size: 1.5rem; color: #7c3aed; }}
.header .meta {{ color: #888; font-size: 0.85rem; margin-top: 0.5rem; }}
.message {{ margin: 1rem 0; padding: 1rem; border-radius: 8px; }}
.user {{ background: #16213e; border-left: 3px solid #7c3aed; }}
.assistant {{ background: #1a1a2e; border-left: 3px solid #06b6d4; }}
.tool-result {{ background: #0f3460; border-left: 3px solid #e94560; font-size: 0.9rem; }}
.role {{ font-weight: 600; font-size: 0.8rem; text-transform: uppercase; margin-bottom: 0.5rem; }}
.user .role {{ color: #7c3aed; }}
.assistant .role {{ color: #06b6d4; }}
.tool-result .role {{ color: #e94560; }}
.content {{ word-break: break-word; }}
/* Tool-result and thinking bodies keep verbatim whitespace — they
   inherit pre-wrap from .verbatim, which markdown-rendered bodies
   deliberately do NOT inherit (otherwise prose would never reflow). */
.verbatim {{ white-space: pre-wrap; }}
.markdown p {{ margin: 0.5em 0; }}
.markdown ul, .markdown ol {{ padding-left: 1.5em; }}
.markdown a {{ color: #06b6d4; }}
.markdown blockquote {{ border-left: 3px solid #444; margin: 0.5em 0; padding: 0 0 0 1em; color: #bbb; }}
.markdown table {{ border-collapse: collapse; margin: 0.5em 0; }}
.markdown th, .markdown td {{ border: 1px solid #333; padding: 4px 8px; }}
.markdown h1, .markdown h2, .markdown h3 {{ margin-top: 1em; }}
.markdown img {{ max-width: 100%; height: auto; }}
code {{ background: #0d1117; padding: 2px 6px; border-radius: 4px; font-size: 0.9em; }}
pre {{ background: #0d1117; padding: 1rem; border-radius: 8px; overflow-x: auto; }}
pre code {{ background: none; padding: 0; }}
</style>
</head>
<body>
<div class="header">
<h1>Hand Session</h1>
<div class="meta">Session: {session_id_esc} &bull; Model: {model_id_esc}</div>
</div>
"#
    ));

    for msg in messages {
        match msg {
            Message::User(u) => {
                let raw = match &u.content {
                    model::UserContent::Text(s) => s.clone(),
                    model::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|c| match c {
                            model::UserContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                let rendered = render_markdown(&raw);
                html.push_str(&format!(
                    "<div class=\"message user\"><div class=\"role\">User</div><div class=\"content markdown\">{rendered}</div></div>\n"
                ));
            }
            Message::Assistant(a) => {
                let mut parts = Vec::new();
                for block in &a.content {
                    match block {
                        model::AssistantContentBlock::Text(t) => {
                            parts.push(render_markdown(&t.text));
                        }
                        model::AssistantContentBlock::ToolCall(tc) => {
                            parts.push(format!(
                                "<code>[Tool: {} — {}]</code>",
                                escape_html(&tc.name),
                                escape_html(
                                    &serde_json::to_string(&tc.arguments).unwrap_or_default()
                                )
                            ));
                        }
                        model::AssistantContentBlock::Thinking(th) => {
                            parts.push(format!(
                                "<details><summary>Thinking</summary><pre>{}</pre></details>",
                                escape_html(&th.thinking)
                            ));
                        }
                    }
                }
                html.push_str(&format!(
                    "<div class=\"message assistant\"><div class=\"role\">Assistant</div><div class=\"content markdown\">{}</div></div>\n",
                    parts.join("\n")
                ));
            }
            Message::ToolResult(tr) => {
                let text = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        model::ToolResultContent::Text(t) => Some(escape_html(&t.text)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let error_marker = if tr.is_error { " (error)" } else { "" };
                html.push_str(&format!(
                    "<div class=\"message tool-result\"><div class=\"role\">Tool Result: {}{}</div><div class=\"content verbatim\">{}</div></div>\n",
                    escape_html(&tr.tool_name),
                    error_marker,
                    text,
                ));
            }
        }
    }

    html.push_str("</body>\n</html>\n");

    std::fs::write(output, html)
        .map_err(|e| CodingAgentError::Session(format!("Failed to write HTML: {}", e)))?;
    Ok(())
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render markdown into a safe HTML fragment for use inside a
/// `<div class="content markdown">` block.
///
/// We strip raw HTML events so untrusted LLM output cannot inject
/// `<script>`, `<iframe>`, JavaScript URLs, or other arbitrary
/// elements into the exported document. Tables, strikethrough, task
/// lists, and footnotes are enabled — they're the GitHub-flavoured
/// markdown features users actually exercise (#65). The output
/// fragment is meant to live inside a styled container, not as a
/// standalone document.
fn render_markdown(input: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    // Rewrite raw-HTML events into escaped Text events. The two
    // legitimate sources of <…> in chat output are (a) the user
    // genuinely typing `<world>` and (b) the LLM occasionally
    // emitting tags inside prose. Both should appear as literal
    // characters — the user shouldn't see arbitrary tags from an
    // LLM execute, and the user's own `<world>` shouldn't vanish.
    // Escaping (not dropping) handles both.
    // html::push_html escapes Text-event contents on its own, so
    // we forward the raw `<...>` string and let the renderer turn
    // it into `&lt;...&gt;` — no double-escaping.
    let parser = Parser::new_ext(input, options).map(|ev| match ev {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        // Math events render as TeX source by default; drop the
        // markers and keep the text so old exports still parse the
        // string instead of erroring.
        Event::DisplayMath(s) | Event::InlineMath(s) => Event::Text(s),
        other => other,
    });

    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::UserMessage;
    use tempfile::TempDir;

    #[test]
    fn test_export_html() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("session.html");

        let messages = vec![
            Message::User(UserMessage::new_text("Hello <world>")),
            Message::User(UserMessage::new_text("How are you?")),
        ];

        export_to_html(&messages, "test-session", "test-model", &output).unwrap();

        let content = std::fs::read_to_string(&output).unwrap();
        assert!(content.contains("Hand Session"));
        assert!(content.contains("Hello &lt;world&gt;"));
        assert!(content.contains("How are you?"));
    }

    /// Issue #65: HTML export must RENDER markdown in user and
    /// assistant text blocks, not dump the raw source. Verified
    /// downstream in Chrome via DevTools (zero <code>/<pre>/<table>/<a>
    /// elements before the fix). Here we just grep the output HTML
    /// for the structural elements the markdown source asks for.
    #[test]
    fn export_html_renders_markdown_to_html_elements() {
        use model::types::{Api, Provider};
        use model::{AssistantContentBlock, AssistantMessage, TextContent, Usage};

        let dir = TempDir::new().unwrap();
        let output = dir.path().join("session.html");

        // Cover every markdown feature the reporter checked in Chrome.
        let md = r#"# Heading

A paragraph with **bold**, *italic*, and `inline code`.

```rust
fn main() {}
```

- list item one
- list item two

1. ordered
2. ordered too

| Feature | Rust |
|---------|------|
| safety  | yes  |

> a blockquote

[a link](https://example.com)
"#;

        let messages = vec![Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new(md))],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "claude-test".into(),
            usage: Usage::default(),
            stop_reason: model::StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        })];

        export_to_html(&messages, "sess", "claude-test", &output).unwrap();
        let html = std::fs::read_to_string(&output).unwrap();

        for needle in [
            "<h1",
            "<strong>bold</strong>",
            "<em>italic</em>",
            "<code>inline code</code>",
            "<pre>",
            "<ul>",
            "<ol>",
            "<table>",
            "<blockquote>",
            "<a href=\"https://example.com\"",
        ] {
            assert!(
                html.contains(needle),
                "rendered HTML missing {needle:?}; got: {html}"
            );
        }

        // And the raw markdown source must NOT survive as literal
        // text — the user reporter saw "##" headers and "```"
        // fences leak through pre-fix.
        assert!(!html.contains("```rust"), "raw fence leaked: {html}");
    }

    /// Issue #65 follow-on: the markdown renderer must NOT let an
    /// LLM-authored `<script>` tag survive as real HTML. It should
    /// appear as escaped text so the browser displays it literally.
    #[test]
    fn export_html_escapes_inline_html_in_markdown() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("session.html");

        let messages = vec![Message::User(model::UserMessage::new_text(
            "Look: <script>alert('x')</script> and <iframe src=\"bad\"></iframe>",
        ))];

        export_to_html(&messages, "sess", "claude-test", &output).unwrap();
        let html = std::fs::read_to_string(&output).unwrap();

        // The raw `<script>` and `<iframe>` strings appear escaped,
        // not as live elements.
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;iframe"));
        // And a real <script>/<iframe> open tag must NOT live in the
        // body — only the CSS / meta head ever uses tag names. Probe
        // specifically for the inputs we passed in to keep the
        // assertion tight.
        let body_start = html.find("<body>").unwrap_or(0);
        let body = &html[body_start..];
        assert!(!body.contains("<script>"), "live <script> survived: {body}");
        assert!(!body.contains("<iframe"), "live <iframe> survived: {body}");
    }

    /// Issue #19: HTML export was claimed to render assistant messages
    /// as User and to drop them altogether. The renderer code path
    /// itself does the right thing — pin it so any future refactor of
    /// `export_to_html` doesn't quietly regress, and so we can localise
    /// the real bug (which lives upstream in `session.messages()` /
    /// session persistence, not in the rendering pass) without
    /// guessing.
    #[test]
    fn export_html_renders_user_assistant_and_tool_result_distinctly() {
        use model::types::{
            Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent,
            ToolResultContent, ToolResultMessage, Usage,
        };

        let dir = TempDir::new().unwrap();
        let output = dir.path().join("mixed.html");
        let messages = vec![
            Message::User(UserMessage::new_text("Remember 42")),
            Message::Assistant(AssistantMessage {
                role: "assistant".into(),
                content: vec![AssistantContentBlock::Text(TextContent::new("Got it."))],
                api: Api::OpenAICompletions,
                provider: Provider::OpenAI,
                model: "gpt-4o-mini".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
                response_model: None,
                response_id: None,
                diagnostics: None,
            }),
            Message::User(UserMessage::new_text("What did I say?")),
            Message::Assistant(AssistantMessage {
                role: "assistant".into(),
                content: vec![AssistantContentBlock::Text(TextContent::new(
                    "You said: Remember 42.",
                ))],
                api: Api::OpenAICompletions,
                provider: Provider::OpenAI,
                model: "gpt-4o-mini".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
                response_model: None,
                response_id: None,
                diagnostics: None,
            }),
            Message::ToolResult(ToolResultMessage::new(
                "tc1",
                "read",
                vec![ToolResultContent::Text(TextContent::new("file body"))],
            )),
        ];

        export_to_html(&messages, "test-session", "gpt-4o-mini", &output).unwrap();
        let content = std::fs::read_to_string(&output).unwrap();
        let user_blocks = content.matches("class=\"message user\"").count();
        let assistant_blocks = content.matches("class=\"message assistant\"").count();
        let tool_blocks = content.matches("class=\"message tool-result\"").count();
        assert_eq!(user_blocks, 2, "expected 2 user blocks, got {user_blocks}");
        assert_eq!(
            assistant_blocks, 2,
            "expected 2 assistant blocks, got {assistant_blocks}"
        );
        assert_eq!(
            tool_blocks, 1,
            "expected 1 tool-result block, got {tool_blocks}"
        );
        // Spot-check the actual assistant text is present so we don't
        // get fooled by the count alone.
        assert!(content.contains("Got it."), "first assistant text missing");
        assert!(
            content.contains("You said: Remember 42."),
            "second assistant text missing"
        );
    }

    /// Issue #19 (serde probe): does an Assistant Message round-trip
    /// through serde_json correctly? Pin the on-wire shape so we know
    /// whether the tag is being emitted and re-parsed.
    #[test]
    fn assistant_message_serde_round_trips() {
        use model::types::{
            Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
        };
        let m = Message::Assistant(AssistantMessage {
            role: "assistant".into(),
            content: vec![AssistantContentBlock::Text(TextContent::new("hi"))],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "x".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        });
        let s = serde_json::to_string(&m).expect("serialize");
        eprintln!("[probe-#19] serialized: {s}");
        let back: Message = serde_json::from_str(&s).expect("deserialize");
        eprintln!("[probe-#19] deserialized variant: {back:?}");
        assert!(
            matches!(back, Message::Assistant(_)),
            "round-trip lost the Assistant variant: {back:?}"
        );
    }

    /// Issue #19 (root-cause probe): write a 4-message session
    /// (user/assistant/user/assistant) through SessionManager,
    /// re-open, and check `build_context()`. If this returns a
    /// User-only list, the bug is in persistence; if it returns all 4
    /// in order, the bug is in `session.messages()` upstream of the
    /// exporter.
    #[test]
    fn round_trip_session_preserves_assistant_messages_in_build_context() {
        use model::types::{
            Api, AssistantContentBlock, AssistantMessage, Provider, StopReason, TextContent, Usage,
        };

        let dir = TempDir::new().unwrap();
        let mut mgr = SessionManager::create(dir.path()).unwrap();
        let asst = |text: &str| {
            Message::Assistant(AssistantMessage {
                role: "assistant".into(),
                content: vec![AssistantContentBlock::Text(TextContent::new(text))],
                api: Api::OpenAICompletions,
                provider: Provider::OpenAI,
                model: "gpt-4o-mini".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
                response_model: None,
                response_id: None,
                diagnostics: None,
            })
        };
        mgr.append_message(Message::User(UserMessage::new_text("Remember 42")))
            .unwrap();
        mgr.append_message(asst("Got it.")).unwrap();
        mgr.append_message(Message::User(UserMessage::new_text("What did I say?")))
            .unwrap();
        mgr.append_message(asst("You said: Remember 42.")).unwrap();

        let path = mgr.path().to_path_buf();
        drop(mgr);

        let reopened = SessionManager::open(&path).expect("re-open session");
        let ctx = reopened.build_context();
        assert_eq!(ctx.len(), 4, "expected 4 messages, got {ctx:?}");
        assert!(matches!(ctx[0], Message::User(_)));
        assert!(
            matches!(ctx[1], Message::Assistant(_)),
            "second message must be assistant, got {:?}",
            ctx[1]
        );
        assert!(matches!(ctx[2], Message::User(_)));
        assert!(matches!(ctx[3], Message::Assistant(_)));

        // And end-to-end: export the rehydrated messages and confirm
        // assistant blocks land in the HTML.
        let html_out = dir.path().join("round-trip.html");
        export_to_html(&ctx, "test-session", "gpt-4o-mini", &html_out).unwrap();
        let content = std::fs::read_to_string(&html_out).unwrap();
        let user_blocks = content.matches("class=\"message user\"").count();
        let assistant_blocks = content.matches("class=\"message assistant\"").count();
        assert_eq!(user_blocks, 2);
        assert_eq!(assistant_blocks, 2);
        assert!(content.contains("Got it."));
        assert!(content.contains("You said: Remember 42."));
    }

    #[test]
    fn test_export_jsonl_in_memory_fails() {
        let session = SessionManager::in_memory();
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("session.jsonl");
        let result = export_to_jsonl(&session, &output);
        assert!(result.is_err());
    }

    #[test]
    fn test_export_jsonl_from_file() {
        let dir = TempDir::new().unwrap();
        let session = SessionManager::create(dir.path()).unwrap();
        let output = dir.path().join("exported.jsonl");
        export_to_jsonl(&session, &output).unwrap();
        assert!(output.exists());
    }

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    /// HTML export interpolates session metadata into the `<title>` and a
    /// header `<div>`. Without escaping, a session id like
    /// `</title><script>alert(1)</script>` would close the title tag and
    /// execute script at view time. Same exposure for model id.
    /// Cross-site contexts: a session HTML pulled from a shared share or
    /// loaded as a local file in a browser executes whatever the metadata
    /// contains.
    #[test]
    fn export_html_escapes_session_and_model_metadata() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("xss-meta.html");
        let session_id = "</title><script>alert('xss')</script>";
        let model_id = "evil<img src=x onerror=alert(1)>";
        export_to_html(&[], session_id, model_id, &output).unwrap();
        let content = std::fs::read_to_string(&output).unwrap();
        // Find the <body> region and inspect only the rendered portion —
        // the inline <style> block obviously contains < and > tokens, but
        // we care about whether attacker-controlled text was emitted as
        // raw HTML inside the document body.
        let body = content
            .split_once("<body>")
            .map(|(_, rest)| rest)
            .unwrap_or(&content);
        assert!(
            !body.contains("<script>"),
            "session metadata must not inject <script>: {body}"
        );
        assert!(
            !body.contains("<img "),
            "model metadata must not inject raw <img> tag: {body}"
        );
        assert!(body.contains("&lt;script&gt;alert"));
        assert!(body.contains("evil&lt;img"));
    }
}
