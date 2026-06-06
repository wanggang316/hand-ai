# Artifacts — Web UI Acceptance Test Cases

Scope: end-user acceptance of the web-ui Artifacts subsystem — the artifacts panel (tab bar, Map-backed tabs, open/switch, imperative insertion, show/collapse + floating pill), the `artifacts` tool commands (create/update/rewrite/get/delete/logs), HTML-create log waiting and HTML re-execution after CRUD, message-history reconstruction on session load, `getFileType` viewer dispatch, every viewer element (HTML, SVG, Markdown, Text, Image, PDF, DOCX, Excel, Generic), the artifact console, the inline artifact pill, and the in-transcript artifacts tool renderer. Cases are written from the user's point of view with concrete, observable assertions. Source: `crates/web-ui/web/src/artifacts/*`, `src/ui/{preview-code-toggle.ts,diff.ts}`; references `docs/web-ui-architecture.md` §7 and `docs/exec-plans/web-ui.md` M4 (matrix rows 70-90).

---

### WUI-ART-01: Empty artifacts panel is hidden until the first artifact exists
- **Persona:** First-time user starting a fresh chat session.
- **Preconditions:** A new session with no artifacts created yet; the artifacts panel element is mounted but `_artifacts` is empty.
- **Steps:**
  1. Open the app and start a new conversation.
  2. Observe the right-hand area before any artifact tool runs.
- **Assertions:**
  - A1: The artifacts panel host renders its outer container with the `hidden` class (no tab bar, no content area visible) because `artifacts.length === 0`.
  - A2: No tab buttons are present in the tab bar.
  - A3: No floating "Artifacts N" pill is shown anywhere on the chat shell while the count is zero.
- **Traces:** artifacts-panel.ts (`render`, `showPanel = artifacts.length > 0 && !this.collapsed`); exec-plan rows 71, 13.

### WUI-ART-02: Creating an artifact reveals the panel with a single active tab
- **Persona:** User whose agent just produced a first artifact.
- **Preconditions:** Empty panel; agent issues `artifacts` create for `notes.md` with content.
- **Steps:**
  1. Send a prompt that causes the agent to call `artifacts` with command `create`, filename `notes.md`.
  2. Wait for the tool call to complete.
- **Assertions:**
  - A1: The panel container loses the `hidden` class and becomes visible.
  - A2: Exactly one tab button appears, labeled `notes.md` in a monospace font.
  - A3: That tab is styled active (primary border + primary text color).
  - A4: The viewer content area shows the `notes.md` artifact element with `display: block`.
  - A5: The tool returns the text `Created file notes.md`.
- **Traces:** artifacts-panel.ts (`createArtifact`, `showArtifact`, `render` tab bar); exec-plan row 73.

### WUI-ART-03: create rejects duplicate filenames
- **Persona:** User whose agent retries a create on an existing file.
- **Preconditions:** `index.html` already exists as an artifact.
- **Steps:**
  1. The agent calls `artifacts` create with filename `index.html` again (new content).
- **Assertions:**
  - A1: The command returns `Error: File index.html already exists`.
  - A2: The existing tab and its content are unchanged; no second `index.html` tab is added.
  - A3: The tab count stays at its prior value.
- **Traces:** artifacts-panel.ts (`createArtifact` duplicate guard).

### WUI-ART-04: create requires both filename and content
- **Persona:** User whose agent emits a malformed create call.
- **Preconditions:** Empty panel.
- **Steps:**
  1. The agent calls `artifacts` create with a filename but empty/absent `content`.
- **Assertions:**
  - A1: The command returns `Error: create command requires filename and content`.
  - A2: No tab is added and the panel stays hidden.
- **Traces:** artifacts-panel.ts (`createArtifact` validation).

### WUI-ART-05: Switching tabs swaps the visible viewer and scrolls the tab into view
- **Persona:** User reviewing multiple generated files.
- **Preconditions:** Three artifacts exist: `a.md`, `b.svg`, `c.html`; `a.md` is active.
- **Steps:**
  1. Click the `c.html` tab.
- **Assertions:**
  - A1: The `c.html` tab becomes active (primary border/text); the `a.md` tab returns to muted/transparent styling.
  - A2: Only the `c.html` viewer element has `display: block`; the other two viewers are `display: none`.
  - A3: The clicked tab button is scrolled into view (smooth, nearest, centered) so it is visible even in an overflowing tab bar.
  - A4: The active artifact's header buttons (toggle/reload/copy/download as applicable) appear at the right of the tab bar.
- **Traces:** artifacts-panel.ts (`showArtifact`, header-button slot in `render`).

### WUI-ART-06: Header buttons in the tab bar belong to the active artifact only
- **Persona:** User who toggled tabs between a text file and an HTML file.
- **Preconditions:** `data.json` (text) and `app.html` (html) both exist.
- **Steps:**
  1. Activate `data.json` and inspect the header action area.
  2. Activate `app.html` and inspect the header action area.
- **Assertions:**
  - A1: For `data.json` the header shows copy + download only (no preview/code toggle, no reload).
  - A2: For `app.html` the header shows the preview/code toggle, a reload button, copy, and download.
  - A3: The Close (X) button is always present at the far right regardless of active type.
