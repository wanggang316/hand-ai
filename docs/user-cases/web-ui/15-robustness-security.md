# Robustness & Security — Web UI Acceptance Test Cases

Scope: cross-cutting robustness and security guarantees of the web UI — server-side API-key resolution and key non-exposure, the download endpoint's session-cwd path-safety gate, the upload size cap, the HTML/REPL sandbox iframe isolation (`allow-scripts allow-modals`, no `allow-same-origin`), graceful degradation under malformed WS frames, IndexedDB failures, browser-tool errors and socket drops, large-input / large-transcript resilience, the `get_state` in-flight-prompt deferral, documented carry-forward constraints, and the brand-neutrality / no-blocking-dialog source invariants.

### WUI-SEC-01: Provider API keys are resolved from the process environment, never from the browser
- **Persona:** Security reviewer
- **Preconditions:** The Rust server (`crates/web-ui`) is started with provider credentials present in its process environment; a browser session is connected over `/ws`
- **Steps:**
  1. Send a prompt that triggers a real LLM call.
  2. Inspect every WebSocket frame the browser transmitted (DevTools → Network → WS) during connection and prompting.
  3. Inspect the `ClientCommand` catalog (`web/src/client/wire.ts`) for any key-bearing field.
- **Assertions:**
  - A1: The LLM call succeeds using credentials the server read from its own environment at session-build time (`session.rs` builds the session with `model::Client::new()`; no key arrives over the wire).
  - A2: No outbound frame (`prompt`, `set_model`, `get_state`, `tool_result`, etc.) contains an `apiKey` / `api_key` / bearer-token field.
  - A3: The `ClientCommand` union declares no field carrying a provider secret.
  - A4: Removing all browser-stored provider keys does not break server-side LLM calls (the server does not depend on browser-supplied keys for its own dispatch).
- **Traces:** rows 195, 193; architecture §4.4; M0/M8

### WUI-SEC-02: API keys never appear in any inbound frame sent to the browser
- **Persona:** Security reviewer
- **Preconditions:** A connected session whose server holds provider credentials in its environment
- **Steps:**
  1. Drive a full turn (agent_start → agent_end), including a tool call.
  2. Capture every inbound WS frame (event frames, response frames, error frames).
  3. Search the captured frames for any substring matching a known credential value.
- **Assertions:**
  - A1: No inbound frame echoes a provider API key or any environment secret.
  - A2: `get_state` / `get_available_models` responses describe models and provider names but carry no key material.
  - A3: Error frames surfaced as toasts contain human-readable messages, not raw credential values.
- **Traces:** row 195; architecture §4.4; M0/M8

### WUI-SEC-03: API keys are absent from the shipped JavaScript bundle and inline HTML
- **Persona:** Security reviewer auditing the served assets
- **Preconditions:** A production build of the frontend is served (embedded bundle or `--web-dir dist`)
- **Steps:**
  1. Download `index.html` and every `assets/*.js` chunk the page loads.
  2. Grep the served bytes for credential-shaped strings and for hardcoded provider secrets.
  3. View the page source and any inline `<script>` blocks (including the `DEV_INDEX` connectivity probe).
- **Assertions:**
  - A1: No provider API key or secret is embedded in any served HTML or JS asset.
  - A2: The inline connectivity-probe page (`app.rs` `DEV_INDEX` / `assets.rs` `EMPTY_INDEX`) contains no key material and only opens `/ws`.
  - A3: The bundle reads keys only from the browser's own IndexedDB store at runtime (never compiled in).
- **Traces:** rows 195, 196; architecture §4.4; M8/M12

### WUI-SEC-04: Browser-stored provider keys live only in IndexedDB and are never transmitted over /ws
- **Persona:** Security reviewer
- **Preconditions:** A user has entered a provider key via the API-keys settings tab; the key is persisted to the `provider-keys` IndexedDB store
- **Steps:**
  1. Open DevTools → Application → IndexedDB → `hand-ai` → `provider-keys` and confirm the key is present.
  2. Call `ProviderKeysStore.list()` and observe the returned data.
  3. Trigger client-side local-provider discovery (Ollama / llama.cpp / vLLM / LM Studio) and a normal prompt; capture all `/ws` frames.
