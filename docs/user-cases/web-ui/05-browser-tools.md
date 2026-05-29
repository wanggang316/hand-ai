# Browser Tools (JS REPL & Extract Document) — Web UI Acceptance Test Cases

Scope: end-user acceptance behavior for the two browser-executed agent tools of the hand-ai web UI — `javascript_repl` (code runs in a transient hidden sandbox iframe; console output + return value + returned files round-trip back to the agent loop) and `extract_document` (the browser fetches a document URL with a 50MB cap, parses it, and returns extracted text). Covers the server-side tool declarations, the `tool_result` round-trip via `RemoteAgent`, the two auto-registered renderers (collapsible code/console + attachment-tile chips; filename/format/size), base64 chunked file transport, the document-fetch proxy, the neutral (brand-free) CORS fallback message, and execution-error surfacing. All cases are written from the perspective of an end user who types a prompt that makes the agent decide to call one of these tools; assertions are observable in the chat UI, in the rendered tool card, or in the agent turn continuing.

---

### WUI-TOOL-01: Agent runs javascript_repl and the result round-trips back into the turn
- **Persona:** user whose prompt makes the agent call `javascript_repl`
- **Preconditions:**
  - The web UI is open and connected to the server (WebSocket established).
  - A model capable of tool use is selected.
- **Steps:**
  1. Type a prompt that requires computation, e.g. "Use the JavaScript REPL to compute the sum and average of [10, 20, 15, 25] and log them."
  2. Send the message and wait for the turn to proceed.
- **Assertions:**
  - A1: A tool card titled "Executing JavaScript" appears in the assistant turn for the `javascript_repl` call.
  - A2: The code the agent supplied is executed in the browser (not on the server); the turn does not block waiting on a server-side executor.
  - A3: After execution, the captured `console.log` output (e.g. text containing `Sum:` and `Average:`) is shown in the tool card's console area.
  - A4: A `tool_result` frame is sent back to the server keyed by the tool call id, and the agent loop resumes — the assistant produces a follow-up message that references the computed values.
  - A5: The turn reaches a normal end state (input is re-enabled); the loop does not hang.
- **Traces:** `javascript-repl.ts:executeJavaScript`, `javascript-repl.ts:createJavaScriptReplTool/execute`, `remote-agent.ts:runBrowserTool` (sends `tool_result`), matrix rows 91, 92, 99.

---

### WUI-TOOL-02: REPL renderer shows collapsible code plus console output
- **Persona:** user whose prompt makes the agent call `javascript_repl`
- **Preconditions:**
  - A completed `javascript_repl` tool call is present in the transcript (e.g. from WUI-TOOL-01).
- **Steps:**
  1. Locate the "Executing JavaScript" tool card.
  2. Click the collapsible header to expand the card.
- **Assertions:**
  - A1: The card header is collapsible (it shows a chevron affordance and a code icon).
  - A2: When collapsed, the code/console body is hidden (zero height); when expanded, the body becomes visible.
  - A3: The expanded body renders the executed JavaScript inside a syntax-highlighted code block labeled as `javascript`.
  - A4: The expanded body renders the captured console output inside a console block beneath the code.
  - A5: For a successful run the console block uses the default (non-error) variant.
- **Traces:** `javascript-repl.ts:javascriptReplRenderer.render` (`renderCollapsibleHeader`, `code-block`, `console-block`), matrix row 93.

---

### WUI-TOOL-03: REPL with no output reports a friendly success line
- **Persona:** user whose prompt makes the agent call `javascript_repl`
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to run REPL code that neither logs nor returns anything (e.g. "Run `const x = 1;` in the REPL").
- **Assertions:**
  - A1: The tool call completes successfully (the card is in a non-error/complete state).
  - A2: The console area shows the text "Code executed successfully (no output)" rather than being empty or showing an error.
  - A3: The agent turn continues normally after the result returns.
- **Traces:** `javascript-repl.ts:executeJavaScript` (`output.trim() || "Code executed successfully (no output)"`).

---

### WUI-TOOL-04: REPL return value is appended with a `=>` prefix and JSON-formatted for objects
- **Persona:** user whose prompt makes the agent call `javascript_repl`
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to run REPL code whose final value is an object, e.g. "Return `{ a: 1, b: [2, 3] }` from the REPL."
- **Assertions:**
  - A1: The console output includes a line beginning with `=> ` carrying the return value.
  - A2: An object return value is rendered as pretty-printed JSON (2-space indentation), not `[object Object]`.
  - A3: A scalar return value (e.g. a number from a separate run) is rendered with its `String()` form after `=> `.
  - A4: The full assembled text (console lines, then return value) is what round-trips to the agent as the tool result.
