# Sandbox Runtime — Web UI Acceptance Test Cases

Scope: end-user and integration-observable behavior of the browser sandbox runtime that underpins HTML artifacts and the JavaScript REPL — the transient `execute()` iframe, the persistent `loadContent()` iframe, `prepareHtmlDocument()` standalone assembly, `srcdoc` vs `sandboxUrlProvider` delivery, the navigation interceptor, the HTML validation gate, the `RUNTIME_MESSAGE_ROUTER` dispatcher, the injectable message bridge, and the four runtime providers (console, artifacts, attachments, file-download) in both online and offline modes. Cases are framed against user-observable surfaces (the JS REPL tool, live HTML artifacts, downloaded standalone HTML) wherever possible. Source under test: `crates/web-ui/web/src/sandbox/*`.

---

### WUI-SBX-01: REPL run captures a console line and the returned value
- **Persona:** User running a one-line JS REPL snippet that exercises the transient sandbox.
- **Preconditions:** A `<sandbox-iframe>` element is attached to the document (custom element registered via a runtime/side-effect import of `sandboxed-iframe.ts`).
- **Steps:**
  1. Call `execute("smoke-1", "console.log('x'); return 1+1;")`.
  2. Await the returned `Promise<SandboxResult>`.
- **Assertions:**
  - A1: The promise resolves (does not reject).
  - A2: `result.consoleLogs` contains exactly one entry with `type: "log"` and `text: "x"`.
  - A3: `result.returnValue === 2`.
  - A4: `result.error` is `undefined`.
  - A5: After resolution the transient iframe has been removed from the light DOM (the element holds no child `<iframe>`).
- **Traces:** matrix row 56; arch §7 (JavaScript REPL); M3 acceptance (`console.log('x'); return 1+1;` → log "x", returnValue 2); `sandbox-smoke.ts`.

### WUI-SBX-02: Transient execute() iframe is hidden, persistent loadContent() iframe is visible
- **Persona:** User who runs a REPL snippet (no visual output expected) vs. a user previewing a live HTML artifact.
- **Preconditions:** Two `<sandbox-iframe>` elements available.
- **Steps:**
  1. On the first element call `execute("vis-1", "return 1;")` and inspect the created iframe before completion.
  2. On the second element call `loadContent("vis-2", "<html><body><h1>Hi</h1></body></html>")` and inspect the created iframe.
- **Assertions:**
  - A1: The `execute()` iframe (srcdoc path) carries `display: none` in its inline style.
  - A2: The `loadContent()` iframe is visible (no `display: none`; `width: 100%`, `height: 100%`, `border: none`).
  - A3: The `loadContent()` iframe persists in the DOM after the call returns (caller owns its lifecycle), whereas the `execute()` iframe is removed once the result resolves.
- **Traces:** matrix rows 56, 57; M3 (transient hidden vs persistent visible).

### WUI-SBX-03: Sandbox iframe carries exactly `allow-scripts allow-modals`
- **Persona:** Security-conscious user / integration auditor inspecting the rendered iframe of any artifact or REPL run.
- **Preconditions:** A `<sandbox-iframe>` element attached.
- **Steps:**
  1. Trigger an iframe via `execute("attr-1", "return 1;")` (srcdoc mode) and capture the iframe's `sandbox` token list.
  2. Repeat via `loadContent("attr-2", "<html><body>x</body></html>")`.
- **Assertions:**
  - A1: For both iframes, `iframe.sandbox.contains("allow-scripts")` is true.
  - A2: `iframe.sandbox.contains("allow-modals")` is true.
  - A3: The token list contains no other tokens — specifically NOT `allow-same-origin`, `allow-top-navigation`, `allow-popups`, `allow-forms`, or `allow-popups-to-escape-sandbox` (`iframe.sandbox.length === 2`).
- **Traces:** matrix row 69; arch §7 (sandbox attribute exactly `allow-scripts allow-modals`); M3.

