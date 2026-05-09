//! Session export — JSONL and HTML export of sessions.

use crate::core::error::CodingAgentError;
use crate::core::session_manager::SessionManager;
use model::Message;
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
    let mut html = String::new();
    html.push_str(&format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Hand Session — {session_id}</title>
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
.content {{ white-space: pre-wrap; word-break: break-word; }}
code {{ background: #0d1117; padding: 2px 6px; border-radius: 4px; font-size: 0.9em; }}
pre {{ background: #0d1117; padding: 1rem; border-radius: 8px; overflow-x: auto; }}
pre code {{ background: none; padding: 0; }}
</style>
</head>
<body>
<div class="header">
<h1>Hand Session</h1>
<div class="meta">Session: {session_id} &bull; Model: {model_id}</div>
</div>
"#
    ));

    for msg in messages {
        match msg {
            Message::User(u) => {
                let text = match &u.content {
                    model::UserContent::Text(s) => escape_html(s),
                    model::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|c| match c {
                            model::UserContentBlock::Text(t) => Some(escape_html(&t.text)),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                html.push_str(&format!(
                    "<div class=\"message user\"><div class=\"role\">User</div><div class=\"content\">{text}</div></div>\n"
                ));
            }
            Message::Assistant(a) => {
                let mut parts = Vec::new();
                for block in &a.content {
                    match block {
                        model::AssistantContentBlock::Text(t) => {
                            parts.push(escape_html(&t.text));
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
                    "<div class=\"message assistant\"><div class=\"role\">Assistant</div><div class=\"content\">{}</div></div>\n",
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
                    "<div class=\"message tool-result\"><div class=\"role\">Tool Result: {}{}</div><div class=\"content\">{}</div></div>\n",
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
}