- **Assertions:**
  - A1: `list()` returns only provider names that have a stored key, never the key values themselves.
  - A2: The stored key is used only for direct browser→local-endpoint discovery fetches (`providers/discovery.ts` `Authorization: Bearer`), not sent to the Rust server over `/ws`.
  - A3: No `/ws` frame carries the stored key.
  - A4: The key is documented as stored unencrypted at rest in the browser (architecture decision), and the server does not rely on it for its own LLM calls.
- **Traces:** rows 195; architecture §4.4, §7 (IndexedDB row); M8

### WUI-SEC-05: API keys never appear in any URL
- **Persona:** Security reviewer
- **Preconditions:** A connected session; key configured both browser-side (IndexedDB) and server-side (env)
- **Steps:**
  1. Inspect the `/ws` connection URL, `/upload`, `/download/register`, and `/download/:id` request URLs.
  2. Inspect the document URL and any object URLs created for downloads.
- **Assertions:**
  - A1: The `/ws` URL is `ws(s)://<host>/ws` with no query string carrying a secret.
  - A2: `/download/:id` uses an opaque server-minted id (`dl-<nanos>-<n>`), not a path or key, in the URL.
  - A3: No credential ever appears in a query string, fragment, or path segment of any request the browser issues.
- **Traces:** row 195; architecture §4.4, §5.4; M8/M10

### WUI-SEC-06: Download registration rejects a path that escapes the session cwd (`..` traversal)
- **Persona:** Security reviewer probing the download endpoint
- **Preconditions:** The server runs with a known session `cwd`; a sensitive file exists outside it (e.g. `/etc/hosts`)
- **Steps:**
  1. `POST /download/register` with `{ "path": "../../../../etc/hosts" }`.
  2. `POST /download/register` with an absolute path outside the cwd (e.g. `/etc/hosts`).
- **Assertions:**
  - A1: Both requests fail; neither yields a download id.
  - A2: A path that canonicalizes outside the cwd returns `403 Forbidden` ("path is outside the session directory").
  - A3: A path that does not exist returns `404 Not Found`.
  - A4: No bytes from outside the session cwd are ever streamed.
- **Traces:** row (download safety); architecture §5.4; download.rs `register_rejects_path_outside_cwd`; M10

### WUI-SEC-07: Download registration rejects a symlink that points outside the cwd
- **Persona:** Security reviewer attempting a symlink escape
- **Preconditions:** Inside the session cwd there is a symlink `escape.txt` whose target is a file outside the cwd
- **Steps:**
  1. `POST /download/register` with `{ "path": "escape.txt" }`.
- **Assertions:**
  - A1: Registration canonicalizes the symlink (resolving the real target) before the cwd-containment check.
  - A2: Because the resolved target lies outside the cwd, the request returns `403 Forbidden` and no id is issued.
  - A3: The symlink cannot be used to exfiltrate an out-of-cwd file via the download endpoint.
- **Traces:** row (download safety); architecture §5.4; download.rs (canonicalize both sides); M10

### WUI-SEC-08: A legitimate in-cwd export file registers and downloads correctly
- **Persona:** User exporting a session transcript to HTML
- **Preconditions:** The server produced an `export_html` output file inside the session cwd
- **Steps:**
  1. `POST /download/register` with the export file's cwd-relative path.
  2. `GET /download/:id` with the returned id.
- **Assertions:**
  - A1: Registration succeeds and returns an opaque id.
  - A2: `GET /download/:id` returns `200 OK` with the file bytes intact.
  - A3: The response sets `Content-Disposition: attachment` so the browser saves rather than renders the file.
  - A4: The `Content-Type` matches the file extension (e.g. `text/html; charset=utf-8` for `.html`).
- **Traces:** row (download); architecture §5.4; download.rs `register_then_download_serves_cwd_file`; M10

### WUI-SEC-09: GET /download/:id returns 404 for an unknown or expired id
- **Persona:** Security reviewer / user with a stale link
- **Preconditions:** A connected server; no blob registered under the probe id
- **Steps:**
  1. `GET /download/missing-id`.
  2. Register a file, then request `GET /download/:id` after the backing file has been removed from disk.