### WUI-SBX-04: Execution timeout resolves with a 120s timeout error, not a hang
- **Persona:** User whose REPL snippet never signals completion (e.g. an infinite `await new Promise(() => {})`).
- **Preconditions:** A `<sandbox-iframe>` attached; `SANDBOX_EXECUTE_TIMEOUT_MS === 120000` is the configured constant.
- **Steps:**
  1. Call `execute("to-1", "await new Promise(() => {});")` (code that never resolves and never calls `complete`).
  2. Allow the timeout to elapse (with fake timers, advance 120000 ms).
- **Assertions:**
  - A1: The exported constant `SANDBOX_EXECUTE_TIMEOUT_MS` equals `120000`.
  - A2: After the timeout the promise RESOLVES (it does not reject) with `result.error.message === "Execution timeout (120s)"` and `result.error.stack === ""`.
  - A3: `result.consoleLogs` is still returned (captured logs up to the timeout are preserved) and `result.files` is present (possibly empty).
  - A4: The transient iframe is removed and the sandbox is unregistered from the router after timeout.
- **Traces:** matrix row 56; arch §7 (hard 120s timeout); M3 (120s timeout constant preserved).

### WUI-SBX-05: A pre-aborted signal rejects execution immediately
- **Persona:** User who hits the stop button before the REPL run even starts (the agent passes an already-aborted signal).
- **Preconditions:** A `<sandbox-iframe>` attached; an `AbortController` whose signal is already aborted.
- **Steps:**
  1. `const c = new AbortController(); c.abort();`
  2. Call `execute("ab-1", "return 1;", [], [], c.signal)`.
- **Assertions:**
  - A1: The call REJECTS synchronously with an `Error` whose message is `"Execution aborted"`.
  - A2: No iframe is created or appended for this run.
- **Traces:** matrix row 56; M3 (AbortSignal).

### WUI-SBX-06: Aborting mid-run rejects and tears down the sandbox
- **Persona:** User pressing stop while a long-running REPL snippet is still executing.
- **Preconditions:** A `<sandbox-iframe>` attached; a fresh `AbortController`.
- **Steps:**
  1. Call `execute("ab-2", "await new Promise(() => {});", [], [], controller.signal)` (long-running, never completes).
  2. After the iframe is created, call `controller.abort()`.
- **Assertions:**
  - A1: The promise REJECTS with an `Error` message `"Execution aborted"`.
  - A2: On abort the iframe is removed, the timeout is cleared, the abort listener is removed, and the sandbox is unregistered from `RUNTIME_MESSAGE_ROUTER`.
  - A3: If a real `execution-complete`/`execution-error` arrives after abort, it is ignored (the `completed` guard prevents a second settle).
- **Traces:** matrix row 56; M3 (AbortSignal cancellation).

### WUI-SBX-07: Live HTML artifact renders via srcdoc and runs its own console
- **Persona:** User viewing a generated HTML artifact in the artifacts panel.
- **Preconditions:** A `<sandbox-iframe>` attached; no `sandboxUrlProvider` configured (default srcdoc delivery).
- **Steps:**
  1. Call `loadContent("html-1", "<html><head></head><body><script>console.log('hello from artifact');</script></body></html>")`.
- **Assertions:**
  - A1: A visible `<iframe>` is appended with its `srcdoc` set to the assembled document (runtime injected after `<head>`).
  - A2: The injected runtime forwards the artifact's `console.log` to the host via `sendRuntimeMessage`; the registered `ConsoleRuntimeProvider` records a log entry `{ type: "log", text: "hello from artifact" }`.
  - A3: The iframe's `srcdoc` contains the injected `<script>` runtime and the navigation interceptor (non-standalone).
- **Traces:** matrix rows 57, 59; M3 (loadContent persistent iframe).