- **Traces:** `javascript-repl.ts:executeJavaScript` (return-value formatting with `JSON.stringify(..., null, 2)`).

---

### WUI-TOOL-05: REPL returns a downloadable file shown as an attachment-tile chip
- **Persona:** user whose prompt makes the agent call `javascript_repl` to generate a file
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to generate a file in the REPL, e.g. "Generate a CSV with two rows and return it as a downloadable file named data.csv."
  2. Wait for the tool call to complete, then expand the card.
- **Assertions:**
  - A1: The returned file is collected via `returnDownloadableFile` and reported in the assembled output text as a `[Files returned: N]` notice listing `data.csv` and its MIME type.
  - A2: The renderer shows the returned file as an attachment-tile chip beneath the code/console body.
  - A3: The chip is typed `document` for non-image files (and `image` for image MIME types), with the correct filename, MIME type, and size.
  - A4: A textual file (e.g. `text/csv`, `application/json`) has its decoded text available for inline display (extractedText populated by base64-decoding the payload).
- **Traces:** `javascript-repl.ts:filesToAttachments`, `javascript-repl.ts:fileChip` (`attachment-tile`), `javascript-repl.ts:decodeTextFile`, `prompts.ts:FILE_DOWNLOAD_RUNTIME_DESCRIPTION`, matrix row 93.

---

### WUI-TOOL-06: Returned files are base64-encoded and chunked for transport
- **Persona:** user whose prompt makes the agent call `javascript_repl` to generate a large file
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to generate a large binary file in the REPL (e.g. "Create a ~2MB binary buffer and return it as output.bin").
  2. Wait for the tool call to complete.
- **Assertions:**
  - A1: The returned file's bytes are encoded to base64 in fixed chunks (chunk size `0x8000`) so a multi-megabyte buffer does not throw a call-stack overflow.
  - A2: The serialized file in the tool-result details carries `fileName`, `mimeType`, `size`, and `contentBase64`.
  - A3: A missing/empty filename defaults to `file` and a missing MIME type defaults to `application/octet-stream`.
  - A4: The agent turn completes successfully with the large file reported, not an error.
- **Traces:** `javascript-repl.ts:encodeFiles` → `encodeFileContent`, `attachment-utils.ts` (`BASE64_CHUNK_SIZE = 0x8000`), matrix row 91.

---

### WUI-TOOL-07: REPL exposes user attachments to executed code
- **Persona:** user who attaches a file, then prompts the agent to process it in the REPL
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - The user has at least one attachment in the conversation (e.g. a CSV attached via the paperclip).
- **Steps:**
  1. Attach a CSV file to a message and ask "Use the REPL to read my attached CSV and log the number of rows."
- **Assertions:**
  - A1: The REPL run is given attachment runtime providers built from the live conversation's `user-with-attachments` messages.
  - A2: Inside the sandbox, `listAttachments()`, `readTextAttachment(id)`, and `readBinaryAttachment(id)` are available and return the attached file's metadata/content.
  - A3: The console output reflects the processed attachment (e.g. a correct row count).
  - A4: The tool description shown to the model dynamically includes the attachments helper section (so the model knows the functions exist).
- **Traces:** `chat-panel.ts:buildReplRuntimeProviders`, `javascript-repl.ts:createJavaScriptReplTool` (dynamic `description`), `prompts.ts:ATTACHMENTS_RUNTIME_DESCRIPTION`, matrix row 91.

---

### WUI-TOOL-08: REPL execution error surfaces as a tool error, not a hang
- **Persona:** user whose prompt makes the agent call `javascript_repl` with throwing code
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to run code that throws, e.g. "Run `throw new Error('boom')` in the REPL."
- **Assertions:**
  - A1: The tool card enters an error state (error styling) rather than a complete state.
  - A2: The console block uses the error variant and shows the error message (`boom`) and stack.
  - A3: A `tool_result` frame is still sent back with `isError: true`, so the agent loop resumes instead of hanging.
  - A4: The assistant can react to the failure in its follow-up message; the input is eventually re-enabled.