- **Traces:** artifacts-panel.ts (`getHeaderButtons()` dispatch), text-artifact.ts, html-artifact.ts.

### WUI-ART-07: Closing the panel collapses it without destroying artifacts; floating pill appears
- **Persona:** User who wants the chat to use full width temporarily.
- **Preconditions:** Two artifacts exist; the panel is open.
- **Steps:**
  1. Click the Close (X) button in the panel header.
- **Assertions:**
  - A1: The `onClose` callback fires; the shell collapses the panel (panel container becomes hidden when `collapsed` is true).
  - A2: A floating "Artifacts N" pill appears showing the count (2) since artifacts still exist while the panel is collapsed.
  - A3: The underlying artifact elements are retained (not removed) so re-opening shows the same tabs and content.
- **Traces:** artifacts-panel.ts (`onClose`, `collapsed`, `disconnectedCallback` keeps elements); exec-plan rows 13, 71.

### WUI-ART-08: Re-opening the collapsed panel restores prior tabs and active selection
- **Persona:** User returning to the artifacts view after collapsing it.
- **Preconditions:** Panel collapsed with two artifacts; `b.svg` was the active tab before collapse.
- **Steps:**
  1. Click the floating "Artifacts 2" pill (or the reopen control) to show the panel.
- **Assertions:**
  - A1: The panel becomes visible again with both tabs present.
  - A2: Re-mounting re-appends the retained artifact elements into the content area, and the previously active artifact (or the first key) is shown with `display: block`.
  - A3: No artifact content is lost or re-fetched.
- **Traces:** artifacts-panel.ts (`connectedCallback` reattach loop).

### WUI-ART-09: openArtifact focuses an existing file and opens the panel
- **Persona:** User clicking an inline reference to jump to a file.
- **Preconditions:** `report.md` and `chart.svg` exist; `report.md` active; panel may be collapsed.
- **Steps:**
  1. Invoke `openArtifact("chart.svg")` (e.g. via an artifact pill click).
- **Assertions:**
  - A1: The `chart.svg` tab becomes active and its viewer is shown.
  - A2: The `onOpen` callback fires so the shell expands the panel if collapsed.
  - A3: Calling `openArtifact` with a non-existent filename is a no-op (no tab change, no `onOpen`).
- **Traces:** artifacts-panel.ts (`openArtifact`), artifact-pill.ts.

### WUI-ART-10: update applies a string replacement and shows the file
- **Persona:** User whose agent edits one line of an existing file.
- **Preconditions:** `style.css` exists containing `color: red;`.
- **Steps:**
  1. The agent calls `artifacts` update on `style.css` with `old_str: "color: red;"`, `new_str: "color: blue;"`.
- **Assertions:**
  - A1: The artifact content now contains `color: blue;` and no longer contains `color: red;`.
  - A2: The command returns `Updated file style.css`.
  - A3: The `style.css` tab is shown/activated after the update.
  - A4: `updatedAt` is refreshed to a newer timestamp than before.
- **Traces:** artifacts-panel.ts (`updateArtifact`).

### WUI-ART-11: update on a missing file or missing match returns a helpful error
- **Persona:** User whose agent targets the wrong file or stale text.
- **Preconditions:** Only `main.js` exists, content `let x = 1;`.
- **Steps:**
  1. The agent calls update on `nope.js` (does not exist).
  2. The agent calls update on `main.js` with `old_str: "let y = 2;"` (not present).
- **Assertions:**
  - A1: Step 1 returns `Error: File nope.js not found. Available files: main.js`.
  - A2: Step 2 returns an error beginning `Error: String not found in file. Here is the full content:` followed by the current `main.js` content (so the agent can self-correct).
  - A3: Step 2 leaves `main.js` content unchanged.
- **Traces:** artifacts-panel.ts (`updateArtifact`, `notFound`).

### WUI-ART-12: update requires old_str and new_str
- **Persona:** User whose agent omits the diff fields.
- **Preconditions:** `a.txt` exists.
- **Steps:**
  1. The agent calls update on `a.txt` with `old_str` present but `new_str` absent.
- **Assertions:**
  - A1: The command returns `Error: update command requires old_str and new_str`.
  - A2: `a.txt` content is unchanged.
- **Traces:** artifacts-panel.ts (`updateArtifact` validation).

### WUI-ART-13: rewrite replaces full content and refreshes the viewer
- **Persona:** User whose agent regenerates a whole file.
- **Preconditions:** `index.html` exists with old markup.
- **Steps:**
  1. The agent calls rewrite on `index.html` with new full `content`.
- **Assertions:**
  - A1: The artifact content equals the new content verbatim.
  - A2: The `index.html` viewer is shown/activated and (being HTML) re-executes in the sandbox.
  - A3: `updatedAt` is refreshed.
  - A4: rewrite on a missing file returns `Error: File <name> not found...`; rewrite with empty content returns `Error: rewrite command requires content`.
- **Traces:** artifacts-panel.ts (`rewriteArtifact`).

### WUI-ART-14: get returns the current content without changing the view
- **Persona:** User whose agent reads a file back.
- **Preconditions:** `config.yaml` exists with known content; `other.md` is the active tab.
- **Steps:**
  1. The agent calls get on `config.yaml`.