### WUI-SBX-08: Browser-extension CSP delivery uses sandboxUrlProvider URL + postMessage handshake
- **Persona:** User running the UI inside a browser extension whose strict CSP forbids `srcdoc` inline execution.
- **Preconditions:** A `<sandbox-iframe>` with `sandboxUrlProvider` set to a function returning a packaged host URL.
- **Steps:**
  1. Call `loadContent("ext-1", "<html><body>x</body></html>")`.
  2. Simulate the host page posting `{ type: "sandbox-ready" }` from the iframe's `contentWindow`.
- **Assertions:**
  - A1: The iframe's `src` equals the URL returned by `sandboxUrlProvider()` and its `srcdoc` is NOT set.
  - A2: The iframe still carries exactly `allow-scripts allow-modals`.
  - A3: Only after receiving `sandbox-ready` (from the matching `contentWindow`) does the host post `{ type: "sandbox-load", sandboxId: "ext-1", code: <completeHtml> }` into the iframe.
  - A4: A `sandbox-error` message from the iframe is converted into an `execution-error` host message carrying `{ message, stack }`.
- **Traces:** matrix row 59; arch §7 (browser-extension CSP via packaged URL); M3.

### WUI-SBX-09: Navigation interceptor opens external links in a new tab instead of navigating the iframe
- **Persona:** User clicking an `<a href="https://example.com">` link inside a live HTML artifact.
- **Preconditions:** An artifact loaded via `loadContent` (srcdoc, non-standalone) containing an external anchor; `window.open` observable.
- **Steps:**
  1. The injected interceptor handles a click on an external `http(s)` link inside the iframe.
  2. The iframe posts `{ type: "open-external-url", url: "https://example.com/" }` to its parent.
- **Assertions:**
  - A1: The host's `open-external-url` handler (gated on `e.source === iframe.contentWindow`) calls `window.open("https://example.com/", "_blank")`.
  - A2: The iframe document does not navigate (the interceptor calls `preventDefault` + `stopPropagation` on the click).
  - A3: A relative/non-`http(s)` link is NOT intercepted (no `open-external-url` posted).
- **Traces:** matrix row 60; arch §7; M3 (navigation interceptor).

### WUI-SBX-10: Form submission inside an artifact opens externally and never navigates the iframe
- **Persona:** User submitting a `<form action="https://api.example.com/x">` inside a live HTML artifact.
- **Preconditions:** Artifact loaded with a form whose `action` is an absolute URL.
- **Steps:**
  1. The interceptor's capture-phase `submit` handler fires on form submission.
- **Assertions:**
  - A1: `preventDefault` + `stopPropagation` are invoked; the iframe does not navigate.
  - A2: The iframe posts `{ type: "open-external-url", url: "https://api.example.com/x" }` to the parent, which calls `window.open(..., "_blank")`.
- **Traces:** matrix row 60; M3 (forms open externally).

### WUI-SBX-11: Programmatic `window.location` assignment is redirected to an external open
- **Persona:** User running an artifact whose script attempts `window.location = "https://evil.example"` to break out.
- **Preconditions:** Artifact loaded via `loadContent` (non-standalone, interceptor active).
- **Steps:**
  1. Artifact script assigns a new value to `window.location`.