- **Traces:** `javascript-repl.ts:executeJavaScript` (error path throws), `javascript-repl.ts:makeJavaScriptReplExecutor` (catches → `isError: true`), `remote-agent.ts:runBrowserTool`, matrix row 92.

---

### WUI-TOOL-09: REPL execution timeout does not deadlock the turn
- **Persona:** user whose prompt makes the agent call `javascript_repl` with long-running code
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to run code that never completes (e.g. an infinite loop or an unresolved promise) in the REPL.
- **Assertions:**
  - A1: The sandbox enforces a 120-second execution timeout.
  - A2: On timeout, the result carries an error message "Execution timeout (120s)", surfaced to the user as a tool error.
  - A3: A `tool_result` (error) is still delivered, so the agent loop continues rather than hanging indefinitely.
- **Traces:** `sandboxed-iframe.ts` (`SANDBOX_EXECUTE_TIMEOUT_MS`, 120s timeout → error result), `javascript-repl.ts:executeJavaScript`.

---

### WUI-TOOL-10: Empty REPL code is rejected with a clear error
- **Persona:** user whose prompt makes the agent call `javascript_repl` with no code
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Trigger a `javascript_repl` call where the `code` argument is empty or omitted.
- **Assertions:**
  - A1: Execution fails fast with the message "Code parameter is required".
  - A2: The failure is delivered as a tool error result (`isError: true`), not a silent no-op.
  - A3: The agent loop continues after the error.
- **Traces:** `javascript-repl.ts:executeJavaScript` (`if (!code) throw`), `javascript-repl.ts:makeJavaScriptReplExecutor`.

---

### WUI-TOOL-11: The hidden REPL sandbox iframe is created and torn down per run
- **Persona:** user whose prompt makes the agent call `javascript_repl`
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Prompt the agent to run any REPL snippet.
  2. Observe the page during and after the run (no visible execution surface should remain).
- **Assertions:**
  - A1: A `<sandbox-iframe>` element is appended to the document body with `display: none` (the user never sees a code execution panel).
  - A2: After the run completes (success, error, or abort), the sandbox element is removed from the DOM (cleanup in a `finally`).
  - A3: Globals do not persist between separate REPL calls (each run uses a fresh sandbox id).
- **Traces:** `javascript-repl.ts:executeJavaScript` (`sandbox.style.display = "none"`, `document.body.appendChild`, `finally { sandbox.remove() }`), `prompts.ts:JAVASCRIPT_REPL_TOOL_DESCRIPTION` (persistence note).

---

### WUI-TOOL-12: Agent extracts text from a document URL and the renderer shows filename/format/size
- **Persona:** user whose prompt makes the agent call `extract_document`
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - A CORS-permissive document URL is reachable from the browser (e.g. a public PDF).
- **Steps:**
  1. Type "Read this document and summarize it: https://example.com/report.pdf".
  2. Send and wait for the tool call to complete.
- **Assertions:**
  - A1: A tool card with a file-text icon appears for the `extract_document` call.
  - A2: The browser fetches the URL directly, parses the document, and returns the extracted plain text as the tool result content.
  - A3: On success the card title reads `Extracted text from <fileName> (<FORMAT>, <N.N>KB)` — i.e. it shows the derived filename, the uppercased format, and the size in KB.
  - A4: The format label is derived from the document's MIME type (e.g. PDF → `PDF`, DOCX → `DOCX`, XLSX → `XLSX`, PPTX → `PPTX`).
  - A5: The agent loop resumes with the extracted text and produces a summary in its follow-up message.
- **Traces:** `extract-document.ts:createExtractDocumentTool/execute`, `extract-document.ts:formatFromMime`, `extract-document.ts:extractDocumentRenderer` (title), matrix rows 94, 95.

---

### WUI-TOOL-13: Extract-document renderer shows the URL and the extracted text in a collapsible body
- **Persona:** user whose prompt makes the agent call `extract_document`
- **Preconditions:**
  - A completed successful `extract_document` call is present (e.g. from WUI-TOOL-12).
- **Steps:**
  1. Click the tool card header to expand it.
- **Assertions:**
  - A1: The card is collapsible (chevron affordance); collapsed body has zero height, expanded body is visible.
  - A2: The expanded body shows a "URL:" label followed by the exact URL the agent passed.
  - A3: On success the extracted text is rendered inside a `plaintext` code block.
  - A4: On success no error console block is shown.