- **Assertions:**
  - A1: The command returns the exact current content string of `config.yaml`.
  - A2: The active tab remains `other.md` (get does not switch tabs).
  - A3: get on a missing file returns `Error: File <name> not found...` (with the available-files list when files exist).
- **Traces:** artifacts-panel.ts (`getArtifactContent`, `notFound`).

### WUI-ART-15: delete removes the tab and selects a sibling; deleting the last clears the panel
- **Persona:** User whose agent removes a generated file.
- **Preconditions:** `one.md` and `two.md` exist; `one.md` is active.
- **Steps:**
  1. The agent calls delete on `one.md`.
  2. The agent calls delete on `two.md`.
- **Assertions:**
  - A1: After step 1 the `one.md` tab is gone, its viewer element is removed from the DOM, and `two.md` becomes the active shown tab.
  - A2: The step-1 command returns `Deleted file one.md`.
  - A3: After step 2 no tabs remain, `_activeFilename` is null, and the panel container becomes hidden again.
  - A4: delete on a non-existent file returns `Error: File <name> not found...`.
- **Traces:** artifacts-panel.ts (`deleteArtifact`).

### WUI-ART-16: Deleting a non-active artifact keeps the current selection
- **Persona:** User cleaning up a background file while viewing another.
- **Preconditions:** `a.md`, `b.md`, `c.md` exist; `b.md` active.
- **Steps:**
  1. The agent calls delete on `a.md`.
- **Assertions:**
  - A1: `a.md` tab and viewer are removed.
  - A2: `b.md` remains active and shown (selection unchanged because the deleted file was not active).
  - A3: Remaining tab order is `b.md`, `c.md`.
- **Traces:** artifacts-panel.ts (`deleteArtifact` active-only reselection branch).

### WUI-ART-17: HTML create waits up to ~1500ms and appends captured console logs to the result
- **Persona:** User whose agent creates a self-logging HTML file.
- **Preconditions:** Empty panel; sandbox iframe runtime available.
- **Steps:**
  1. The agent calls create on `demo.html` with content that runs `console.log('hello')` and `console.error('boom')`.
  2. Wait for the tool call to return.
- **Assertions:**
  - A1: The returned text starts with `Created file demo.html` on the first line.
  - A2: Subsequent lines include the captured logs formatted as `[log] hello` and `[error] boom`.
  - A3: The wait is bounded (resolves after at most ~1500ms even if more logs would arrive later).
  - A4: For a non-HTML create (e.g. `demo.md`) no log-wait occurs and the result is just `Created file demo.md`.
- **Traces:** artifacts-panel.ts (`waitForHtmlExecution`, `createArtifact` html branch), html-artifact.ts (`getLogs`); exec-plan row 74.

### WUI-ART-18: CRUD on one artifact re-executes all HTML artifacts (cross-artifact dependency)
- **Persona:** User with one HTML page that reads data from another artifact at runtime.
- **Preconditions:** `data.json` and `viewer.html` exist; `viewer.html` reads `data.json` via the artifacts runtime and logs a value.
- **Steps:**
  1. The agent calls rewrite on `data.json` with a changed value.
- **Assertions:**
  - A1: After the rewrite, `viewer.html`'s sandbox iframe is re-executed (reloadAllHtmlArtifacts runs after the CRUD op).
  - A2: `viewer.html`'s console reflects the new `data.json` value on the next render.
  - A3: Re-execution refreshes the HTML artifact's runtime providers so it sees current artifact state.
- **Traces:** artifacts-panel.ts (`reloadAllHtmlArtifacts`, called from create/update/rewrite/delete), html-artifact.ts (`executeContent`); exec-plan row 74.

### WUI-ART-19: logs returns captured console output only for HTML artifacts
- **Persona:** User debugging an HTML artifact via the agent.
- **Preconditions:** `app.html` (with logs) and `readme.md` exist.
- **Steps:**
  1. The agent calls logs on `app.html`.
  2. The agent calls logs on `readme.md`.
  3. The agent calls logs on a never-created filename.
- **Assertions:**
  - A1: Step 1 returns the HTML artifact's accumulated logs (e.g. `[log] ...` lines), or `No logs for app.html` if none were captured.
  - A2: Step 2 returns `Error: File readme.md is not an HTML file. Logs are only available for HTML files.`
  - A3: Step 3 returns `Error: File <name> not found...`.
- **Traces:** artifacts-panel.ts (`getLogs`), html-artifact.ts (`getLogs`).

### WUI-ART-20: reconstructFromMessages replays artifact history on session load without auto-opening the panel
- **Persona:** Returning user reloading a saved session that previously created artifacts.
- **Preconditions:** A stored message list containing assistant `toolCall`s for `artifacts` plus successful `artifact`/`toolResult` history that created `a.html`, then updated it, then created `b.md`, then deleted a temp file `t.txt`.
- **Steps:**
  1. Load the session; the shell calls `reconstructFromMessages(messages)`.
- **Assertions:**
  - A1: Final state contains exactly `a.html` (with the update applied) and `b.md`; the deleted `t.txt` is absent.
  - A2: Reconstruction runs each create with skipWait + silent, so it does not block on the 1500ms HTML log wait per op and does not fire `onArtifactsChange` per op.
  - A3: The panel is NOT auto-opened by the shell during reconstruction (reconstruct only shows the first artifact internally and fires `onArtifactsChange` once; it does not call `onOpen`).
  - A4: After reconstruction the first artifact key is the internally-shown tab, but the user still sees the panel collapsed if it was collapsed (no forced expand).
