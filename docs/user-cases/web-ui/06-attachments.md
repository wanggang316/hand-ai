# Attachments — Web UI Acceptance Test Cases

Scope: end-to-end, end-user acceptance cases for the Attachments subsystem of the web UI composer — `loadAttachment` ingestion per format (PDF, DOCX, PPTX, Excel/XLS, image, plain text) with chunked base64 for large files; the editor ingestion paths (paperclip file-picker, drag-and-drop with drop overlay, clipboard image paste); the `<attachment-tile>` row (thumbnail/icon, format badge, file name, delete, opens overlay); the limits (max 10 attachments, 20 MB per file, accepted types) and their inline non-blocking validation errors; the full-screen `<attachment-overlay>` (PDF all-pages canvas, DOCX render, Excel multi-sheet tabs, PPTX extracted text, image, plain text, the extracted-text toggle, file download, error state); and read-only attachment tiles inside sent user messages. Assertions are observable in the browser. Sources: `crates/web-ui/web/src/attachments/{attachment-utils.ts,attachment-tile.ts,attachment-overlay.ts}`, `crates/web-ui/web/src/shell/message-editor.ts`, `crates/web-ui/web/src/shell/messages/user-message.ts`; exec-plan M6 + matrix rows 101-124.

### WUI-ATT-01: Attach a PDF via the paperclip file picker
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer is empty with no attachments; a single-page (or multi-page) `.pdf` of ≤ 20 MB is available locally.
- **Steps:**
  1. Click the paperclip ("Attach files") button in the left toolbar.
  2. In the OS file picker choose one `.pdf` file and confirm.
  3. Wait for ingestion to finish.
- **Assertions:**
  - A1: While ingestion runs, the paperclip button is replaced by a spinning loader (animate-spin), and once done the paperclip returns.
  - A2: Exactly one `<attachment-tile>` appears in the attachment row above the textarea.
  - A3: The tile shows a thumbnail image (the rendered PDF first page), not a generic document icon.
  - A4: A "PDF" badge overlays the bottom of the thumbnail.
  - A5: The tile carries a delete (X) affordance in the top-right corner.
- **Traces:** rows 102, 103, 110, 124; M6.

### WUI-ATT-02: PDF thumbnail is generated at the 160×160 fit box
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A `.pdf` whose first page is non-square (e.g. A4 portrait).
- **Steps:**
  1. Attach the PDF via any ingestion path.
  2. Inspect the tile thumbnail image.
- **Assertions:**
  - A1: The thumbnail is a PNG raster of the first page rendered into a box that fits within 160×160 (longest side ≤ 160 px, aspect ratio preserved).
  - A2: The tile renders the thumbnail at a 64×64 (w-16 h-16) object-cover frame regardless of source aspect ratio.
  - A3: The attachment's `preview` is base64 PNG with no `data:` URL prefix (the tile prepends `data:image/png;base64,`).
- **Traces:** rows 103, 110; M6.

### WUI-ATT-03: PDF extracted text is page-tagged XML
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A multi-page `.pdf` with selectable text on each page.
- **Steps:**
  1. Attach the PDF.
  2. Open the overlay (click the tile) and switch to the "Text" view.
- **Assertions:**
  - A1: The extracted text begins with `<pdf filename="…">` using the attachment's file name.
  - A2: Each page is wrapped in `<page number="N">…</page>` with N starting at 1 and incrementing per page.
  - A3: The text closes with `</pdf>`.
  - A4: Per-page text is the page's text runs joined by spaces, with empty runs dropped.
- **Traces:** rows 103, 118; M6.

### WUI-ATT-04: Attach a DOCX — text extraction with tables
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A `.docx` containing at least one paragraph and one table.
- **Steps:**
  1. Attach the `.docx` via the picker.
  2. Open the overlay and switch to the "Text" view.
- **Assertions:**
  - A1: The tile shows a generic document (file-text) icon (no thumbnail) with a truncated file name beneath it.
  - A2: Extracted text begins `<docx filename="…">` and contains a single `<page number="1">` wrapper, closing with `</page></docx>`.
  - A3: Table content is emitted between `[Table]` and `[/Table]` markers, cells joined by ` | ` and rows on separate lines.
  - A4: The attachment mimeType is the DOCX OpenXML type (`…wordprocessingml.document`).
