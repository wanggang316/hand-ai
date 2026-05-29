# Networking (WebSocket, Upload/Download, Dispatch) — Web UI Acceptance Test Cases

Scope: the transport seam of the web UI — the `/ws` WebSocket lifecycle (framing, send-before-open buffering, correlated request/response, auto-reconnect with capped backoff), the JSONL-over-WebSocket bridge that reuses the server dispatcher, server-side interception of browser-tool `tool_result` frames, the out-of-band HTTP endpoints (`POST /upload`, `GET /download/:id`, `POST /download/register`), hybrid attachment dispatch, the export→download flow, the client-side document-fetch proxy, `/healthz`, and the inbound event stream. Assertions are observable either in the browser (DevTools Network/console) or via `curl`-style HTTP probes.

### WUI-NET-01: WebSocket connects at /ws on page load
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The web UI server is running and reachable; a browser tab is opened on the app origin
- **Steps:**
  1. Open the app and watch the DevTools Network panel filtered to WS.
  2. Observe the request that establishes the live channel.
- **Assertions:**
  - A1: Exactly one WebSocket request is made to the path `/ws` on the same origin (scheme `ws://` for `http`, `wss://` for `https`).
  - A2: The request completes the HTTP 101 Switching Protocols upgrade handshake.
  - A3: After the handshake the socket reaches the `open` state and stays open with no immediate close frame.
- **Traces:** rows 170, 4; M0

### WUI-NET-02: Each text frame carries exactly one JSON object
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected `/ws` socket; a prompt has been sent so the server is streaming events back
- **Steps:**
  1. Capture the inbound WebSocket text frames in the Network panel's Messages view.
  2. Parse each captured frame body independently with a JSON parser.
- **Assertions:**
  - A1: Every inbound frame is a UTF-8 text message (not binary).
  - A2: Each frame body parses as exactly one JSON object (no newline-delimited concatenation, no trailing data).
  - A3: Each outbound command the browser sends is likewise one JSON object per text frame.
  - A4: A frame that fails to parse as JSON is silently ignored by the client and does not break the stream of subsequent frames.
- **Traces:** rows 3, 170; M0

### WUI-NET-03: Sends issued before the socket opens are buffered then flushed in order
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The app is mid-connect — a command is dispatched in the window between socket construction and the `open` event (e.g. an early `get_state` during bootstrap, or a reconnect in progress)
- **Steps:**
  1. Trigger one or more `send`/`request` calls while the socket is not yet open.
  2. Allow the socket to reach `open`.
  3. Observe the outbound Messages timeline.
- **Assertions:**
  - A1: No send throws or is dropped while the socket is not open.
  - A2: When the socket opens, the buffered command(s) are transmitted as outbound frames.
  - A3: Buffered commands are sent in the exact order they were enqueued.
  - A4: After the buffer flushes, the internal send queue is empty so subsequent sends go out immediately.
- **Traces:** rows 170, 7; M0

### WUI-NET-04: A correlated request resolves with its response data
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket; the model selector triggers `get_available_models` (a request/response command)
- **Steps:**
  1. Issue a request-style command (e.g. open the model selector to fire `get_available_models`).
  2. Observe the outbound command frame and the matching inbound `response` frame.
- **Assertions:**
  - A1: The outbound command frame carries a unique correlation `id` (e.g. `req-1`) that the caller did not supply.
  - A2: The inbound frame has `type: "response"`, the same `id`, and `command` equal to the issued command type.
  - A3: When `success: true`, the request promise resolves with the frame's `data` payload (the model list renders).
  - A4: The pending-request slot for that `id` is cleared after settlement (a duplicate late `response` with the same `id` has no further effect).
- **Traces:** rows 182, 183, 187; M0

### WUI-NET-05: A request rejects on success:false
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket; a request-style command is issued whose server handler reports failure (`success: false` with an `error` string)
- **Steps:**
  1. Issue a request that the server rejects (e.g. an `export_html` to an invalid target).
  2. Observe the inbound `response` frame and the caller's outcome.