- **Traces:** artifacts-panel.ts (`reconstructFromMessages` steps 1-6); exec-plan rows 75, 15, 357.

### WUI-ART-21: Reconstruction folds get/logs/error results out of the replay
- **Persona:** Returning user whose prior session included read-only `get`/`logs` calls and a failed op.
- **Preconditions:** Message history where `artifacts` `get` and `logs` toolResults appear, plus one toolResult marked `isError: true`, alongside genuine create/update operations.
- **Steps:**
  1. Load the session triggering reconstruction.
- **Assertions:**
  - A1: `get` and `logs` toolResults are skipped (they do not mutate reconstructed state).
  - A2: Any toolResult with `isError: true` (or with no matching artifacts toolCall) is skipped.
  - A3: `artifact`-role messages drive create/update(as rewrite)/delete directly; the simulated in-memory pass produces the same final files a live run would.
- **Traces:** artifacts-panel.ts (`reconstructFromMessages` operation collection + simulation).

### WUI-ART-22: getFileType dispatches each extension to the correct viewer
- **Persona:** User generating files of many types.
- **Preconditions:** A panel that instantiates a viewer per file.
- **Steps:**
  1. Create artifacts named `page.html`, `logo.svg`, `doc.md`, `readme.markdown`, `photo.png`, `report.pdf`, `book.xlsx`, `old.xls`, `memo.docx`, `script.ts`, `notes.csv`, and `archive.zip`.
- **Assertions:**
  - A1: `page.html` → `<html-artifact>`; `logo.svg` → `<svg-artifact>`; `doc.md` and `readme.markdown` → `<markdown-artifact>`.
  - A2: `photo.png` → `<image-artifact>`; `report.pdf` → `<pdf-artifact>`; `book.xlsx` and `old.xls` → `<excel-artifact>`; `memo.docx` → `<docx-artifact>`.
  - A3: `script.ts` and `notes.csv` → `<text-artifact>`; `archive.zip` (unknown) → `<generic-artifact>`.
  - A4: A filename with no extension falls through to `generic`.
- **Traces:** file-type.ts (`getFileType`), artifacts-panel.ts (`getOrCreateArtifactElement`); exec-plan row 72.

### WUI-ART-23: HTML artifact renders live in a sandboxed iframe and captures console output
- **Persona:** User previewing an interactive HTML page.
- **Preconditions:** `app.html` whose script calls `console.log('ready')`.
- **Steps:**
  1. Create `app.html` and view it in the panel (preview mode).
- **Assertions:**
  - A1: A `<sandbox-iframe>` renders the page content inside the preview area.
  - A2: A `window.complete()` invocation is injected just before `</html>` (or appended if no closing tag) so the runtime signals readiness without timing out.
  - A3: The captured `console.log('ready')` appears as `[log] ready` in the artifact console pane below the preview.
  - A4: `console.error(...)` output is classified as type `error` (red text).
- **Traces:** html-artifact.ts (`executeContent`, complete() injection, console consumer), console.ts; arch §7 (HTML artifact sandbox), exec-plan row 76.

### WUI-ART-24: HTML artifact preview/code toggle switches views without re-running the page
- **Persona:** User who wants to read the source of a running page.
- **Preconditions:** `app.html` is active in preview mode.
- **Steps:**
  1. Click the "Code" segment of the preview/code toggle in the header.
  2. Click "Preview" to switch back.
- **Assertions:**
  - A1: In code mode the raw HTML is shown in a `<code-block>` with `language="html"`; the sandbox iframe area is hidden (`display: none`) but stays in the DOM.
  - A2: In preview mode the iframe area is shown and the code view is hidden.
  - A3: Toggling views does not reload/re-execute the iframe (both views are always mounted; only `display` changes).
  - A4: The toggle reflects the current mode (active segment highlighted) and emits a `mode-change` event with detail `"preview"`/`"code"`.
- **Traces:** html-artifact.ts (`render`, `setViewMode`), preview-code-toggle.ts; exec-plan row 76.

### WUI-ART-25: HTML artifact reload clears the console and re-executes
- **Persona:** User re-running a page after fixing nothing (just to retrigger side effects).
- **Preconditions:** `app.html` active with prior captured logs visible.
- **Steps:**
  1. Click the reload (refresh) header button.
- **Assertions:**
  - A1: The accumulated logs array is cleared, so the console resets before the new run.
  - A2: `executeContent` runs again against the current content, producing a fresh set of captured logs.
  - A3: The console pane re-appears once new logs arrive (it is only rendered when `logs.length > 0`).
- **Traces:** html-artifact.ts (reload button `onClick`, `executeContent`).

### WUI-ART-26: HTML artifact copy puts the raw HTML on the clipboard
- **Persona:** User copying the page source.
- **Preconditions:** `app.html` active with known content.
- **Steps:**
  1. Click the copy header button.
- **Assertions:**
  - A1: The clipboard receives the artifact's raw `content` (the original HTML, not the runtime-injected variant).
  - A2: The copy button shows its copied/confirmation state momentarily.