- **Traces:** `extract-document.ts:extractDocumentRenderer.render` (URL line, `code-block language="plaintext"`).

---

### WUI-TOOL-14: CORS-blocked document fetch surfaces the neutral fallback message (no brand strings)
- **Persona:** user whose prompt makes the agent call `extract_document` against a host that blocks browser reads
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - The document-fetch proxy is NOT enabled.
  - The target host blocks cross-origin browser fetches (produces a `TypeError: Failed to fetch`).
- **Steps:**
  1. Type "Extract the text from https://blocked.example.com/private.pdf".
  2. Send and wait for the tool call to resolve.
- **Assertions:**
  - A1: The fetch failure is classified as a CORS error and the tool result carries the neutral fallback message.
  - A2: The message text begins with "TELL USER: Unable to fetch the document due to cross-origin (CORS) restrictions; the server hosting the file blocks browser downloads."
  - A3: The message instructs the user to download the file manually and attach it using "the attachment button (paperclip icon) in the message input area."
  - A4: The fallback message contains NO product, brand, vendor, project, or author name of any kind — it is fully generic and refers only to "the document", "the file", "the server", and "the attachment button (paperclip icon)".
  - A5: The card renders in an error state with the message shown in an error console block; the agent loop still continues.
- **Traces:** `extract-document.ts:CORS_FALLBACK_MESSAGE`, `extract-document.ts:execute` (catch → `isCorsError` → throw fallback), `cors.ts:isCorsError`, matrix rows 96, 97.

---

### WUI-TOOL-15: isCorsError predicate classifies failures correctly
- **Persona:** user whose prompt makes the agent call `extract_document` and triggers a network failure
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Trigger fetch failures of different kinds through repeated `extract_document` attempts (a `TypeError: Failed to fetch`, a `NetworkError`, and a message mentioning "CORS"/"cross-origin").
- **Assertions:**
  - A1: A `TypeError` whose message includes "failed to fetch" is treated as a CORS error → neutral fallback shown.
  - A2: An error whose `name` is `NetworkError` is treated as a CORS error → neutral fallback shown.
  - A3: Any error whose message contains "cors" or "cross-origin" (case-insensitive) is treated as a CORS error → neutral fallback shown.
  - A4: A non-CORS error (e.g. a thrown size-limit error, or any other non-fetch error) is NOT classified as CORS and is propagated as its own message rather than the CORS fallback.
- **Traces:** `cors.ts:isCorsError`, `extract-document.ts:execute` (`if (isCorsError(fetchError)) ... else throw fetchError`), matrix row 97.

---

### WUI-TOOL-16: Non-OK HTTP response yields a brand-neutral manual-download instruction
- **Persona:** user whose prompt makes the agent call `extract_document` against a URL that returns an error status
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - The target URL is reachable but returns a non-2xx status (e.g. 403 or 404).
- **Steps:**
  1. Type "Extract https://example.com/forbidden.pdf".
  2. Send and wait for the result.
- **Assertions:**
  - A1: The tool result is an error containing "TELL USER: Unable to download the document (<status> <statusText>)." with the actual HTTP status and status text.
  - A2: The message advises that the site likely blocks automated downloads and instructs the user to attach the file manually via the paperclip icon.
  - A3: The message is brand-neutral (no product/vendor names).
  - A4: This is distinct from the CORS fallback (a reachable-but-rejecting server vs. a cross-origin-blocked fetch).
- **Traces:** `extract-document.ts:execute` (`if (!response.ok) throw ...`).

---

### WUI-TOOL-17: Document over 50MB is rejected
- **Persona:** user whose prompt makes the agent call `extract_document` against an oversized file
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - The target URL serves a document larger than 50MB.
- **Steps:**
  1. Type "Extract the text from https://example.com/huge.pdf" where the file exceeds 50MB.
- **Assertions:**
  - A1: If the response declares a `content-length` over the limit, the tool fails early with "Document is too large (<N.N>MB). Maximum supported size is 50MB." before downloading the body.
  - A2: If no/incorrect content-length is declared, the downloaded `arrayBuffer.byteLength` is re-checked against the 50MB cap and the same too-large error is raised.
  - A3: The size-limit error is delivered as a tool error and is NOT treated as a CORS error (no CORS fallback shown).
  - A4: The agent loop continues after the error.
- **Traces:** `extract-document.ts:execute` (`MAX_SIZE = 50 * 1024 * 1024`, content-length and byteLength checks), matrix row 94.