- **Assertions:**
  - A1: The inbound `response` frame has `success: false` and a non-null `error` (or a `command` fallback).
  - A2: The request promise rejects with an `Error` whose message equals the server `error` (or `Command failed: <command>` when `error` is absent).
  - A3: The rejection does NOT close the socket; subsequent commands still succeed on the same connection.
- **Traces:** rows 187, 168; M10

### WUI-NET-06: A request rejects on timeout
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket; a request is issued for which no matching `response` arrives within the request timeout window (default 30s)
- **Steps:**
  1. Issue a request-style command whose response is withheld/delayed past the timeout.
  2. Wait for the timeout to elapse.
- **Assertions:**
  - A1: After the timeout the request promise rejects with an `Error` whose message reads `Request timed out: <command type>`.
  - A2: The pending-request entry for that `id` is removed so a late-arriving response for it is a no-op.
  - A3: Event frames (non-response) continue to fan out to subscribers during and after the timeout.
- **Traces:** row 170; M0

### WUI-NET-07: Pending requests reject when the socket closes
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket with at least one in-flight correlated request awaiting its response
- **Steps:**
  1. Issue a request-style command.
  2. Before its response arrives, force the socket to close (e.g. stop the server or drop the network).
- **Assertions:**
  - A1: Every in-flight request promise rejects with an `Error` reading `WebSocket closed before response`.
  - A2: All pending-request timers are cleared (no later spurious timeout rejection fires for those ids).
  - A3: The pending-request map is emptied on close.
- **Traces:** row 170; M0

### WUI-NET-08: Unexpected close triggers auto-reconnect with capped exponential backoff
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket that was NOT closed by the client; the server becomes briefly unavailable then returns
- **Steps:**
  1. Drop the server so the socket closes unexpectedly.
  2. Observe the sequence and timing of reconnect attempts in the Network panel.
  3. Keep the server down long enough to observe several attempts, then restore it.
- **Assertions:**
  - A1: The client schedules a reconnect after an unexpected close (the first retry after ~1000ms).
  - A2: Successive retry delays double (≈1s, 2s, 4s, 8s, …) until they cap at 15s and never exceed 15s thereafter.
  - A3: Each reconnect attempt opens a fresh `/ws` socket to the same URL.
  - A4: Once a reconnect succeeds (`open`), the backoff delay resets to its initial value (~1s) for any future close.
- **Traces:** row 170; M0/M10

### WUI-NET-09: Explicit close() stops reconnection
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket
- **Steps:**
  1. Invoke the client's `close()` path (deliberate teardown).
  2. Wait well beyond the initial backoff window.
- **Assertions:**
  - A1: The socket closes and no reconnect attempt is scheduled or made.
  - A2: No new `/ws` request appears in the Network panel after the deliberate close.
  - A3: A close initiated by the client is distinguished from an unexpected close (only the latter reconnects).
- **Traces:** row 170; M0

### WUI-NET-10: Frame subscribers persist across reconnects
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A `RemoteAgent` subscribed to inbound frames over a connected socket; the connection drops and auto-reconnects
- **Steps:**
  1. Confirm the agent is receiving event frames on the original socket.
  2. Force an unexpected close and let auto-reconnect re-open the socket.
  3. Send a prompt on the reconnected socket.
- **Assertions:**
  - A1: After reconnect, inbound event frames still reach the same subscriber without re-registration (handlers live on the connection object, not the socket).
  - A2: The new turn streams normally on the fresh socket.
  - A3: Browser-side state (transcript already rendered) is unaffected by the reconnect, even though the server side is a fresh per-connection session.
- **Traces:** rows 170, 194; M0/M10

### WUI-NET-11: JSONL bridge forwards prompt and streams the agent event sequence
- **Persona:** End user sending a chat message
- **Preconditions:** A connected socket; a model is selected
- **Steps:**
  1. Type a message and submit it.
  2. Observe the outbound `prompt` frame and the inbound event frames.