- **Traces:** html-artifact.ts (`getHeaderButtons` CopyButton text = `_content`).

### WUI-ART-27: HTML artifact download produces a standalone file with the runtime injected
- **Persona:** User saving an offline-runnable copy of the page.
- **Preconditions:** `app.html` active; the page uses an artifacts/attachments runtime global.
- **Steps:**
  1. Click the download header button.
- **Assertions:**
  - A1: A file named `app.html` is downloaded with MIME `text/html`.
  - A2: The downloaded HTML is produced via `prepareHtmlDocument(..., { isHtmlArtifact: true, isStandalone: true })`, so the runtime is inlined (no bridge / navigation interceptor) and the file works offline.
  - A3: If the sandbox iframe ref is not yet available, the download falls back to the raw `content`.
- **Traces:** html-artifact.ts (`getHeaderButtons` DownloadButton, `prepareHtmlDocument`); exec-plan row 77, arch §7.

### WUI-ART-28: SVG artifact shows a rendered image preview and a code view
- **Persona:** User viewing a generated vector graphic.
- **Preconditions:** `logo.svg` with valid SVG markup.
- **Steps:**
  1. View `logo.svg` in preview mode.
  2. Toggle to code mode.
- **Assertions:**
  - A1: Preview renders an `<img>` whose `src` is a Blob object URL created from the SVG content with type `image/svg+xml`, scaled with `object-contain`.
  - A2: Code mode shows the raw SVG in a `<code-block>` with `language="xml"`.
  - A3: The Blob object URL is revoked when the content changes or the element disconnects (no URL leak).
- **Traces:** svg-artifact.ts (`updatePreviewUrl`, `revokePreviewUrl`, `render`); exec-plan row 78.

### WUI-ART-29: SVG artifact copy/download use SVG content and MIME
- **Persona:** User exporting an SVG.
- **Preconditions:** `logo.svg` active.
- **Steps:**
  1. Click copy, then click download.
- **Assertions:**
  - A1: Copy places the raw SVG markup on the clipboard.
  - A2: Download saves `logo.svg` with MIME `image/svg+xml` and the SVG content.
- **Traces:** svg-artifact.ts (`getHeaderButtons`).

### WUI-ART-30: Markdown artifact renders formatted preview and a code view
- **Persona:** User reviewing a generated report.
- **Preconditions:** `report.md` with headings, a list, and a code fence.
- **Steps:**
  1. View `report.md` in preview mode.
  2. Toggle to code mode.
- **Assertions:**
  - A1: Preview renders via `<markdown-block>` (headings, list, fenced code formatted), not raw text.
  - A2: Code mode shows the raw markdown in a `<code-block>` with `language="markdown"`.
  - A3: Copy places the raw markdown on the clipboard; download saves `report.md` with MIME `text/markdown`.
- **Traces:** markdown-artifact.ts (`render`, `getHeaderButtons`); exec-plan row 79.

### WUI-ART-31: Text artifact syntax-highlights code extensions and uses plain pre for others
- **Persona:** User viewing both a source file and a plain log.
- **Preconditions:** `main.ts` (code) and `notes.txt` (plain text) exist.
- **Steps:**
  1. View `main.ts`.
  2. View `notes.txt`.
- **Assertions:**
  - A1: `main.ts` renders inside a `<code-block>` with `language="typescript"` (extension `ts` maps to typescript).
  - A2: `notes.txt` renders inside a plain monospace `<pre>` with wrapping (`whitespace-pre-wrap break-words`), not a `<code-block>`.
  - A3: A `data.csv` file (text-but-not-code) also renders as plain `<pre>`.
- **Traces:** text-artifact.ts (`isCode`, `getLanguageFromExtension`, `render`); exec-plan row 80.

### WUI-ART-32: Text artifact download uses an extension-derived MIME
- **Persona:** User saving a text/code file.
- **Preconditions:** `main.ts`, `note.md`, and `pic.svg` opened as text artifacts.
- **Steps:**
  1. Download each from its header.
- **Assertions:**
  - A1: `main.ts` downloads with MIME `text/plain`.
  - A2: `note.md` downloads with MIME `text/markdown`; `pic.svg` downloads with MIME `image/svg+xml`.
  - A3: Each download keeps the exact filename and current content; copy puts the raw content on the clipboard.
- **Traces:** text-artifact.ts (`getMimeType`, `getHeaderButtons`).

### WUI-ART-33: Image artifact renders a data-URL image with extension-mapped MIME
- **Persona:** User viewing a generated image.
- **Preconditions:** `photo.jpg` artifact whose content is raw base64 (no data: prefix).
- **Steps:**
  1. View `photo.jpg`.
- **Assertions:**
  - A1: The `<img src>` is `data:image/jpeg;base64,<content>` (jpg/jpeg map to `image/jpeg`).
  - A2: A `gif`/`webp`/`bmp`/`ico` image maps to `image/gif`/`image/webp`/`image/bmp`/`image/x-icon` respectively; an unknown image extension defaults to `image/png`.
  - A3: When content already begins with `data:`, that string is used as the `src` unchanged.
  - A4: The image is scaled with `object-contain` and centered.
- **Traces:** image-artifact.ts (`getMimeType`, `getImageUrl`, `render`); exec-plan row 81.