- **Traces:** rows 104, 110, 118; M6.

### WUI-ATT-05: Attach a PPTX — slides and notes extracted
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A `.pptx` with multiple slides; at least one slide carries speaker notes.
- **Steps:**
  1. Attach the `.pptx`.
  2. Open the overlay.
- **Assertions:**
  - A1: Extracted text begins `<pptx filename="…">` and ends `</pptx>`.
  - A2: Each slide is wrapped in `<slide number="N">…</slide>`, numbered from 1 in slide order; slides with no text runs render as an empty `<slide number="N"></slide>`.
  - A3: Speaker notes appear inside a `<notes>…</notes>` block, each line formatted `[Slide N notes]: …`.
  - A4: The overlay body shows the extracted text in a monospace `<pre>` (PPTX has no native slide rendering).
- **Traces:** rows 105, 115; M6.

### WUI-ATT-06: Attach an Excel workbook — one CSV block per sheet
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An `.xlsx` workbook with at least two named sheets, each holding tabular data.
- **Steps:**
  1. Attach the `.xlsx`.
  2. Open the overlay and switch to the "Text" view.
- **Assertions:**
  - A1: Extracted text begins `<excel filename="…">` and ends `</excel>`.
  - A2: Each sheet is wrapped in `<sheet name="…" index="K">` where K is 1-based in workbook order, and the body is the sheet rendered as CSV.
  - A3: The tile shows a spreadsheet (file-spreadsheet) icon rather than the generic document icon.
- **Traces:** rows 106, 110, 114, 118; M6.

### WUI-ATT-07: Legacy .xls is accepted as Excel
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A legacy `.xls` workbook.
- **Steps:**
  1. Attach the `.xls` file.
- **Assertions:**
  - A1: Ingestion succeeds and a tile appears with the spreadsheet icon.
  - A2: The attachment is treated as Excel (extracted text uses the `<excel …>` / `<sheet …>` format).
  - A3: Opening the overlay shows the spreadsheet table view, not a plain-text fallback.
- **Traces:** rows 106, 114; M6.

### WUI-ATT-08: Attach an image — base64 content doubles as preview
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A PNG or JPEG image ≤ 20 MB.
- **Steps:**
  1. Attach the image via the picker.
  2. Inspect the tile.
- **Assertions:**
  - A1: The tile shows the image itself as a 64×64 object-cover thumbnail.
  - A2: No "PDF" badge appears over an image tile.
  - A3: The attachment `type` is `image`, and its `preview` equals its base64 `content`.
  - A4: The tile `<img>` uses the image's own mimeType in the data URL (`data:<mimeType>;base64,…`).
- **Traces:** rows 107, 110, 116; M6.

### WUI-ATT-09: Attach a plain-text/code file via extension allowlist
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A `.md` (or `.json`, `.ts`, `.yaml`, etc.) file whose MIME type is not reported by the OS.
- **Steps:**
  1. Attach the file via the picker.
  2. Open the overlay.
- **Assertions:**
  - A1: Ingestion succeeds because the extension is on the text allowlist (`.txt .md .json .xml .html .css .js .ts .jsx .tsx .yml .yaml`).
  - A2: The attachment's extracted text is the decoded UTF-8 file contents (verbatim, no XML wrapper).
  - A3: When the source MIME is absent, the stored mimeType is normalized to `text/plain`.
  - A4: The overlay renders the text inside a monospace `<pre>`.
- **Traces:** rows 108, 117; M6.

### WUI-ATT-10: Unsupported file type is rejected with a non-blocking error
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A file with an unsupported type/extension (e.g. `.zip` or a binary `.bin` with no recognized MIME).
- **Steps:**
  1. Attempt to attach the unsupported file via the picker.
- **Assertions:**
  - A1: No tile is added to the attachment row.
  - A2: An inline error banner appears in the editor (role="alert") reading "Failed to process <name>: …" — not a blocking `window.alert`.
  - A3: The error banner has a dismiss (✕) control and auto-clears after ~5 seconds.
  - A4: The composer remains interactive (textarea still typable) while the error is shown.
- **Traces:** rows 108, 124; M6.

### WUI-ATT-11: Large file (>1 MB) encodes without a stack overflow
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A supported file several MB in size but ≤ 20 MB (e.g. a 5 MB PDF or image).
- **Steps:**
  1. Attach the large file.
  2. Open the overlay and trigger Download.