- **Assertions:**
  - A1: The outbound frame is `{ type: "prompt", id, message }` (one JSON object).
  - A2: The server reuses its existing dispatcher: the inbound stream carries `agent_start`, then streaming `message_update` frames, then `turn_end`, then `agent_end` for the turn.
  - A3: Assistant text appears token-by-token in the chat as `message_update` frames arrive.
  - A4: `agent_end` reconciles the transcript and re-enables the editor.
- **Traces:** rows 180, 190, 193; M0/M1

### WUI-NET-12: Lifecycle / mode / session commands ride the same dispatcher
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket
- **Steps:**
  1. Exercise representative commands across the catalog: `abort`, `set_model`, `cycle_model`, `set_thinking_level`, `compact`, `new_session`, `switch_session`, `set_session_name`, `get_state`, `get_messages`, `get_session_stats`, `get_commands`, `get_available_models`, `set_auto_compaction`, `set_auto_retry`.
  2. Observe each outbound frame and the corresponding server reaction.
- **Assertions:**
  - A1: Each command serializes to a single JSON text frame whose `type` is the snake_case command name with camelCase payload fields (e.g. `set_model` carries `provider` + `modelId`; `set_thinking_level` carries `level`).
  - A2: Read commands (`get_state`, `get_messages`, `get_available_models`, `get_session_stats`, `get_commands`) return a correlated `response` frame with `data`.
  - A3: Fire-and-forget commands (`abort`, `set_model`, `new_session`, etc.) produce their observable session effect (e.g. `abort` ends the turn; `set_model` changes the active model label).
  - A4: `set_thinking_level` with level `off` is NOT sent over the wire (no frame emitted); a non-`off` level emits a `set_thinking_level` frame.
- **Traces:** rows 180, 181, 182, 183, 184, 186, 187; M1/M8/M9/M10

### WUI-NET-13: tool_result frames are intercepted server-side and never reach the dispatcher
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket; the agent has called a browser-executed tool (e.g. `artifacts`) so the server emitted `tool_execution_start` and is awaiting a reply
- **Steps:**
  1. Let the browser run the tool locally and send back a `tool_result` frame keyed by `toolCallId`.
  2. Observe the inbound event stream after the reply.
- **Assertions:**
  - A1: The outbound frame is `{ type: "tool_result", toolCallId, toolName, content, isError, details? }`.
  - A2: The server's inbound task recognizes the `type: "tool_result"` discriminator and routes the frame to the browser-tool hub instead of the JSONL dispatcher.
  - A3: The suspended tool closure resolves with the frame's concatenated text content, and the agent loop resumes (subsequent `tool_execution_end` / `message_update` / `turn_end` frames arrive).
  - A4: No `response` frame with `command: "tool_result"` is ever produced (the dispatcher does not model `tool_result` as a command).
- **Traces:** row 191; M5/M10

### WUI-NET-14: A malformed tool_result frame is dropped without stalling the agent
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket
- **Steps:**
  1. Send a frame with `type: "tool_result"` but a body that fails structural deserialization (e.g. missing `toolCallId` or non-array `content`).
  2. Continue interacting with the session.
- **Assertions:**
  - A1: The malformed frame is dropped server-side (logged as a warning) and is NOT forwarded to the dispatcher.
  - A2: The socket stays open and the session remains usable for subsequent commands.
  - A3: A correctly formed `tool_result` for the same pending `toolCallId` sent afterward still resolves the suspended tool call.
- **Traces:** row 191; M5/M10

### WUI-NET-15: A late or duplicate tool_result for an unknown id is a harmless no-op
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A connected socket; either no tool call is pending, or one already resolved
- **Steps:**
  1. Send a well-formed `tool_result` whose `toolCallId` has no pending execution (already resolved, or never existed).
- **Assertions:**
  - A1: The server accepts and silently discards the frame (resolve on an unregistered id is a no-op).
  - A2: No panic, no socket close, and no duplicate effect on the agent loop.
  - A3: Subsequent legitimate tool calls and prompts continue to work.
- **Traces:** row 191; M5/M10