---

### WUI-TOOL-18: Unsupported document format is reported with the supported-formats list
- **Persona:** user whose prompt makes the agent call `extract_document` against an unsupported file type
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - The target URL serves a file the parser cannot extract text from (no `extractedText` produced).
- **Steps:**
  1. Type "Extract the text from https://example.com/archive.zip".
- **Assertions:**
  - A1: The fetch and parse complete, but because no extractable text is produced the tool fails with "Document format not supported."
  - A2: The error lists the supported formats: PDF (.pdf), Word (.docx), Excel (.xlsx, .xls), PowerPoint (.pptx).
  - A3: The error is delivered as a tool error and the agent loop continues.
- **Traces:** `extract-document.ts:execute` (`if (!attachment.extractedText) throw ...`).

---

### WUI-TOOL-19: Invalid or empty URL is rejected before any fetch
- **Persona:** user whose prompt makes the agent call `extract_document` with a bad URL
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Trigger `extract_document` with an empty `url`, then again with a non-URL string like "not a url".
- **Assertions:**
  - A1: An empty/whitespace URL fails with "URL is required" before any network call.
  - A2: A syntactically invalid URL fails with "Invalid URL: <value>" (validated by `new URL(url)`), before any network call.
  - A3: Both failures are delivered as tool errors; neither shows the CORS fallback.
- **Traces:** `extract-document.ts:execute` (trim + `if (!url)` + `new URL(url)` validation).

---

### WUI-TOOL-20: Document-fetch proxy is applied when configured
- **Persona:** user who enabled the document-fetch proxy, then makes the agent call `extract_document`
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
  - In settings the proxy is enabled (`proxy.enabled = true`) with a proxy URL set (`proxy.url`, e.g. `https://proxy.example.com`).
- **Steps:**
  1. Type "Extract the text from https://blocked.example.com/report.pdf".
  2. Send and wait for the fetch.
- **Assertions:**
  - A1: The browser fetches through the proxy: the requested URL is `<proxy-url>/?url=<encoded original URL>` (trailing slash on the proxy base is normalized away; the target is URL-encoded).
  - A2: When the proxy is disabled OR the proxy URL is empty, the original target URL is fetched directly.
  - A3: A failure while reading proxy settings degrades gracefully to fetching the original URL (no crash).
  - A4: With a working proxy, a host that would otherwise be CORS-blocked now succeeds and returns extracted text.
- **Traces:** `cors.ts:resolveDocumentFetchUrl` (`<proxy-url>/?url=<encoded>`, graceful fallback), `extract-document.ts:execute` (`const fetchUrl = await resolveDocumentFetchUrl(url)`), matrix row 169.

---

### WUI-TOOL-21: arxiv abstract URLs get a `.pdf` filename for correct format detection
- **Persona:** user whose prompt makes the agent call `extract_document` with an arxiv URL
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Type "Read this paper: https://arxiv.org/pdf/2401.00001".
- **Assertions:**
  - A1: The filename derived from the URL has `.pdf` appended for `https://arxiv.org/` URLs, so extension-based format detection works.
  - A2: For non-arxiv URLs the filename is the last path segment with any query string stripped (`?...` removed), defaulting to `document` when empty.
  - A3: On success the card title shows the derived `.pdf` filename and `PDF` format.
- **Traces:** `extract-document.ts:execute` (filename derivation, arxiv `.pdf` special-case).

---

### WUI-TOOL-22: Both renderers auto-register via side-effect imports
- **Persona:** user whose prompt makes the agent call either browser tool
- **Preconditions:**
  - The web UI is freshly loaded (no prior tool calls).
- **Steps:**
  1. Trigger a `javascript_repl` call and an `extract_document` call.
- **Assertions:**
  - A1: The `javascript_repl` tool card uses the dedicated REPL renderer (code icon, "Executing JavaScript" header), not the generic default renderer.
  - A2: The `extract_document` tool card uses the dedicated extract renderer (file-text icon, "Extracting document..."/"Extracted text..." header), not the generic default renderer.
  - A3: These renderers are registered purely as import side effects (`tools/index.ts` imports `./extract-document` and `./javascript-repl`), with no explicit per-tool wiring needed in the shell.
- **Traces:** `tools/index.ts` (side-effect imports), `javascript-repl.ts` / `extract-document.ts` (`registerToolRenderer(...)` at module bottom), matrix row 98.