- **Assertions:**
  - A1: An unknown id returns `404 Not Found` ("unknown download id").
  - A2: A known id whose on-disk bytes are missing returns `404 Not Found` ("download contents missing"), not a server crash.
  - A3: Ids are opaque and not guessable from sequential URLs (no directory listing is exposed).
- **Traces:** row (download); download.rs `download_unknown_id_is_404`; M10

### WUI-SEC-10: Upload size cap (50MB) is enforced on both multipart and raw bodies
- **Persona:** Security reviewer / user uploading a large attachment
- **Preconditions:** A connected server; `/upload` available
- **Steps:**
  1. `POST /upload` with a multipart `file` field larger than 50MB.
  2. `POST /upload` with a raw (non-multipart) body larger than 50MB.
- **Assertions:**
  - A1: Both requests are rejected with `413 Payload Too Large` ("upload too large").
  - A2: The cap is enforced by the router body limit (`DefaultBodyLimit::max(MAX_UPLOAD_BYTES)`) AND re-checked in the handler/multipart field path.
  - A3: An over-cap upload stores nothing in the blob store (no partial blob is left behind).
  - A4: A within-cap upload returns `{ id, size }` and round-trips via `GET /download/:id`.
- **Traces:** row (upload); architecture §5.4; upload.rs `MAX_UPLOAD_BYTES`; M10

### WUI-SEC-11: Upload rejects empty / fieldless bodies without crashing
- **Persona:** Security reviewer sending malformed uploads
- **Preconditions:** `/upload` available
- **Steps:**
  1. `POST /upload` with an empty raw body.
  2. `POST /upload` with a multipart body that contains no file field (or only empty fields).
- **Assertions:**
  - A1: An empty raw body returns `400 Bad Request` ("empty upload").
  - A2: A multipart body with no non-empty file field returns `400 Bad Request` ("no file field in upload").
  - A3: A malformed multipart body returns `400 Bad Request` ("invalid multipart") rather than a panic or hang.
- **Traces:** row (upload); upload.rs; M10

### WUI-SEC-12: Client-supplied filenames cannot inject path traversal into Content-Disposition
- **Persona:** Security reviewer
- **Preconditions:** `/upload` available
- **Steps:**
  1. `POST /upload` with a filename of `../../etc/passwd`.
  2. Download the resulting blob and inspect the `Content-Disposition` header.
- **Assertions:**
  - A1: The stored filename is sanitized to its final component (`passwd`), stripping all path segments (`sanitize_file_name`).
  - A2: An empty or path-only filename falls back to `attachment`.
  - A3: The on-disk upload path is keyed by content hash inside a per-process temp dir, never derived from the client filename.
- **Traces:** row (upload); blob_store.rs `sanitize_strips_path_traversal`; M10

### WUI-SEC-13: The HTML artifact / REPL sandbox iframe uses exactly `allow-scripts allow-modals`
- **Persona:** Security reviewer inspecting the sandbox boundary
- **Preconditions:** A conversation that creates a live HTML artifact (and a REPL execution)
- **Steps:**
  1. Open the rendered artifact and inspect its `<iframe>` `sandbox` attribute tokens.
  2. Inspect the transient hidden iframe used by `execute()` for the REPL.
- **Assertions:**
  - A1: The sandbox attribute contains exactly the tokens `allow-scripts` and `allow-modals`.
  - A2: The sandbox attribute does NOT contain `allow-same-origin` (the iframe is forced into an opaque origin).
  - A3: The same token set is applied in both the `srcdoc` delivery path and the `sandboxUrlProvider` (extension CSP) path, and for both `loadContent()` and `execute()`.
- **Traces:** rows 56, 57, 59; architecture §7 (HTML artifact sandbox); sandboxed-iframe.ts; M3

### WUI-SEC-14: Sandboxed artifact code cannot read host cookies, DOM, or storage
- **Persona:** Security reviewer running adversarial artifact code
- **Preconditions:** A live HTML artifact whose script attempts to reach the host page
- **Steps:**
  1. Create an artifact whose script attempts `document.cookie`, `window.parent.document`, `localStorage`, and `indexedDB` access on the host.
  2. Observe the captured console output and any thrown errors.
