//! Embedded HTML pages returned by the loopback server during OAuth flows.
//!
//! The success page is shown after a code has been captured; the error page
//! is shown when something goes wrong (state mismatch, missing code, etc.).

const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#fff" fill-rule="evenodd" d="M165.29 165.29 H517.36 V400 H400 V517.36 H282.65 V634.72 H165.29 Z M282.65 282.65 V400 H400 V282.65 Z"/><path fill="#fff" d="M517.36 400 H634.72 V634.72 H517.36 Z"/></svg>"##;

const STYLE: &str = r#":root {
  --text: #fafafa;
  --text-dim: #a1a1aa;
  --page-bg: #09090b;
  --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
  --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
}
* { box-sizing: border-box; }
html { color-scheme: dark; }
body {
  margin: 0;
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: var(--page-bg);
  color: var(--text);
  font-family: var(--font-sans);
  text-align: center;
}
main {
  width: 100%;
  max-width: 560px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
}
.logo {
  width: 72px;
  height: 72px;
  display: block;
  margin-bottom: 24px;
}
h1 {
  margin: 0 0 10px;
  font-size: 28px;
  line-height: 1.15;
  font-weight: 650;
  color: var(--text);
}
p {
  margin: 0;
  line-height: 1.7;
  color: var(--text-dim);
  font-size: 15px;
}
.details {
  margin-top: 16px;
  font-family: var(--font-mono);
  font-size: 13px;
  color: var(--text-dim);
  white-space: pre-wrap;
  word-break: break-word;
}"#;

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let title = escape_html(title);
    let heading = escape_html(heading);
    let message = escape_html(message);
    let details_block = details
        .map(|d| format!("<div class=\"details\">{}</div>", escape_html(d)))
        .unwrap_or_default();

    format!(
        "<!doctype html>\n\
<html lang=\"en\">\n\
<head>\n\
  <meta charset=\"utf-8\" />\n\
  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
  <title>{title}</title>\n\
  <style>{STYLE}</style>\n\
</head>\n\
<body>\n\
  <main>\n\
    <div class=\"logo\">{LOGO_SVG}</div>\n\
    <h1>{heading}</h1>\n\
    <p>{message}</p>\n\
    {details_block}\n\
  </main>\n\
</body>\n\
</html>"
    )
}

/// HTML returned by the loopback after a successful code capture.
pub fn success_html(message: &str) -> String {
    render_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

/// HTML returned by the loopback when the OAuth flow fails.
pub fn error_html(message: &str, details: Option<&str>) -> String {
    render_page(
        "Authentication failed",
        "Authentication failed",
        message,
        details,
    )
}

/// Static success page used when callers don't want to format a custom message.
pub const SUCCESS_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Authentication successful</title></head><body><h1>Authentication successful</h1><p>You can close this window.</p></body></html>";