### WUI-NET-16: POST /upload (multipart) returns a content-addressed id and size
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running; a small binary file is available
- **Steps:**
  1. `curl -F "file=@sample.bin" http://<host>/upload`.
  2. Inspect the JSON response.
- **Assertions:**
  - A1: The response status is 200 with a JSON body `{ "id": <hex>, "size": <bytes> }`.
  - A2: `id` is the lowercase hex SHA-256 of the uploaded bytes (content-addressed), and `size` equals the byte length of the file.
  - A3: Uploading byte-identical content again returns the same `id` (uploads coalesce); uploading different content returns a different `id`.
  - A4: The original filename and content-type travel as multipart metadata and are retained for later download.
- **Traces:** row 164; M10

### WUI-NET-17: POST /upload accepts a raw body with optional headers
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running
- **Steps:**
  1. `curl --data-binary @sample.bin -H "content-type: application/pdf" -H "x-file-name: report.pdf" http://<host>/upload`.
- **Assertions:**
  - A1: A non-multipart content type is treated as a raw-body upload and stored, returning `{ id, size }`.
  - A2: The `x-file-name` header is used as the stored filename and the `content-type` header as the stored MIME type.
  - A3: An empty raw body is rejected with HTTP 400 and an "empty upload" message.
- **Traces:** row 164; M10

### WUI-NET-18: Upload over the 50MB cap is rejected
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running; a file larger than 50MB is available
- **Steps:**
  1. `curl -F "file=@big.bin" http://<host>/upload` with `big.bin` > 50MB.
- **Assertions:**
  - A1: The request is rejected with HTTP 413 Payload Too Large.
  - A2: The response body indicates the upload is too large; no blob is stored.
  - A3: The cap applies on both the multipart field path and the raw-body path (the router body limit and the handler check both guard 50MB).
- **Traces:** row 164; M10

### WUI-NET-19: An empty multipart upload with no file field is rejected
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running
- **Steps:**
  1. POST a multipart body that contains no non-empty file field.
- **Assertions:**
  - A1: The request is rejected with HTTP 400.
  - A2: The response body reads "no file field in upload".
  - A3: A multipart field that is present but zero-length is skipped rather than stored.
- **Traces:** row 164; M10

### WUI-NET-20: GET /download/:id streams the bytes with Content-Disposition
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A blob has been uploaded and its `id` is known
- **Steps:**
  1. `curl -i http://<host>/download/<id>` and inspect headers + body.
- **Assertions:**
  - A1: The response status is 200 and the body bytes are byte-identical to the originally uploaded content.
  - A2: A `Content-Disposition: attachment; filename="<name>"` header is present, with the stored filename (any embedded quotes stripped).
  - A3: The `Content-Type` header reflects the stored/derived MIME type.
- **Traces:** rows 164, 165; M10

### WUI-NET-21: GET /download for an unknown id returns 404
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running
- **Steps:**
  1. `curl -i http://<host>/download/does-not-exist`.
- **Assertions:**
  - A1: The response status is 404.
  - A2: The body reads "unknown download id".
  - A3: A registered id whose backing file has since gone missing returns 404 with "download contents missing".
- **Traces:** row 165; M10

### WUI-NET-22: POST /download/register serves only files under the session cwd
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running with a known session cwd; a real file exists inside that cwd (e.g. an export output)
- **Steps:**
  1. `curl -X POST -H "content-type: application/json" -d '{"path":"export.html"}' http://<host>/download/register`.
  2. Fetch the returned id via `GET /download/:id`.
- **Assertions:**
  - A1: Registering a path that resolves to an existing file inside the cwd returns 200 with `{ "id": "dl-..." }`.
  - A2: A subsequent `GET /download/:id` for that id returns 200 with the file's exact bytes.
  - A3: Each registration of a (different) file yields a fresh, unique, hard-to-guess id.
- **Traces:** rows 165, 168; M10