- **Assertions:**
  - A1: Because the iframe lacks `allow-same-origin`, host-document and cross-origin storage access throw `SecurityError`; the artifact cannot read host cookies or the host DOM.
  - A2: The artifact's own `document.cookie` / `localStorage` are scoped to its opaque origin and do not reach the host's IndexedDB (`hand-ai`) or provider-keys store.
  - A3: The artifact communicates with the host only via the `postMessage` runtime bridge routed through `RUNTIME_MESSAGE_ROUTER`, which validates `e.source === iframe.contentWindow`.
  - A4: The host page's provider keys and session data remain unreadable from inside the sandbox.
- **Traces:** rows 56, 62; architecture §7; sandboxed-iframe.ts, runtime-message-router.ts; M3

### WUI-SEC-15: Sandbox navigation is intercepted; external links/forms open in a new tab, not the iframe
- **Persona:** Security reviewer testing navigation containment
- **Preconditions:** A live HTML artifact with an external link and a form posting to an external URL
- **Steps:**
  1. Click the external link inside the artifact.
  2. Submit the external form inside the artifact.
  3. Have the artifact attempt to set `window.location`.
- **Assertions:**
  - A1: Each navigation is prevented inside the iframe and re-emitted as an `open-external-url` postMessage to the parent.
  - A2: The parent opens the URL via `window.open(url, "_blank")` rather than navigating the host or the iframe.
  - A3: An assignment to `window.location` inside the sandbox is intercepted and routed externally, not performed in place.
- **Traces:** row 60; architecture §7; sandboxed-iframe.ts navigation interceptor; M3

### WUI-SEC-16: Injected sandbox content cannot break out of the runtime `<script>` tag
- **Persona:** Security reviewer probing script-injection escapes
- **Preconditions:** A REPL execution and an HTML artifact whose content includes a literal `</script>` sequence and provider data containing `</script>`
- **Steps:**
  1. Run REPL code that contains the substring `</script>`.
  2. Inject provider data (e.g. attachment metadata) containing `</script>`.
- **Assertions:**
  - A1: User code is escaped (`</script` → `<\/script`) before embedding, so the runtime script tag is not prematurely closed.
  - A2: JSON-serialized injected data is likewise escaped against `</script`.
  - A3: The HTML validation gate (DOMParser `parsererror`) renders an error page instead of running malformed/injected markup.
- **Traces:** rows 56, 61; sandboxed-iframe.ts `escapeScriptContent` / `validateHtml`; M3

### WUI-SEC-17: A malformed inbound WebSocket frame is ignored without crashing the app
- **Persona:** User under faulty/garbled network conditions
- **Preconditions:** A connected, working chat session
- **Steps:**
  1. Have the server (or a proxy) deliver a non-JSON text frame, then a JSON frame missing required fields, then a binary frame.
  2. Continue a normal turn afterward.
- **Assertions:**
  - A1: A non-JSON frame is dropped silently (`JSON.parse` failure → early return) and the connection stays open.
  - A2: A structurally invalid frame does not throw to the top level or break the message-handler loop.
  - A3: The next valid frame is processed normally; streaming and rendering resume.
- **Traces:** rows 190–192; ws-connection.ts (try/catch around parse); M1

### WUI-SEC-18: A malformed `tool_result` frame on the server is logged and dropped, not fatal
- **Persona:** Security reviewer / client sending garbage tool replies
- **Preconditions:** A session with a suspended browser-tool execution awaiting a reply
- **Steps:**
  1. Send a frame whose `type` is `tool_result` but whose body fails to deserialize (`ToolResultFrame`).
  2. Send a `tool_result` for an unknown `toolCallId`.
- **Assertions:**
  - A1: The malformed frame is dropped with a `tracing::warn!` ("dropping malformed tool_result frame"); the connection and dispatcher keep running.
  - A2: A `tool_result` for an unregistered id is a no-op (`BrowserToolHub::resolve` removes nothing) and never panics.
  - A3: A subsequent well-formed `tool_result` for a real pending id still resolves its suspended closure.
- **Traces:** rows 191, 193; ws.rs, browser_tools.rs `resolve_unknown_id_is_noop`; M2

### WUI-SEC-19: IndexedDB failures are swallowed; the chat keeps working
- **Persona:** User in a private-mode / quota-exceeded / blocked-storage browser
- **Preconditions:** IndexedDB is unavailable or every write rejects
- **Steps:**
  1. Load the app where `indexedDB.open` or store writes fail.
  2. Send and receive a full chat turn.
  3. Toggle theme and trigger session auto-save.