### WUI-ART-34: Image artifact shows an error placeholder for broken data and downloads decoded bytes
- **Persona:** User who received a corrupt image artifact.
- **Preconditions:** `bad.png` with invalid/undecodable image data.
- **Steps:**
  1. View `bad.png`; the `<img>` fails to load.
  2. Click download.
- **Assertions:**
  - A1: On image load error the `<img src>` is replaced with the inline SVG placeholder reading "Image Error".
  - A2: Download decodes the base64 content to bytes and saves `bad.png` with the extension-mapped MIME.
- **Traces:** image-artifact.ts (`@error` handler, `decodeBase64`, `getHeaderButtons`).

### WUI-ART-35: PDF artifact renders all pages onto canvases and downloads the file
- **Persona:** User viewing a generated multi-page PDF.
- **Preconditions:** `report.pdf` base64 with 3 pages; pdfjs worker configured.
- **Steps:**
  1. View `report.pdf`.
  2. Click download.
- **Assertions:**
  - A1: Three `<canvas>` elements render (one per page) at scale 1.5, each on a white background with rounded border, with a thin separator between pages.
  - A2: The pdfjs worker is loaded from the Vite-emitted worker asset URL (`GlobalWorkerOptions.workerSrc`), i.e. rendering does not require a server round-trip.
  - A3: Download saves `report.pdf` with MIME `application/pdf` from the decoded bytes.
- **Traces:** pdf-artifact.ts (`renderPdf`, worker config, `getHeaderButtons`); exec-plan row 82, arch §7.

### WUI-ART-36: PDF artifact shows an error panel when the document cannot be parsed
- **Persona:** User who received a malformed PDF.
- **Preconditions:** `broken.pdf` with invalid base64/PDF bytes.
- **Steps:**
  1. View `broken.pdf`.
- **Assertions:**
  - A1: No canvases render; instead an error panel titled "Error loading PDF" shows with the failure message.
  - A2: Replacing the content with a valid PDF clears the error (`error` resets to null on content set) and renders pages.
- **Traces:** pdf-artifact.ts (`renderPdf` catch, `render` error branch, `set content`).

### WUI-ART-37: DOCX artifact renders the document and downloads it
- **Persona:** User viewing a generated Word document.
- **Preconditions:** `memo.docx` base64 of a valid DOCX with headings and a table.
- **Steps:**
  1. View `memo.docx`.
  2. Click download.
- **Assertions:**
  - A1: The document renders inline (docx-preview `renderAsync` into the container) with paragraphs/tables visible and the panel's theme-fitting style overrides applied.
  - A2: An invalid DOCX shows the "Error loading document" panel instead.
  - A3: Download saves `memo.docx` with the Word MIME `application/vnd.openxmlformats-officedocument.wordprocessingml.document`.
- **Traces:** docx-artifact.ts (`renderDocx`, error branch, `getHeaderButtons`); exec-plan row 83.

### WUI-ART-38: Excel artifact shows multi-sheet tabs with styled tables and downloads the workbook
- **Persona:** User viewing a generated spreadsheet with several sheets.
- **Preconditions:** `book.xlsx` base64 with sheets `Sales`, `Costs`, `Summary`.
- **Steps:**
  1. View `book.xlsx`.
  2. Click the `Costs` sheet tab.
  3. Click download.
- **Assertions:**
  - A1: A sheet tab bar shows `Sales`, `Costs`, `Summary`; the first sheet (`Sales`) is active and its table is shown initially.
  - A2: Clicking `Costs` activates that tab (primary border/text) and shows only the `Costs` table; the others are `display: none`.
  - A3: Each table renders header cells styled distinctly (bold, muted background, sticky) and zebra-striped even rows.
  - A4: A single-sheet workbook renders just the one table with no tab bar.
  - A5: Download saves `book.xlsx` with the xlsx MIME (`...spreadsheetml.sheet`); an `.xls` file downloads with `application/vnd.ms-excel`.
- **Traces:** excel-artifact.ts (`renderExcel`, `renderExcelSheet`, tab onclick, `getMimeType`); exec-plan row 84.

### WUI-ART-39: Excel artifact shows an error panel for an unreadable workbook
- **Persona:** User who received a corrupt spreadsheet.
- **Preconditions:** `bad.xlsx` with invalid bytes.
- **Steps:**
  1. View `bad.xlsx`.
- **Assertions:**
  - A1: No tables render; an "Error loading spreadsheet" panel shows with the failure message.
- **Traces:** excel-artifact.ts (`renderExcel` catch, `render` error branch).

### WUI-ART-40: Generic artifact shows a placeholder and a download with extension-mapped MIME
- **Persona:** User who received an unsupported file type.
- **Preconditions:** `bundle.zip` base64 artifact (unknown viewer type).
- **Steps:**
  1. View `bundle.zip`.
  2. Click download.
- **Assertions:**
  - A1: The viewer shows a file icon, the filename `bundle.zip`, and the text "Preview not available for this file type." plus a prompt to download.
  - A2: Download decodes the base64 to bytes and saves `bundle.zip` with MIME `application/zip`.
  - A3: Other extensions map appropriately (e.g. `mp4` → `video/mp4`, `mp3` → `audio/mpeg`); an unmapped extension defaults to `application/octet-stream`.