### WUI-NET-23: Path traversal / out-of-cwd registration is rejected with 4xx
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running with a session cwd
- **Steps:**
  1. `curl -i -X POST -H "content-type: application/json" -d '{"path":"../../../../etc/hosts"}' http://<host>/download/register`.
  2. `curl -i -X POST -H "content-type: application/json" -d '{"path":"/etc/passwd"}' http://<host>/download/register`.
  3. `curl -i -X POST -H "content-type: application/json" -d '{"path":"missing-in-cwd.txt"}' http://<host>/download/register`.
- **Assertions:**
  - A1: A path that canonicalizes outside the session cwd is rejected with HTTP 403 ("path is outside the session directory").
  - A2: A path (including `..` segments or an absolute path) pointing at a non-existent target is rejected with HTTP 404 ("file not found") — never registered.
  - A3: A path that resolves inside the cwd but is a directory, not a file, is rejected with HTTP 404 ("not a file").
  - A4: No download id is ever returned for any rejected path, so an arbitrary file can never be served.
- **Traces:** row 165; M10

### WUI-NET-24: Export → download flow saves the exported HTML in the browser
- **Persona:** End user exporting a conversation
- **Preconditions:** A session with at least one turn; a connected socket
- **Steps:**
  1. Trigger "export" for the session.
  2. Observe the WebSocket, the HTTP calls, and the browser download.
- **Assertions:**
  - A1: An `export_html` request frame is sent and a correlated `response` returns `{ path }` (the server-side written file path).
  - A2: The client POSTs that path to `/download/register` and receives an id, then GETs `/download/:id`.
  - A3: The browser saves a file whose name derives from the returned path (e.g. `export.html`) and whose bytes match the server-written export.
  - A4: The save happens via a fetched blob + temporary anchor; the temporary object URL is revoked afterward.
- **Traces:** rows 168, 187; M10