- **Assertions:**
  - A1: Session auto-save failures are caught and logged ("session auto-save failed"); no exception bubbles to the chat.
  - A2: A failed `providerKeysStore.has(...)` resolves to `false` (`.catch(() => false)`) so API-key gating still proceeds via the prompt dialog.
  - A3: Theme/settings persistence failures are swallowed (`.catch(() => {})`) and the UI continues with defaults.
  - A4: Streaming, sending, and rendering all work despite storage being dead.
- **Traces:** row (IndexedDB resilience); architecture §7 (IndexedDB row); main.ts; M7

### WUI-SEC-20: A browser-tool execution error becomes a tool error result, not an agent hang
- **Persona:** User whose artifact/REPL/extract tool throws or has no executor
- **Preconditions:** A session where a browser tool throws during execution (or no executor is registered for the tool name)
- **Steps:**
  1. Trigger a server tool call whose browser executor throws.
  2. Trigger a server tool call for a tool name with no registered browser executor.
- **Assertions:**
  - A1: A thrown executor produces a `tool_result` frame with `isError: true` and a "Browser tool ... failed" message rather than no reply.
  - A2: A missing executor produces a `tool_result` with `isError: true` ("No browser executor registered ...").
  - A3: The server-side suspended tool closure is resolved by the error reply, so the agent loop continues instead of hanging.
  - A4: If the channel closes before a reply, the server-side closure resolves to `ToolResult::error("browser tool channel closed")`.
- **Traces:** rows 191, 56; remote-agent.ts `runBrowserTool`, browser_tools.rs; M2/M5

### WUI-SEC-21: WebSocket drop triggers automatic reconnect with capped backoff
- **Persona:** User whose network blips or whose server restarts mid-session
- **Preconditions:** A connected session; the user did not press a disconnect/close affordance
- **Steps:**
  1. Force the underlying socket to close unexpectedly.
  2. Keep the close event firing several times to exercise backoff.
  3. Send a message while the socket is still re-opening.
- **Assertions:**
  - A1: On an unexpected close the client schedules a reconnect (initial 1s delay), and the app does not surface a dead, unrecoverable state.
  - A2: The reconnect delay doubles each attempt, capped at 15s; on a successful open the backoff resets to 1s.
  - A3: Frame subscribers (`onFrame`) persist across reconnects; in-flight correlated `request()` promises are rejected on close (not left hanging).
  - A4: Messages sent while closed are queued and flushed on the next open.
  - A5: A user-initiated `close()` sets `closedByUser` and does NOT auto-reconnect.
- **Traces:** row 194; ws-connection.ts reconnect/backoff; M0/M1

### WUI-SEC-22: Very long input and a very large transcript do not crash the UI
- **Persona:** Power user pasting huge text and continuing a long conversation
- **Preconditions:** A working session
- **Steps:**
  1. Paste a very large block of text into the message editor and send it.
  2. Continue a conversation until the transcript holds hundreds of messages.
  3. Trigger session persistence and preview generation.
- **Assertions:**
  - A1: A large message is sent (text WS frame) without throwing; the UI stays responsive.
  - A2: Auto-save preview generation bounds its work (preview capped at 2048 chars, title at 80) and does not block on the full transcript.
  - A3: A returned downloadable file >1MB round-trips via chunked base64 (`0x8000`) without a stack overflow.
  - A4: The message list keeps rendering; no unhandled exception or blank screen results from transcript size.
- **Traces:** rows 56, 62; exec-plan M3 observable (chunked base64); M1/M3

### WUI-SEC-23: Oversized / disallowed attachments are blocked client-side before upload
- **Persona:** User attaching too many or too-large files
- **Preconditions:** The message editor is open
- **Steps:**
  1. Attempt to attach more than 10 files.
  2. Attempt to attach a single file larger than 20MB.
- **Assertions:**
  - A1: Exceeding 10 attachments is refused and an inline, auto-dismissing error is shown (not a blocking dialog).
  - A2: A file over the 20MB per-file cap is rejected with an inline "exceeds the maximum size" message and is not added.
  - A3: The editor's per-file cap (20MB) sits under the server's 50MB `/upload` cap, so a within-editor-cap non-image upload is also within the server cap.