- **Traces:** generic-artifact.ts (`render`, `getMimeType`, `getHeaderButtons`); exec-plan row 85.

### WUI-ART-41: Artifact console is collapsed by default and expands on click
- **Persona:** User glancing at console output under an HTML preview.
- **Preconditions:** `app.html` running with 4 logs, 0 errors.
- **Steps:**
  1. Observe the console summary line.
  2. Click the summary to expand.
- **Assertions:**
  - A1: Collapsed, the console shows a chevron-right and the summary `console (4)` (total log count when there are no errors).
  - A2: After clicking, the chevron becomes chevron-down and the log lines are revealed in a scrollable area.
  - A3: When collapsed, the autoscroll toggle and copy button are hidden; when expanded they appear.
- **Traces:** console.ts (`render`, `expanded`, summary); exec-plan row 86.

### WUI-ART-42: Artifact console summary shows an error count when errors are present
- **Persona:** User checking whether the page errored.
- **Preconditions:** `app.html` with 2 errors and several logs.
- **Steps:**
  1. Observe the console summary.
- **Assertions:**
  - A1: The summary reads `console (2 errors)` (error count, pluralized) rather than the total count.
  - A2: With exactly 1 error the summary reads `console (1 error)` (singular).
  - A3: Error lines render in destructive (red) styling; log lines render muted.
- **Traces:** console.ts (`render` summary errorCount, line classes).

### WUI-ART-43: Artifact console autoscroll keeps the newest line in view and can be locked
- **Persona:** User watching streaming console output.
- **Preconditions:** `app.html` actively emitting logs; console expanded; autoscroll on by default.
- **Steps:**
  1. Let new logs arrive while expanded.
  2. Click the autoscroll toggle to disable it.
  3. Let more logs arrive.
- **Assertions:**
  - A1: With autoscroll enabled the log container scrolls to the bottom on each update (newest line visible); the toggle shows the active (accent) state with the chevrons-down icon and title "Autoscroll enabled".
  - A2: After disabling, the container no longer auto-jumps; the toggle shows the lock icon and title "Autoscroll disabled".
- **Traces:** console.ts (`updated` autoscroll, toggle button).

### WUI-ART-44: Artifact console copy yields all log lines as text
- **Persona:** User pasting console output into a bug report.
- **Preconditions:** Console expanded with mixed log/error lines.
- **Steps:**
  1. Click the console copy button.
- **Assertions:**
  - A1: The clipboard receives every line formatted as `[log] ...` / `[error] ...`, joined by newlines, in order.
- **Traces:** console.ts (`getLogsText`, copy-button).

### WUI-ART-45: Inline artifact pill navigates to the file in the panel
- **Persona:** User reading the chat transcript who wants to jump to a file.
- **Preconditions:** A transcript with an `artifacts` tool card carrying a pill for `chart.svg`; the panel exists and holds `chart.svg`.
- **Steps:**
  1. Click the pill labeled `chart.svg`.
- **Assertions:**
  - A1: The pill shows a file icon and the filename text; with a live panel it has a pointer cursor and hover state.
  - A2: Clicking calls `openArtifact("chart.svg")` (with click default/propagation prevented) so the panel opens and focuses that tab.
  - A3: When no panel ref is provided the pill renders as a static badge (no pointer cursor, no click handler).
- **Traces:** artifact-pill.ts; exec-plan row 87.

### WUI-ART-46: Tool renderer — create/rewrite show a collapsible code block (HTML adds a console block)
- **Persona:** User reading a transcript where the agent created files.
- **Preconditions:** Completed `artifacts` create for `page.html` (with captured logs) and a completed create for `data.json`.
- **Steps:**
  1. Expand the `page.html` create card.
  2. Expand the `data.json` create card.
- **Assertions:**
  - A1: Each card header reads "Created artifact" with an inline pill for the filename and is collapsible.
  - A2: `page.html` body shows a `<code-block>` with `language="html"` plus a `<console-block>` containing the captured logs.
  - A3: `data.json` body shows a `<code-block>` with `language="json"` and no console block (non-HTML).
  - A4: A rewrite card behaves the same but its header reads "Rewrote artifact".
- **Traces:** artifacts-tool-renderer.ts (create/rewrite branch, `getLanguageFromFilename`); exec-plan row 88.

### WUI-ART-47: Tool renderer — update shows a Diff of old_str → new_str
- **Persona:** User reviewing an edit the agent made.
- **Preconditions:** Completed `artifacts` update on `style.css` with `old_str`/`new_str`.
- **Steps:**
  1. Expand the update card.
- **Assertions:**
  - A1: The header reads "Updated artifact" with the `style.css` pill.
  - A2: The body renders a line-level Diff: removed lines show a `-` prefix in red, added lines show a `+` prefix in green, context lines are muted.
  - A3: For an HTML update with captured logs, a `<console-block>` is appended below the diff.
- **Traces:** artifacts-tool-renderer.ts (update branch), diff.ts (`computeLineDiff`, `Diff`); exec-plan row 88.

### WUI-ART-48: Tool renderer — get shows content code block, logs shows a console block, delete shows only a header
- **Persona:** User reading read-only and delete operations in the transcript.
- **Preconditions:** Completed `get` on `config.yaml`, completed `logs` on `app.html`, completed `delete` on `temp.txt`.
- **Steps:**
  1. Expand the get card.
  2. Expand the logs card.
  3. View the delete card.