---

### WUI-TOOL-23: Server declares both tools so the model knows they exist
- **Persona:** user whose prompt could be satisfied by either browser tool
- **Preconditions:**
  - The web UI is connected; the server constructed the agent session with the browser tools bound to the hub.
- **Steps:**
  1. Type a prompt that should make the model choose to compute in JS, e.g. "Calculate 17 factorial precisely."
  2. Separately, type a prompt that should make the model choose to read a remote document.
- **Assertions:**
  - A1: The model is offered both `javascript_repl` and `extract_document` (declared server-side with brand-neutral descriptions and JSON-schema parameters), so it can select them without the browser pre-injecting them.
  - A2: The `javascript_repl` schema requires a `code` string; the `extract_document` schema requires a `url` string.
  - A3: The server descriptions are brand-neutral (no product/vendor names) and stay consistent with the client descriptions.
  - A4: When declared, prompting for a calculation results in the model emitting a `javascript_repl` tool call (rather than answering inline) for cases needing precise computation.
- **Traces:** `browser_tools.rs` (`javascript_repl_browser_tool`, `extract_document_browser_tool`, descriptions + `*_parameters()`), matrix rows 100, 179, 180.

---

### WUI-TOOL-24: Server suspends the browser-tool call until the browser replies, then resumes
- **Persona:** user whose prompt makes the agent call a browser tool
- **Preconditions:**
  - The web UI is connected; a browser tool is invoked mid-turn.
- **Steps:**
  1. Trigger any `javascript_repl` or `extract_document` call.
- **Assertions:**
  - A1: The server's tool `execute` closure does NOT do the work itself; it registers a one-shot channel keyed by the tool call id on the per-connection hub and awaits the browser.
  - A2: The browser receives the `tool_execution_start` event, runs the matching local executor, and sends a `tool_result` frame keyed by the same tool call id.
  - A3: The server's inbound task resolves the pending channel by id, unblocking the suspended closure so the agent loop continues mid-prompt (no deadlock between dispatcher and inbound tasks).
  - A4: A duplicate or late `tool_result` for an unknown/already-resolved id is a silent no-op (does not panic or corrupt state).
- **Traces:** `browser_tools.rs:BrowserToolHub` (`register`/`resolve`), `browser_tools.rs:browser_tool`, `remote-agent.ts:runBrowserTool` (sends `tool_result`), matrix row 99.

---

### WUI-TOOL-25: Missing browser executor is reported as a tool error rather than hanging
- **Persona:** user whose prompt makes the agent call a browser tool that has no registered local executor
- **Preconditions:**
  - The web UI is connected; for some reason a server-declared browser tool has no client-side executor registered.
- **Steps:**
  1. Trigger a server `tool_execution_start` for a browser tool name that the client never registered.
- **Assertions:**
  - A1: The client immediately replies with an error `tool_result`: "No browser executor registered for tool <name>".
  - A2: `isError` is true on that result.
  - A3: The agent loop resumes (the suspended server closure is resolved) instead of hanging forever.
- **Traces:** `remote-agent.ts:runBrowserTool` (`if (!execute) ...`), matrix row 99.

---

### WUI-TOOL-26: Agent uses a REPL-returned file as a follow-up attachment
- **Persona:** user whose prompt makes the agent call `javascript_repl` to produce a file, then act on it
- **Preconditions:**
  - The web UI is connected and a tool-capable model is selected.
- **Steps:**
  1. Type "Generate a small PNG chart with the REPL and return it as chart.png, then describe what it shows."
  2. Wait for the REPL call and the follow-up.
- **Assertions:**
  - A1: The returned image file is shown as an attachment-tile chip typed `image`, with its base64 content set as a preview so the tile can show a thumbnail.
  - A2: The returned file's metadata (fileName `chart.png`, image MIME type, size) is reported back to the agent in the tool result.
  - A3: The agent can reference the returned file in its follow-up message (it is offered to the user and reported to the model, even though it is not accessible in a later REPL call).
  - A4: A non-image returned file is typed `document` with its decoded text (if textual) populated, distinguishing image vs. document handling.
- **Traces:** `javascript-repl.ts:filesToAttachments` (image vs. document, preview, extractedText), `prompts.ts:FILE_DOWNLOAD_RUNTIME_DESCRIPTION`, matrix rows 91, 93.