- **Assertions:**
  - A1: Ingestion completes with no "Maximum call stack size exceeded" error in the console (base64 is chunked at 0x8000 bytes).
  - A2: A tile appears and the overlay renders the file.
  - A3: Downloading reproduces a file whose byte size matches the original.
- **Traces:** row 109; M6.

### WUI-ATT-12: Drag-and-drop a file shows the drop overlay
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer visible; a supported file available in the OS file manager.
- **Steps:**
  1. Begin dragging a file over the composer area.
  2. Observe the composer while hovering.
  3. Drop the file onto the composer.
- **Assertions:**
  - A1: While dragging over the composer, an overlay with the text "Drop files here" appears and the composer border switches to the active (primary) highlight.
  - A2: On drop, the overlay disappears and the file is ingested into a tile.
  - A3: Dragging out of the composer bounds (without dropping) clears the "Drop files here" overlay.
- **Traces:** row 122; M6.

### WUI-ATT-13: Drag-and-drop multiple files at once
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer empty; two or three supported files selected for drag.
- **Steps:**
  1. Drag the multiple files together onto the composer.
  2. Drop.
- **Assertions:**
  - A1: One tile is added per successfully ingested file, in the order provided.
  - A2: The drop overlay clears after the drop.
  - A3: If one of the dropped files is unsupported, the others still ingest and only the failing one raises an inline error.
- **Traces:** rows 122, 124; M6.

### WUI-ATT-14: Paste an image from the clipboard
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An image is on the system clipboard (e.g. a screenshot); cursor is focused in the textarea.
- **Steps:**
  1. Press the OS paste shortcut in the textarea.
- **Assertions:**
  - A1: The clipboard image is ingested and shown as an image tile.
  - A2: The image is NOT also pasted into the textarea as text/markup (default paste is suppressed).
  - A3: The textarea value is unchanged by the paste.
- **Traces:** row 123; M6.

### WUI-ATT-15: Pasting non-image clipboard content does not create an attachment
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Plain text is on the clipboard; cursor focused in the textarea.
- **Steps:**
  1. Paste into the textarea.
- **Assertions:**
  - A1: No attachment tile is created.
  - A2: The pasted text appears in the textarea normally (default paste not suppressed).
- **Traces:** row 123; M6.

### WUI-ATT-16: Maximum 10 attachments enforced
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer already holds 10 attachments.
- **Steps:**
  1. Attempt to add one more file (picker or drop).
- **Assertions:**
  - A1: The new file is rejected; the attachment row still shows exactly 10 tiles.
  - A2: An inline error reads "Maximum 10 files allowed".
  - A3: The error banner is non-blocking and auto-dismisses after ~5 seconds.
- **Traces:** row 124; M6.

### WUI-ATT-17: Batch that would exceed 10 is rejected wholesale
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer holds 8 attachments.
- **Steps:**
  1. Select a batch of 4 files at once via the picker.
- **Assertions:**
  - A1: None of the 4 are added (8 + 4 > 10 triggers the count guard before ingestion), so the row stays at 8 tiles.
  - A2: The inline error "Maximum 10 files allowed" is shown.
- **Traces:** row 124; M6.

### WUI-ATT-18: Per-file 20 MB limit enforced
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A supported file larger than 20 MB.
- **Steps:**
  1. Attempt to attach the oversized file.
- **Assertions:**
  - A1: No tile is added for the oversized file.
  - A2: An inline error reads "<name> exceeds the maximum size of 20MB".
  - A3: The error is non-blocking and the composer stays interactive.
- **Traces:** row 124; M6.

### WUI-ATT-19: Oversized file in a mixed batch is skipped, others ingest
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A batch of two files: one ≤ 20 MB (valid) and one > 20 MB.
- **Steps:**
  1. Select both files together (count is within the 10 limit).
- **Assertions:**
  - A1: The valid file is ingested into a tile.
  - A2: The oversized file is skipped with the "<name> exceeds the maximum size of 20MB" error.
  - A3: The single (latest) error message is the one displayed.
- **Traces:** row 124; M6.

### WUI-ATT-20: File picker advertises only accepted types
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer visible.
- **Steps:**
  1. Open the picker via the paperclip button and inspect the accepted-types filter.