- **Traces:** row (attachments/limits); message-editor.ts (`maxFiles`, `maxFileSize`); M6/M10

### WUI-SEC-24: `get_state` requested during an in-flight prompt is deferred (documented latency)
- **Persona:** User opening settings / hydrating model state while the agent is streaming
- **Preconditions:** A prompt is actively streaming on the per-connection session
- **Steps:**
  1. While a turn is in flight, issue a `get_state` (e.g. via `hydrate()` or model-state read).
  2. Observe when the response arrives.
- **Assertions:**
  - A1: The `get_state` response is deferred until the in-flight prompt completes (per the documented carry-forward constraint), not interleaved into the stream.
  - A2: The deferral does not deadlock: once the prompt finishes, the `get_state` response is delivered and the model label updates.
  - A3: A `get_state` that never resolves within the request timeout rejects cleanly and `hydrate()` keeps the placeholder label (best-effort), without crashing.
- **Traces:** row 202; architecture §4.4 (carry-forward constraints); remote-agent.ts `hydrate`; M12

### WUI-SEC-25: Documented carry-forward constraints are honored (Compact.customInstructions dropped; absolute session paths)
- **Persona:** Security reviewer / user relying on documented limits
- **Preconditions:** A session capable of compaction and export
- **Steps:**
  1. Trigger compaction with custom instructions supplied.
  2. Export the session and inspect the returned path.
  3. Review the documented constraints list.
- **Assertions:**
  - A1: `Compact.customInstructions` is dropped server-side and is not advertised as working (no UI claim that it takes effect).
  - A2: `export_html` returns an absolute server-filesystem path, which the browser download flow registers and fetches via `GET /download/:id` rather than reading the path directly.
  - A3: The carry-forward constraints (get_state latency, dropped custom instructions, absolute session paths) are documented and the implementation matches them.
- **Traces:** row 202; architecture §4.4; exec-plan §Verification; M12

### WUI-SEC-26: Brand-neutrality grep over the frontend and server source returns zero forbidden substrings
- **Persona:** Security/brand reviewer running the de-branding gate
- **Preconditions:** A checkout of `crates/web-ui`
- **Steps:**
  1. Run a case-insensitive grep over `crates/web-ui/web/src` and `crates/web-ui/src` for the forbidden substrings (the reference project's package prefixes, the author handle, the issue marker, and the reference-project name — as defined by the team branding policy).
- **Assertions:**
  - A1: The grep returns ZERO matches across both source trees.
  - A2: Emitted custom-element tags use the `hand-` prefix where the reference UI used a brand prefix; already-neutral tags are unchanged.
  - A3: The frontend imports no external agent/model/helper package by brand name; the Rust server depends only on the workspace crates (`hand-coding-agent`, `hand-agent`, `model`).
- **Traces:** rows 199, 200–202; architecture Appendix (Naming and De-branding); exec-plan M11/M12; M11/M12

### WUI-SEC-27: No blocking `window.confirm` / `window.alert` / `window.prompt` calls in shipped code
- **Persona:** Security/UX reviewer auditing for UI-thread-blocking, untestable dialogs
- **Preconditions:** A checkout of `crates/web-ui/web/src`
- **Steps:**
  1. Grep the frontend source for native blocking-dialog calls (`window.confirm`, `window.alert`, `window.prompt`, and bare `alert(` / `confirm(` / `prompt(`).
  2. Exercise the editor validation, the extension-UI confirm/select/input/editor flows, and the API-key prompt.
- **Assertions:**
  - A1: No shipped code path calls a native blocking `window.confirm` / `window.alert` / `window.prompt`.
  - A2: Editor validation errors render as a non-blocking, auto-dismissing inline message (the former `window.alert` sites were replaced).
  - A3: Confirmations and prompts go through in-app dialog components (e.g. the extension-UI `confirm/select/input/editor` and the API-key prompt dialog), which are testable and do not freeze the UI thread.
- **Traces:** rows 199; message-editor.ts (`showError`), dialogs/extension-ui.ts, dialogs/api-key-prompt-dialog.ts; M9/M11