- **Assertions:**
  - A1: The interceptor's `location` setter posts `{ type: "open-external-url", url: "https://evil.example" }` instead of navigating the frame.
  - A2: The getter still returns the original location object (the frame's own location is unchanged).
- **Traces:** matrix row 60; M3 (navigation interceptor).

### WUI-SBX-12: Malformed HTML triggers the validation gate and shows an error page (loadContent)
- **Persona:** User whose generated HTML artifact contains an unparseable document.
- **Preconditions:** A `<sandbox-iframe>` attached; content that makes `DOMParser` emit a `parsererror` node.
- **Steps:**
  1. Call `loadContent("val-1", <html-that-produces-a-parsererror>)`.
- **Assertions:**
  - A1: No artifact iframe is created from the original content; instead an error iframe is rendered whose `srcdoc` contains the heading "HTML Validation Error".
  - A2: The error page shows the `parsererror` text content inside a `<pre>`.
  - A3: `console.error` is invoked with a "HTML validation failed" message.
- **Traces:** matrix row 61; M3 (DOMParser parse error → error page).

### WUI-SBX-13: Malformed HTML in execute() rejects with a validation error
- **Persona:** User running the REPL/artifact path where invalid HTML must fail fast rather than run.
- **Preconditions:** A `<sandbox-iframe>` attached; `isHtmlArtifact: true` content that yields a `parsererror`.
- **Steps:**
  1. Call `execute("val-2", <invalid-html>, [], [], undefined, /* isHtmlArtifact */ true)`.
- **Assertions:**
  - A1: The promise REJECTS with an `Error` whose message starts with `"HTML validation failed:"`.
  - A2: The sandbox is cleaned up (unregistered, no lingering iframe) before the rejection.
- **Traces:** matrix row 61; M3 (validation gate).

### WUI-SBX-14: Valid HTML passes the gate (well-formed REPL wrapper never spuriously fails)
- **Persona:** User running ordinary REPL code that is wrapped into a valid HTML document.
- **Preconditions:** A `<sandbox-iframe>` attached.
- **Steps:**
  1. Call `execute("val-3", "return 42;")` (plain JS, wrapped into a full HTML doc by `prepareHtmlDocument`).
- **Assertions:**
  - A1: `validateHtml` returns null for the generated document (no `parsererror`), so the run proceeds.
  - A2: The promise resolves with `returnValue === 42` and no `error`.
- **Traces:** matrix row 61; M3.

### WUI-SBX-15: prepareHtmlDocument() assembles a standalone HTML document with no bridge/interceptor
- **Persona:** User downloading a generated HTML artifact as a self-contained file to open offline.
- **Preconditions:** A `<sandbox-iframe>` instance (method is public on the element).
- **Steps:**
  1. Call `prepareHtmlDocument("dl-1", "<html><head></head><body>x</body></html>", providers, { isHtmlArtifact: true, isStandalone: true })`.
- **Assertions:**
  - A1: The returned string is a complete HTML document containing the artifact body.
  - A2: It contains NO message-bridge code (`sendRuntimeMessage` definition is absent) and NO navigation interceptor (no `open-external-url` postMessage block).
  - A3: It still contains each provider's stringified runtime function and any injected `window.<key>` data (so offline globals work).
- **Traces:** matrix row 58; M3 (prepareHtmlDocument standalone assembly).

### WUI-SBX-16: REPL wrapper assembly injects runtime into `<head>` and wraps user code in an async IIFE
- **Persona:** User whose REPL code uses `await` and returns a value, expecting both to work.
- **Preconditions:** A `<sandbox-iframe>` instance.
- **Steps:**
  1. Call `prepareHtmlDocument("repl-1", "const v = await Promise.resolve(7); return v;", providers)` (defaults: not HTML artifact).
- **Assertions:**
  - A1: The output is a full `<!DOCTYPE html>` document with the runtime placed inside `<head>`.
  - A2: The user code body appears inside an `async () => { ... }` function whose return value is passed to `window.complete(null, returnValue)`.
  - A3: For HTML-artifact input containing a `<head>`, the runtime is injected immediately after the `<head ...>` open tag; with `<html>` but no `<head>`, after `<html ...>`; with neither, prepended before the content.
- **Traces:** matrix row 58; M3 (prepareHtmlDocument).

### WUI-SBX-17: `</script>` in user code cannot break out of the injected script tag
- **Persona:** User whose REPL snippet or injected data legitimately contains the literal `</script>`.
- **Preconditions:** A `<sandbox-iframe>` instance.
- **Steps:**
  1. Call `prepareHtmlDocument("esc-1", "console.log('</script><img src=x onerror=alert(1)>'); return 1;")`.
- **Assertions:**
  - A1: The assembled document contains no raw `</script>` originating from user code inside the wrapper script — occurrences are escaped to `<\/script`.
  - A2: Injected provider `window.<key>` JSON data is likewise escaped so an embedded `</script` cannot prematurely close the runtime `<script>`.
- **Traces:** matrix row 58; M3 (escapeScriptContent); arch §7 (preserve injection constraints).

### WUI-SBX-18: Router dispatches provider handlers before consumers and ignores foreign messages
- **Persona:** Integration consumer relying on ordered runtime-message dispatch (request/response then lifecycle broadcast).
- **Preconditions:** A sandbox registered via `RUNTIME_MESSAGE_ROUTER.registerSandbox(id, providers, consumers)`.
- **Steps:**
  1. Post a `message` event with `{ sandboxId: id, type: "console", method: "log", text: "hi", messageId: "m1" }` from the registered iframe.
  2. Post a `message` event with no `sandboxId` (foreign message).
- **Assertions:**
  - A1: Every provider's `handleMessage` runs (in registration order) before any consumer's `handleMessage` for the same message.
  - A2: The console provider's response (`{ success: true }`) is posted back as a `runtime-response` carrying the original `messageId` and `sandboxId`.
  - A3: A message with no `sandboxId`, or with an unregistered `sandboxId`, is dropped (no provider/consumer invoked).
- **Traces:** matrix row 62; M3 (RUNTIME_MESSAGE_ROUTER dispatch).

### WUI-SBX-19: Router installs the global listener lazily and removes it when the last sandbox unregisters
- **Persona:** Long-lived app session where an idle UI should hold no global `message` listener.
- **Preconditions:** No sandboxes registered initially.
- **Steps:**
  1. `registerSandbox("r1", [], [])` then `registerSandbox("r2", [], [])`.
  2. `unregisterSandbox("r1")`, then `unregisterSandbox("r2")`.
- **Assertions:**
  - A1: A single global `window` `message` listener is added on the first registration (not duplicated on the second).
  - A2: The listener is removed only once the sandbox map becomes empty (after unregistering the last sandbox).
  - A3: `addConsumer`/`removeConsumer` and `setSandboxIframe` are no-ops for an unknown `sandboxId` (no throw).
- **Traces:** matrix row 62; M3 (register/set/add/remove/unregister).

### WUI-SBX-20: Injected message bridge round-trips a request and times out after 30s
- **Persona:** User whose artifact/REPL code calls a runtime global (e.g. `listArtifacts()`) that round-trips to the host.
- **Preconditions:** Bridge code generated via `RuntimeMessageBridge.generateBridgeCode({ context: "sandbox-iframe", sandboxId })` and injected.
- **Steps:**
  1. Inside the iframe, call `window.sendRuntimeMessage({ type: "artifact-operation", action: "list" })`.
  2. The host posts a matching `runtime-response` with `{ success: true, result: [...] }`.
- **Assertions:**
  - A1: The posted message includes the generated `messageId`, the `sandboxId`, and the original payload fields.
  - A2: The returned promise resolves with the response when `success` is true; it rejects with `new Error(error)` when `success` is false.
  - A3: If no response arrives, the promise rejects with `"Runtime message timeout"` after 30s, and the per-call `message` listener is removed.
  - A4: `window.onCompleted(cb)` registers a completion callback into `window.__completionCallbacks`.
- **Traces:** matrix row 63; M3 (RuntimeMessageBridge generateBridgeCode).

### WUI-SBX-21: ConsoleRuntimeProvider is required and injected first; all four console levels are captured
- **Persona:** User whose REPL/artifact emits `log`, `info`, `warn`, and `error` lines, expecting each to surface with the right severity.
- **Preconditions:** A `<sandbox-iframe>` attached; additional providers may be passed.
- **Steps:**
  1. Call `execute("con-1", "console.log('L'); console.info('I'); console.warn('W'); console.error('E'); return 0;", [otherProvider])`.
- **Assertions:**
  - A1: A `ConsoleRuntimeProvider` is prepended ahead of any caller-supplied providers (its runtime overrides `console.*` before others run).
  - A2: `result.consoleLogs` contains four entries mapping to types `log`, `info`, `warn`, `error` with texts `L`, `I`, `W`, `E` (in emission order).
  - A3: Object arguments are serialized via `JSON.stringify` for `text`; the raw values are preserved under `args`.
- **Traces:** matrix row 64; M3 (ConsoleRuntimeProvider required-first).

### WUI-SBX-22: Thrown errors and unhandled rejections in user code resolve as execution errors
- **Persona:** User whose REPL snippet throws (e.g. `throw new Error("boom")`).
- **Preconditions:** A `<sandbox-iframe>` attached.
- **Steps:**
  1. Call `execute("err-1", "throw new Error('boom');")`.
  2. Separately call `execute("err-2", "Promise.reject(new Error('async boom')); await new Promise(r => setTimeout(r, 10));")`.
- **Assertions:**
  - A1: For the thrown error the promise RESOLVES with `result.error.message === "boom"` and a non-empty `result.error.stack`; `result.returnValue` is absent.
  - A2: Console logs captured before the throw are still present in `result.consoleLogs`.
  - A3: An unhandled rejection is captured by the provider's `unhandledrejection` handler and surfaced as `result.error` with the rejection message.
- **Traces:** matrix row 64; M3 (ConsoleRuntimeProvider error handlers, complete() lifecycle).

### WUI-SBX-23: Artifacts globals work online (round-trip) — list/get/createOrUpdate/delete
- **Persona:** User in the live REPL reading and writing artifacts via `listArtifacts`/`getArtifact`/`createOrUpdateArtifact`/`deleteArtifact`.
- **Preconditions:** A `<sandbox-iframe>` attached; an `ArtifactsRuntimeProvider` constructed with a live `ArtifactsHost` (and `readWrite: true`); the host bridge present.
- **Steps:**
  1. Run REPL code: `await createOrUpdateArtifact('a.json', {n: 1}); const ls = await listArtifacts(); const got = await getArtifact('a.json'); await deleteArtifact('a.json'); return {ls, got};`.
- **Assertions:**
  - A1: `createOrUpdateArtifact` round-trips an `artifact-operation`/`createOrUpdate` request; a non-string value is JSON-stringified (pretty, 2-space) before send.
  - A2: `listArtifacts()` returns the host's filename array; `getArtifact('a.json')` auto-parses the JSON content back to the object `{ n: 1 }`.
  - A3: `deleteArtifact('a.json')` round-trips an `artifact-operation`/`delete` request and resolves void.
  - A4: A failed host response (`success: false`) causes the corresponding global to throw with the host-provided error message.
- **Traces:** matrix row 65; M3/M4 (ArtifactsRuntimeProvider online).

### WUI-SBX-24: Artifacts globals are read-only offline (downloaded standalone HTML)
- **Persona:** User who opens a downloaded standalone HTML artifact offline (no host bridge).
- **Preconditions:** A standalone document assembled with an `ArtifactsRuntimeProvider` snapshot in `window.artifacts`; `sendRuntimeMessage` absent.
- **Steps:**
  1. Offline code: `const names = await listArtifacts(); const data = await getArtifact('data.json');`.
  2. Offline code attempting a write: `await createOrUpdateArtifact('x.txt', 'y');`.
- **Assertions:**
  - A1: `listArtifacts()` returns the keys of the injected `window.artifacts` snapshot.
  - A2: `getArtifact('data.json')` reads from the snapshot and auto-parses JSON; reading an absent file throws `Artifact not found (offline mode): <name>`.
  - A3: `createOrUpdateArtifact(...)` and `deleteArtifact(...)` throw an offline read-only error (e.g. "Cannot create/update artifacts in offline mode (read-only)").
- **Traces:** matrix row 65; M3 (ArtifactsRuntimeProvider offline, readWrite).

### WUI-SBX-25: Read-only artifacts provider description omits write functions
- **Persona:** Model/user reading the tool description to know which artifact operations are available in an HTML artifact (read-only) vs the REPL (read-write).
- **Preconditions:** Two `ArtifactsRuntimeProvider` instances — one `readWrite: true`, one `readWrite: false`.
- **Steps:**
  1. Call `getDescription()` on each.
- **Assertions:**
  - A1: The `readWrite: true` description documents `listArtifacts`, `getArtifact`, `createOrUpdateArtifact`, and `deleteArtifact`.
  - A2: The `readWrite: false` description documents only `listArtifacts` and `getArtifact` and explicitly states modifying/creating is not available.
- **Traces:** matrix row 65; M3/M4 (readWrite description split).

### WUI-SBX-26: Attachments globals list/readText/readBinary work identically online and offline
- **Persona:** User whose REPL/artifact processes a file the user uploaded to the conversation.
- **Preconditions:** A `<sandbox-iframe>` attached; an `AttachmentsRuntimeProvider` constructed with one text attachment (base64 content, optional `extractedText`) and one binary attachment.
- **Steps:**
  1. Run: `const files = listAttachments(); const txt = readTextAttachment(files[0].id); const bin = readBinaryAttachment(files[1].id); return {files, txtLen: txt.length, binLen: bin.length};`.
- **Assertions:**
  - A1: `listAttachments()` returns objects with exactly `{ id, fileName, mimeType, size }` (raw `content`/`extractedText` are NOT exposed by the list).
  - A2: `readTextAttachment(id)` returns `extractedText` when present, otherwise the base64-decoded (`atob`) content.
  - A3: `readBinaryAttachment(id)` returns a `Uint8Array` whose bytes equal the decoded content; an unknown id throws `Attachment not found: <id>`.
  - A4: Because attachments are a `getData()` snapshot, behavior is identical whether or not the host bridge is present (no `sendRuntimeMessage` calls).
- **Traces:** matrix row 66; M3/M6 (AttachmentsRuntimeProvider).

### WUI-SBX-27: returnDownloadableFile collects the file online and triggers a direct download offline
- **Persona:** User whose REPL generates a file (e.g. a CSV) to hand back, vs. a user running the same code in a downloaded standalone HTML.
- **Preconditions (online):** A `<sandbox-iframe>` attached; a `FileDownloadRuntimeProvider` supplied; host bridge present. **Offline:** standalone doc, no bridge; `URL.createObjectURL`/anchor click observable.
- **Steps:**
  1. Online: `execute("dl-on", "await returnDownloadableFile('out.csv', 'a,b\\n1,2'); return 'done';", [fileDownloadProvider])`.
  2. Offline: run the same `returnDownloadableFile` call inside a standalone document.
- **Assertions:**
  - A1 (online): `result.files` contains one `SandboxFile` `{ fileName: "out.csv", mimeType: "text/plain", content: "a,b\n1,2" }`, and `result.returnValue === "done"`.
  - A2 (online): a string defaults to `text/plain`; a non-string/non-binary value is JSON-stringified with `application/json`.
  - A3 (online): a `Blob`/`Uint8Array` with no resolvable MIME type throws a "MIME type is required" error.
  - A4 (offline): no host message is sent; a `Blob` object URL is created, an anchor with `download = fileName` is clicked, and the URL is revoked.
- **Traces:** matrix row 67; M3 (FileDownloadRuntimeProvider online + offline).

### WUI-SBX-28: A returned file larger than 1MB round-trips via chunked base64 without stack overflow
- **Persona:** User whose REPL produces a large binary file (e.g. a multi-megabyte image) to download.
- **Preconditions:** `encodeFileContent` / `encodeDownloadableFile` available; `BASE64_CHUNK_SIZE === 0x8000`.
- **Steps:**
  1. Build a `Uint8Array` of 2,000,000 bytes.
  2. Call `encodeFileContent(bytes)`, then `atob` the result and compare to the original.
- **Assertions:**
  - A1: `BASE64_CHUNK_SIZE` equals `0x8000` (32768) and encoding iterates the buffer in `0x8000`-byte chunks (no `String.fromCharCode(...wholeBuffer)` spread over the full array).
  - A2: Encoding a >1MB buffer does not throw a "Maximum call stack size exceeded" / range error.
  - A3: The returned `{ base64, size }` has `size === bytes.length`, and decoding `base64` reproduces the original bytes exactly.
  - A4: `encodeDownloadableFile` defaults `fileName` to `"file"` and `mimeType` to `"application/octet-stream"` when missing.
- **Traces:** matrix row 67; arch §7 (chunked base64 `0x8000`); M3 (>1MB round-trip without stack overflow).

### WUI-SBX-29: Injected provider runtimes are self-contained (no closures/imports) and globals work
- **Persona:** Integration auditor verifying the `getRuntime().toString()` injection constraint that keeps injected globals functional.
- **Preconditions:** Console, Artifacts, Attachments, and FileDownload providers available.
- **Steps:**
  1. For each provider, stringify `getRuntime().toString()` and inspect the body.
  2. Run `execute(...)` with all providers and confirm the corresponding globals exist inside the iframe.
- **Assertions:**
  - A1: Each runtime function body references only `window`, its own parameters, and locally-declared identifiers — no references to outer/captured module variables and no `import` statements (so `.toString()` serialization stays faithful).
  - A2: After injection, the expected globals are defined inside the iframe: `console` overridden + `window.complete`, and `window.listArtifacts`/`getArtifact`/`createOrUpdateArtifact`/`deleteArtifact`, `window.listAttachments`/`readTextAttachment`/`readBinaryAttachment`, `window.returnDownloadableFile`.
  - A3: Provider data is reachable as `window.<key>` (e.g. `window.attachments`, `window.artifacts`) injected ahead of the runtime functions.
- **Traces:** matrix row 68; arch §7 (preserve `.toString()` injection constraint — no closures/imports); M3.

### WUI-SBX-30: Multiple providers' window data and runtimes compose in a single run
- **Persona:** User running a REPL snippet that simultaneously reads an attachment, queries artifacts, and returns a file.
- **Preconditions:** A `<sandbox-iframe>` attached; console + attachments + artifacts + file-download providers all passed to `execute`.
- **Steps:**
  1. Call `execute("multi-1", "const a = listAttachments(); const arts = await listArtifacts(); await returnDownloadableFile('r.txt','ok'); console.log('multi'); return a.length;", [attachmentsProvider, artifactsProvider, fileDownloadProvider])`.
- **Assertions:**
  - A1: All providers' `getData()` is merged into `window` (e.g. both `window.attachments` and `window.artifacts` present), and all runtime functions are injected.
  - A2: The run resolves with `returnValue` equal to the attachment count, `result.files` contains `r.txt`, and `result.consoleLogs` contains `"multi"`.
  - A3: `ConsoleRuntimeProvider` is still first in the effective provider list even though it was not passed explicitly.
- **Traces:** matrix rows 64-68; M3 (provider composition, console required-first).

### WUI-SBX-31: Re-running execute() with the same id does not accumulate console wrapper layers
- **Persona:** User running the REPL repeatedly in the same session (each run is a fresh transient iframe).
- **Preconditions:** A `<sandbox-iframe>` attached.
- **Steps:**
  1. Call `execute("rerun-1", "console.log('one'); return 1;")` and await.
  2. Call `execute("rerun-1", "console.log('two'); return 2;")` and await.
- **Assertions:**
  - A1: Each run resolves independently with the correct `returnValue` (1, then 2) and its own single console line ("one", then "two").
  - A2: The console override captures the truly-original `console.*` once (guarded by `window.__originalConsole`) so repeated wraps do not nest or duplicate forwarded log entries.
  - A3: After each run the sandbox is unregistered; the second run re-registers cleanly (no stale consumer from the first run leaks the result).
- **Traces:** matrix rows 56, 64; M3 (console override idempotence, lifecycle).