- **Assertions:**
  - A1: The hidden file input's `accept` includes `image/*`, `application/pdf`, and `.docx,.pptx,.xlsx,.xls,.txt,.md,.json,.xml,.html,.css,.js,.ts,.jsx,.tsx,.yml,.yaml`.
  - A2: The file input has the `multiple` attribute (several files can be chosen at once).
  - A3: The input is visually hidden (display:none) and only opened by clicking the paperclip.
- **Traces:** row 124; M6.

### WUI-ATT-21: Re-selecting the same file re-triggers ingestion
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer empty.
- **Steps:**
  1. Pick file `report.pdf`; let a tile appear.
  2. Delete the tile.
  3. Open the picker and choose the same `report.pdf` again.
- **Assertions:**
  - A1: The second selection ingests successfully (the input value is reset after each selection so the change event re-fires).
  - A2: A fresh tile appears for the re-selected file.
- **Traces:** row 124; M6.

### WUI-ATT-22: Empty selection is a no-op
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer empty.
- **Steps:**
  1. Open the picker and cancel without choosing a file (or drop with no files).
- **Assertions:**
  - A1: No tile is added and no error banner appears.
  - A2: The loader does not spin (ingestion is skipped for an empty list).
- **Traces:** rows 122, 124; M6.

### WUI-ATT-23: Duplicate file can be attached twice (distinct tiles)
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer holds one attachment of `notes.md`.
- **Steps:**
  1. Attach the identical `notes.md` again.
- **Assertions:**
  - A1: Two tiles are present (no dedup); each has a distinct attachment id.
  - A2: Both count toward the 10-file limit independently.
- **Traces:** rows 102, 124; M6.

### WUI-ATT-24: Tile delete button removes the attachment
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer holds three attachments.
- **Steps:**
  1. Click the delete (X) button on the middle tile.
- **Assertions:**
  - A1: Only that tile is removed; the other two remain in order.
  - A2: Clicking the delete button does NOT open the overlay (the click is stopped from bubbling to the tile).
  - A3: The attachment count drops by one (e.g. capacity for a new file is restored if at the limit).
- **Traces:** rows 110, 124; M6.

### WUI-ATT-25: Clicking a tile body opens the full-screen overlay
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer holds at least one attachment.
- **Steps:**
  1. Click the tile thumbnail/icon (not the delete button).
- **Assertions:**
  - A1: A full-screen modal (`<attachment-overlay>`, fixed inset-0, dark backdrop) appears over the page.
  - A2: The overlay header shows the attachment's file name plus a Download button and a Close button.
  - A3: The overlay body renders the appropriate viewer for the file type.
- **Traces:** rows 110, 111; M6.

### WUI-ATT-26: Overlay closes via backdrop, Escape, and the close button
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An attachment overlay is open.
- **Steps:**
  1. Press the Escape key — observe.
  2. Reopen the overlay; click the dark backdrop outside the content — observe.
  3. Reopen the overlay; click the header Close (X) button — observe.
- **Assertions:**
  - A1: Each of the three actions dismisses the overlay (it is removed from the DOM).
  - A2: Clicking inside the header or body region does NOT close the overlay (those regions stop click propagation to the backdrop).
  - A3: After close, the Escape key listener is removed (pressing Escape again has no overlay-related effect).
- **Traces:** row 111; M6.

### WUI-ATT-27: Overlay PDF viewer renders all pages
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A multi-page PDF attachment.
- **Steps:**
  1. Open the overlay for the PDF.
- **Assertions:**
  - A1: Every page is rendered to its own canvas (page count matches the document), stacked vertically and scrollable.
  - A2: Pages render at scale 1.5 on a white canvas background, with a thin separator between consecutive pages.
  - A3: Closing the overlay mid-load destroys the in-flight loading task (no console worker errors after close).
- **Traces:** rows 111, 112; M6.

### WUI-ATT-28: Overlay DOCX viewer renders the document
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A DOCX attachment with formatted content and a table.
- **Steps:**
  1. Open the overlay for the DOCX.
- **Assertions:**
  - A1: The document body is rendered (formatted text, not raw XML) on a white page surface.
  - A2: Tables render with horizontal scroll if wider than the viewport; images are constrained to the container width.
  - A3: The header shows a "Document / Text" toggle.
- **Traces:** rows 111, 113, 118; M6.