### WUI-NET-25: Hybrid dispatch — small images inline in the prompt frame
- **Persona:** End user attaching an image to a message
- **Preconditions:** A connected socket; an image attachment (within the editor's 20MB cap) is staged in the editor
- **Steps:**
  1. Attach an image and send a message.
  2. Inspect the outbound `prompt` frame and the Network panel.
- **Assertions:**
  - A1: The `prompt` frame includes an `images` array, each entry shaped `{ data: <base64>, mime_type: <type> }` (snake_case, matching the server image shape).
  - A2: No `POST /upload` request is made for the image (it is delivered inline in the frame).
  - A3: The image bytes are not present in any `attachments` reference array.
- **Traces:** rows 167, 180; M10

### WUI-NET-26: Hybrid dispatch — non-image files uploaded by reference
- **Persona:** End user attaching a non-image binary (e.g. a zip) to a message
- **Preconditions:** A connected socket; a non-image, non-document binary attachment is staged
- **Steps:**
  1. Attach the binary and send a message.
  2. Inspect the HTTP calls and the outbound `prompt` frame.
- **Assertions:**
  - A1: A `POST /upload` request is made for the file before the prompt frame is sent.
  - A2: The `prompt` frame carries an `attachments` array with a reference `{ id, fileName, mimeType, size }` (the upload's returned id and size), not the raw bytes.
  - A3: The binary's base64 content is absent from the prompt frame, keeping the WebSocket frame small.
- **Traces:** rows 167, 164; M10

### WUI-NET-27: Hybrid dispatch — document extracted text appended to the message
- **Persona:** End user attaching a document whose text was extracted client-side
- **Preconditions:** A connected socket; a document attachment with non-empty `extractedText` is staged
- **Steps:**
  1. Attach the document and send a message with some typed text.
  2. Inspect the outbound `prompt` frame's `message` field.
- **Assertions:**
  - A1: The `message` field equals the typed text followed by an appended block `\n\n[Document: <fileName>]\n<extractedText>` for the document.
  - A2: The extracted text reaches the agent regardless of whether the binary was inlined or uploaded (delivery is independent of the upload path).
  - A3: If the document's binary upload fails, the message still carries the appended extracted text (the failure is logged, not surfaced as a broken send).
- **Traces:** row 167; M10

### WUI-NET-28: Document-fetch proxy is applied only to extract_document, only when enabled
- **Persona:** End user who configured a CORS proxy for remote document extraction
- **Preconditions:** The document-fetch proxy is configurable in settings (`proxy.enabled`, `proxy.url`); the agent calls the `extract_document` tool with a remote URL
- **Steps:**
  1. With the proxy disabled, let `extract_document` fetch a URL.
  2. Enable the proxy with a URL and let `extract_document` fetch the same target.
- **Assertions:**
  - A1: With the proxy disabled (or no proxy URL set), the browser fetches the original target URL directly.
  - A2: With the proxy enabled and a URL set, the fetch goes to `<proxy-url>/?url=<encoded-target>` (trailing slash on the proxy URL normalized away).
  - A3: The proxy only affects the client-side `extract_document` fetch; it does NOT alter server-side LLM streaming or any other request.
  - A4: A genuine CORS failure surfaces the neutral fallback message instructing the user about cross-origin restrictions, rather than a raw network error.
- **Traces:** row 169; M10

### WUI-NET-29: /healthz returns ok
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** The server is running
- **Steps:**
  1. `curl -i http://<host>/healthz`.
- **Assertions:**
  - A1: The response status is 200.
  - A2: The body is exactly `ok`.
  - A3: The endpoint responds without requiring a WebSocket upgrade or any session.
- **Traces:** rows 4, 170; M0

### WUI-NET-30: Session events flow over the channel and drive the UI
- **Persona:** End user observing live session feedback
- **Preconditions:** A connected socket; a turn that exercises tools, compaction, an error, and a rename
- **Steps:**
  1. Run a turn that triggers tool execution, then a compaction, then provoke a recoverable error, then rename the session.
  2. Observe the inbound event frames and the UI.
- **Assertions:**
  - A1: Agent-loop events arrive in order: `agent_start` → (`turn_start`, `message_start`, `message_update`…, tool `tool_execution_start`/`update`/`end`, `turn_end`) → `agent_end`, and the UI streams/finalizes accordingly.
  - A2: A `compaction_start` / `compaction_end` (with `summary`) pair is delivered and the compaction status is reflected in the UI.
  - A3: An `error` event (`{ kind: "error", message }`) surfaces to the user without closing the socket.
  - A4: A `session_info_changed` event (`{ kind: "session_info_changed", name }`) updates the session title.
- **Traces:** rows 190, 191, 192; M1/M2/M9

### WUI-NET-31: Uploaded blob is re-downloadable by its content id
- **Persona:** Integration probe observing protocol behavior
- **Preconditions:** A file was uploaded via `POST /upload` and its `id` recorded
- **Steps:**
  1. `curl -i http://<host>/download/<upload-id>` using the id returned by `/upload`.
- **Assertions:**
  - A1: Uploaded blobs and registered server files share one id namespace, so the upload id is directly fetchable via `GET /download/:id`.
  - A2: The download returns the original bytes with `Content-Disposition: attachment` and the upload's stored filename.
  - A3: A filename containing path separators is sanitized to its basename in the `Content-Disposition` header (no traversal in the suggested name).
- **Traces:** rows 164, 165; M10

### WUI-NET-32: Restored-session seeding replays transcript over the wire
- **Persona:** End user reopening a saved conversation
- **Preconditions:** A connected socket; a persisted session is loaded from local storage
- **Steps:**
  1. Restore a saved session containing user, assistant (with tool-call blocks), and tool-result messages.
  2. Inspect the outbound frames emitted by the restore.
- **Assertions:**
  - A1: A `set_model` frame is sent to apply the restored model server-side.
  - A2: A `set_messages` frame is sent carrying only model-native roles (user / assistant / toolResult); UI-only roles are filtered out.
  - A3: Assistant tool-call blocks are de-normalized from the canonical `toolCall` discriminator back to the server's `toolcall` before sending, so they deserialize server-side.
  - A4: A following prompt on the same socket carries the restored history as context (the server-side session was seeded).
- **Traces:** rows 181, 182, 193; M7/M9
</content>
</invoke>