- **Assertions:**
  - A1: get header "Got artifact" + pill; body is a `<code-block>` of the returned content with `language` from the filename (`config.yaml` → `yaml`); empty output shows "(no output)".
  - A2: logs header "Got logs" + pill; body is a `<console-block>` of the returned logs (or "(no output)").
  - A3: delete header reads "Deleted artifact" + pill and has no expandable body.
- **Traces:** artifacts-tool-renderer.ts (get/logs/delete branches).

### WUI-ART-49: Tool renderer — streaming state shows in-progress headers and partial content
- **Persona:** User watching an artifact tool call execute live.
- **Preconditions:** An `artifacts` create call streaming in (params present, no result yet).
- **Steps:**
  1. Observe the card while the tool call streams.
- **Assertions:**
  - A1: The header shows the streaming label (e.g. "Creating artifact" / "Updating artifact" / "Rewriting artifact" / "Getting artifact" / "Getting logs" / "Deleting artifact") with the filename pill.
  - A2: For a streaming create/rewrite, partial content (if present) renders in a code-block; for a streaming update, a partial diff renders if both `old_str` and `new_str` are present.
  - A3: With no command yet, the card shows "Preparing artifact...".
- **Traces:** artifacts-tool-renderer.ts (params-only / streaming branch, `getCommandLabels`).

### WUI-ART-50: Tool renderer — error state surfaces the failure and keeps the attempted content
- **Persona:** User whose artifact tool call failed.
- **Preconditions:** An `artifacts` create on `bad.html` that returned `isError: true`; and a failed `get` on a missing file.
- **Steps:**
  1. View/expand the failed create card.
  2. View the failed get card.
- **Assertions:**
  - A1: The failed create card shows the attempted content in a code-block; because it is `.html`, the error message renders in a `<console-block variant="error">`.
  - A2: A non-create/update/rewrite failure (e.g. failed get) renders the error text in destructive styling without a code-block.
  - A3: A failed update still renders the attempted old/new diff above the error.
- **Traces:** artifacts-tool-renderer.ts (error-handling branch).

### WUI-ART-51: Imperative viewer insertion shows exactly one viewer; switching never duplicates DOM
- **Persona:** Power user rapidly creating and switching among many artifacts.
- **Preconditions:** Empty panel.
- **Steps:**
  1. Create `a.md`, `b.html`, `c.svg`, `d.png` in sequence, then click through all four tabs twice.
- **Assertions:**
  - A1: Exactly four viewer elements exist in the content area (one per filename); re-selecting a tab does not create a new element (elements are cached in the `artifactElements` Map).
  - A2: At any moment exactly one viewer has `display: block`; the rest are `display: none`.
  - A3: Re-creating an already-existing filename via the same Map key updates the cached element's content rather than appending a duplicate viewer.
- **Traces:** artifacts-panel.ts (`getOrCreateArtifactElement`, `showArtifact`).

### WUI-ART-52: Deleting the currently-open HTML artifact triggers HTML re-execution of survivors and clean teardown
- **Persona:** User removing the active page while another HTML page remains.
- **Preconditions:** `one.html` (active) and `two.html` both exist and run scripts.
- **Steps:**
  1. The agent calls delete on `one.html`.
- **Assertions:**
  - A1: `one.html`'s viewer is removed from the DOM and its sandbox is unregistered from the runtime message router (disconnect teardown).
  - A2: `two.html` becomes the active shown tab.
  - A3: `reloadAllHtmlArtifacts` runs after the delete, re-executing the surviving `two.html` against current providers.
- **Traces:** artifacts-panel.ts (`deleteArtifact`, `reloadAllHtmlArtifacts`), html-artifact.ts (`disconnectedCallback` unregister).

### WUI-ART-53: A broken/invalid HTML artifact still mounts without crashing the panel
- **Persona:** User whose agent produced malformed HTML.
- **Preconditions:** `oops.html` with no `</html>` tag and a script that throws.
- **Steps:**
  1. Create `oops.html` and view it.
- **Assertions:**
  - A1: The `window.complete()` script is appended at the end (since there is no `</html>` to replace), so the runtime still signals readiness.
  - A2: A thrown error in the page surfaces as an `[error] ...` line in the artifact console rather than breaking the panel UI.
  - A3: The tab, preview/code toggle, copy, download, and reload controls remain usable.
- **Traces:** html-artifact.ts (`executeContent` no-`</html>` branch, console consumer error classification).

### WUI-ART-54: Empty/no-content state per viewer renders gracefully
- **Persona:** User who hit an edge where a viewer has empty content.
- **Preconditions:** Artifacts created with empty string content for `empty.svg`, `empty.md`, `empty.txt`.
- **Steps:**
  1. View each empty artifact.
- **Assertions:**
  - A1: `empty.svg` preview shows no `<img>` (no preview URL is created for empty content) and does not throw.
  - A2: `empty.md` preview renders an empty `<markdown-block>`; `empty.txt` renders an empty `<pre>`.
  - A3: Copy/download header buttons remain present for each.
- **Traces:** svg-artifact.ts (`updatePreviewUrl` early-return), markdown-artifact.ts, text-artifact.ts.