### WUI-ATT-29: Overlay Excel viewer shows multi-sheet tabs and styled tables
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An Excel attachment with at least two sheets.
- **Steps:**
  1. Open the overlay for the workbook.
  2. Click a non-active sheet tab.
- **Assertions:**
  - A1: A sticky tab bar lists one tab per sheet name; the first sheet's table is shown initially with its tab highlighted (primary underline).
  - A2: Clicking another tab shows that sheet's table and moves the active highlight; only one sheet's table is visible at a time.
  - A3: Tables render with bordered cells, a styled header row, and zebra striping on even rows.
- **Traces:** rows 111, 114; M6.

### WUI-ATT-30: Single-sheet Excel renders without a tab bar
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An Excel workbook with exactly one sheet.
- **Steps:**
  1. Open the overlay.
- **Assertions:**
  - A1: The single sheet's table is shown.
  - A2: No tab bar is rendered (tabs only appear when sheet count > 1).
- **Traces:** rows 111, 114; M6.

### WUI-ATT-31: Overlay PPTX viewer shows extracted text body
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A PPTX attachment.
- **Steps:**
  1. Open the overlay for the PPTX.
- **Assertions:**
  - A1: The body shows the slide/notes extracted text inside a monospace, wrapping `<pre>` (no native slide canvas rendering).
  - A2: No "format / Text" toggle is shown for PPTX (the body already is the extracted text).
- **Traces:** rows 111, 115; M6.

### WUI-ATT-32: Overlay image viewer shows the full image
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An image attachment.
- **Steps:**
  1. Open the overlay for the image.
- **Assertions:**
  - A1: The image is shown contained within the viewport (object-contain, rounded with shadow), using the image's own mimeType in the data URL.
  - A2: No "format / Text" toggle is shown for images.
- **Traces:** rows 111, 116; M6.

### WUI-ATT-33: Overlay plain-text viewer shows decoded contents
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A `.txt` or code-file attachment.
- **Steps:**
  1. Open the overlay.
- **Assertions:**
  - A1: The decoded file text is shown verbatim in a monospace, wrapping `<pre>`.
  - A2: If there is no content, the placeholder "No content available" is shown.
  - A3: No "format / Text" toggle is shown for plain-text files.
- **Traces:** rows 111, 117; M6.

### WUI-ATT-34: Extracted-text toggle switches PDF/DOCX/Excel between rendered and raw text
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A PDF (or DOCX, or Excel) attachment that has extracted text.
- **Steps:**
  1. Open the overlay (rendered view is active by default).
  2. Click the "Text" segment of the header toggle.
  3. Click the format segment (e.g. "PDF" / "Document" / "Spreadsheet") to switch back.
- **Assertions:**
  - A1: The toggle shows two segments: the format label and "Text"; the active segment is visually highlighted.
  - A2: "Text" switches the body to the raw page/sheet-tagged extracted text in a monospace `<pre>`.
  - A3: The format segment switches back to the rendered viewer (canvas/document/table) and re-renders it.
  - A4: The toggle is present only when extracted text exists and the type is PDF, DOCX, or Excel.
- **Traces:** row 118; M6.

### WUI-ATT-35: Extracted-text placeholder when no text was extracted
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A document with the toggle available but whose extracted text is empty (e.g. a scanned/image-only PDF).
- **Steps:**
  1. Open the overlay and switch to "Text".
- **Assertions:**
  - A1: The `<pre>` shows "No text content available" rather than being blank.
- **Traces:** rows 118, 120; M6.

### WUI-ATT-36: Download the original file from the overlay
- **Persona:** End user attaching files in the composer.
- **Preconditions:** An attachment overlay is open for any file type.
- **Steps:**
  1. Click the Download button in the overlay header.
- **Assertions:**
  - A1: The browser downloads a file named exactly the attachment's file name.
  - A2: The downloaded bytes match the original (base64 content decoded to a Blob of the attachment's mimeType).
  - A3: The overlay stays open after download (download does not dismiss the modal).
- **Traces:** row 119; M6.

### WUI-ATT-37: Overlay error state on a corrupt/unrenderable file
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A PDF (or DOCX/Excel) whose bytes fail to render in the viewer (e.g. truncated/corrupt content that still ingested).
- **Steps:**
  1. Open the overlay for the file.
- **Assertions:**
  - A1: An error panel appears with the heading "Error loading file" and a detail message, styled with the destructive color.
  - A2: The viewer canvas/table is not shown alongside the error.
  - A3: The overlay can still be closed via Escape / backdrop / Close.
- **Traces:** rows 111, 120; M6.

### WUI-ATT-38: Long file names are truncated on icon tiles
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A non-preview file (e.g. `.docx`) whose name is longer than 10 characters, e.g. `quarterly-financials.docx`.
- **Steps:**
  1. Attach the file and inspect the tile.
- **Assertions:**
  - A1: The icon tile shows the first 8 characters of the name followed by "..." (e.g. `quarterl...`).
  - A2: Hovering the tile reveals the full file name via the tooltip (title attribute).
- **Traces:** row 110; M6.

### WUI-ATT-39: Send a message with attachments clears the composer
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer holds one or more attachments; a model is selected.
- **Steps:**
  1. Type optional text.
  2. Press Enter (or click Send).
- **Assertions:**
  - A1: The send callback receives the typed text and the array of collected attachments.
  - A2: After send, the composer's attachment row and textarea are cleared for the next turn.
  - A3: The Send button was enabled because attachments were present even if the text was empty.
- **Traces:** rows 110, 124; M6.

### WUI-ATT-40: Send is enabled by attachments alone (empty text)
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer textarea is empty; exactly one attachment is present.
- **Steps:**
  1. Observe the Send button.
  2. Press Enter.
- **Assertions:**
  - A1: The Send button is enabled (not disabled) because at least one attachment exists.
  - A2: Pressing Enter sends the message (attachment-only message is allowed).
- **Traces:** row 124; M6.

### WUI-ATT-41: Send is blocked while files are still ingesting
- **Persona:** End user attaching files in the composer.
- **Preconditions:** A large file is mid-ingestion (loader spinning).
- **Steps:**
  1. While the loader spins, press Enter and/or click Send.
- **Assertions:**
  - A1: The message is NOT sent while `processingFiles` is true.
  - A2: The Send button is disabled during ingestion.
  - A3: Once ingestion completes, Send becomes available again.
- **Traces:** rows 109, 124; M6.

### WUI-ATT-42: Sent user message renders attachments as read-only tiles
- **Persona:** End user reviewing a sent message in the transcript.
- **Preconditions:** A user message with attachments has been sent and appears in the message list.
- **Steps:**
  1. Locate the user message in the transcript.
  2. Inspect its attachment tiles.
- **Assertions:**
  - A1: The message shows a row of `<attachment-tile>`s beneath the message text.
  - A2: These tiles have NO delete (X) button (read-only; showDelete is off).
  - A3: Clicking a tile still opens the full-screen overlay for that attachment.
- **Traces:** rows 110, 111; M6.

### WUI-ATT-43: Ingest a file from a URL/Blob source
- **Persona:** End user (or in-app flow) loading a remote document.
- **Preconditions:** A reachable URL serving a supported document, or a Blob produced in-app.
- **Steps:**
  1. Trigger ingestion from the URL/Blob source (e.g. an in-app "extract document" path).
- **Assertions:**
  - A1: For a URL, the file name defaults to the last path segment when not overridden, and the mimeType comes from the response `content-type`.
  - A2: A fetch failure surfaces a "Failed to fetch file" error rather than a partial attachment.
  - A3: The resulting attachment carries base64 content and the correct per-format extracted text/preview, identical to a File-sourced ingest.
- **Traces:** rows 102, 103; M6.

### WUI-ATT-44: Multiple distinct formats coexist in one composer
- **Persona:** End user attaching files in the composer.
- **Preconditions:** Composer empty.
- **Steps:**
  1. Attach, in turn: an image, a PDF, a DOCX, an Excel, a PPTX, and a `.md` text file (6 files total, within the 10-file limit).
- **Assertions:**
  - A1: Six tiles appear; the image and PDF show thumbnails (PDF with its badge), and the DOCX/Excel/PPTX/text show their respective icons (spreadsheet icon for Excel, document icon otherwise) with truncated names.
  - A2: Opening each tile launches the correct viewer (image / PDF canvas / DOCX render / Excel tables / PPTX text / plain text).
  - A3: All six remain independently deletable.
- **Traces:** rows 103-110, 116-117; M6.
